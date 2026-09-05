// Claw'dbot native shell: transparent always-on-top pet window, click-through
// hit-test poller, drag/resize commands, position/scale persistence, and the
// phase-2 event pipeline (hooks.rs HTTP server -> state.rs registry/reducer
// -> "pet-state" events into the webview).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cowork;
mod hooks;
mod state;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use tauri::{LogicalSize, Manager, PhysicalPosition, WindowEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// Poison-tolerant locking. A panic while a lock is held must not take the
/// poller or saver thread down with it on their next `unwrap`: a dead poller
/// freezes click-through in whatever state it was last in, silently.
pub(crate) trait PoisonTolerant<T> {
    fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> PoisonTolerant<T> for Mutex<T> {
    fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

const BASE_SIZE: f64 = 160.0; // logical px at scale 1.0
// Extra window size beyond the canvas: WebView2's viewport under-reports the
// window by ~12px on the right/top, clipping canvas-edge art (dream cloud,
// thinking dots). With padding, the clipped strip falls on empty transparent
// space and the full canvas always renders. The frontend reports hit-test
// bounds page-relative, so click-through stays correct.
const PAD: f64 = 24.0;
// Standing headroom above the canvas for the session popover: the window is
// always this much taller than the sprite area, so opening the popover never
// resizes or moves anything — it just renders into invisible space. The
// click-through poller keeps the empty headroom non-interactive.
const POP_HEADROOM: f64 = 360.0;
// The window is never narrower than this. Empirical (user-eyeballed): the
// on-screen composite clips MORE of the right edge than the reported
// viewport suggests, and neither PrintWindow nor GDI captures can see that
// clip — so the popover gets a wide berth instead of a tight calculation.
const POP_MIN_W: f64 = 400.0;
// Standing room BELOW the canvas for the speech bubble (the frontend anchors
// #pet-wrap this far above the window bottom in Tauri; keep in sync with the
// CSS). Sized for the large bubble: 14px text, title + 3 wrapped detail lines.
const BUBBLE_ROOM: f64 = 110.0;

fn window_size(scale: f64) -> LogicalSize<f64> {
    LogicalSize::new(
        (BASE_SIZE * scale + PAD).max(POP_MIN_W),
        BASE_SIZE * scale + PAD + POP_HEADROOM + BUBBLE_ROOM,
    )
}

/// Clickable region reported by the frontend in PHYSICAL pixels,
/// window-relative (the frontend multiplies by devicePixelRatio, which is
/// the exact CSS->physical factor including webview zoom — the monitor
/// scale factor alone is wrong whenever zoom != 1).
#[derive(Default, Clone, Copy)]
struct OpaqueBounds {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Serialize, Deserialize, Clone)]
struct PetConfig {
    /// window position, physical px (what outer_position reports)
    x: Option<i32>,
    y: Option<i32>,
    scale: f64,
    /// Cowork watchers (absent in older configs = defaults, all on)
    #[serde(default)]
    cowork: cowork::CoworkConfig,
}

impl Default for PetConfig {
    fn default() -> Self {
        Self { x: None, y: None, scale: 1.75, cowork: Default::default() }
    }
}

fn config_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Clawdbot").join("config.json"))
}

/// The pre-rename config folder (the app was called Claudebot until
/// 2026-09-05): read once when the new folder has nothing yet, so position
/// and scale survive the rename; the next save writes the new location.
fn legacy_config_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Claudebot").join("config.json"))
}

fn load_config() -> PetConfig {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .or_else(|| legacy_config_path().and_then(|p| std::fs::read_to_string(p).ok()))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(cfg: &PetConfig) {
    if let Some(p) = config_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(cfg) {
            let _ = std::fs::write(p, json);
        }
    }
}

/// An in-flight app-driven drag: the cursor's offset from the window origin
/// (physical px), captured when the grab started. The poller thread moves
/// the window with set_position while this is Some.
#[derive(Clone, Copy)]
struct DragGrab {
    dx: f64,
    dy: f64,
}

struct AppState {
    bounds: Mutex<OpaqueBounds>,
    cfg: Mutex<PetConfig>,
    dirty: AtomicBool,
    drag: Mutex<Option<DragGrab>>,
}

fn left_button_down() -> bool {
    // SAFETY: a plain Win32 state query, no pointers involved
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16) & 0x8000 != 0 }
}

/// Screen position (physical px) of the buddy's center for a window whose
/// top-left is at (x, y): the sprite is CSS-centered horizontally and its
/// wrap sits BUBBLE_ROOM above the window bottom (index.html, body.tauri
/// #pet-wrap). `sf` is the monitor scale factor (1.0 on 100% displays).
fn buddy_center(x: f64, y: f64, pet_scale: f64, sf: f64) -> (f64, f64) {
    let size = window_size(pet_scale);
    (
        x + size.width * sf / 2.0,
        y + (size.height - BUBBLE_ROOM - BASE_SIZE * pet_scale / 2.0) * sf,
    )
}

/// The OS hit region before the webview's first report: the sprite's rest
/// rect plus its motion envelope and the halo, mirroring reportBounds in
/// index.html (pet.js: rest x32 y68 w96 h80 units, envelope l28 r24 u48 d4,
/// wrap 160 units centered and BUBBLE_ROOM above the window bottom). The
/// buddy is clickable from the first frame — and stays so if the frontend
/// ever fails to load — instead of an invisible 160x160 square at the
/// window's top-left.
fn boot_bounds(pet_scale: f64, sf: f64) -> OpaqueBounds {
    const HALO: f64 = 20.0;
    let size = window_size(pet_scale);
    let wrap_l = size.width / 2.0 - 80.0 * pet_scale;
    let wrap_t = size.height - BUBBLE_ROOM - BASE_SIZE * pet_scale;
    OpaqueBounds {
        x: (wrap_l + 4.0 * pet_scale - HALO) * sf,
        y: (wrap_t + 20.0 * pet_scale - HALO) * sf,
        w: (148.0 * pet_scale + 2.0 * HALO) * sf,
        h: (132.0 * pet_scale + 2.0 * HALO) * sf,
    }
}

fn on_some_monitor(monitors: &[tauri::Monitor], px: f64, py: f64) -> bool {
    monitors.iter().any(|m| {
        let (mp, ms) = (m.position(), m.size());
        px >= mp.x as f64
            && px < mp.x as f64 + ms.width as f64
            && py >= mp.y as f64
            && py < mp.y as f64 + ms.height as f64
    })
}

/// Keep the buddy visible while dragging: if the requested window origin
/// would put the buddy's center off every monitor, pull it back onto the
/// nearest one. The invisible window padding may hang off-screen freely.
fn clamp_to_monitors(monitors: &[tauri::Monitor], x: f64, y: f64, pet_scale: f64, sf: f64) -> (f64, f64) {
    let (cx, cy) = buddy_center(x, y, pet_scale, sf);
    if on_some_monitor(monitors, cx, cy) {
        return (x, y);
    }
    let mut best: Option<(f64, f64, f64)> = None; // (dist², clamped cx, clamped cy)
    for m in monitors {
        let (mp, ms) = (m.position(), m.size());
        let (l, t) = (mp.x as f64, mp.y as f64);
        let (r, b) = (l + ms.width as f64 - 1.0, t + ms.height as f64 - 1.0);
        let (qx, qy) = (cx.clamp(l, r.max(l)), cy.clamp(t, b.max(t)));
        let d2 = (qx - cx).powi(2) + (qy - cy).powi(2);
        if best.map_or(true, |(bd, _, _)| d2 < bd) {
            best = Some((d2, qx, qy));
        }
    }
    match best {
        Some((_, qx, qy)) => (x + (qx - cx), y + (qy - cy)),
        None => (x, y),
    }
}

#[tauri::command]
fn set_opaque_bounds(state: tauri::State<AppState>, x: f64, y: f64, w: f64, h: f64) {
    *state.bounds.lock_or_recover() = OpaqueBounds { x, y, w, h };
}

/// Begin an app-driven drag. The OS move loop (`start_dragging`) is
/// deliberately NOT used: tao gives even this undecorated window WS_CAPTION,
/// so Windows applied its title-bar rules to the 360px of invisible popover
/// headroom — on mouse-up it popped the window down so that empty air sat
/// below the screen top, and the buddy landed somewhere other than where it
/// was released. Moving the window ourselves means no clamp, no Snap, no
/// monitor re-evaluation, and no capture theft (JS keeps its pointerup).
///
/// `x`/`y`: the press point in physical screen px (the webview's screenX/Y
/// times devicePixelRatio). Anchoring on the press rather than on the cursor
/// at IPC time means the movement that happened while this call was in
/// flight is not lost: the buddy stays under the grabbed pixel.
#[tauri::command]
fn start_drag(window: tauri::WebviewWindow, state: tauri::State<AppState>, x: f64, y: f64) {
    // a flick can release the button before this IPC lands; a grab started
    // with the button up would glue the window to the cursor
    if !left_button_down() {
        return;
    }
    let Ok(pos) = window.outer_position() else {
        return;
    };
    *state.drag.lock_or_recover() = Some(DragGrab {
        dx: x - pos.x as f64,
        dy: y - pos.y as f64,
    });
}

#[tauri::command]
fn end_drag(state: tauri::State<AppState>) {
    *state.drag.lock_or_recover() = None;
}

#[tauri::command]
fn set_pet_scale(window: tauri::WebviewWindow, state: tauri::State<AppState>, scale: f64) {
    let s = scale.clamp(0.5, 3.0);
    // The key that got us here arrived through the pet, so it is the
    // foreground window. Test exactly that (not tao's is_focused flag, which
    // reads false while the WebView2 child holds keyboard focus): a future
    // caller running while another app is active must not touch focus at
    // all — focusing a child of an inactive window would activate it.
    let foreground = window
        .hwnd()
        .map(|h| h == unsafe { GetForegroundWindow() })
        .unwrap_or(false);
    let _ = window.set_size(window_size(s));
    // resizing can drop WebView2's keyboard focus; re-focus the WEBVIEW
    // (ICoreWebView2Controller::MoveFocus) so consecutive +/- presses keep
    // working. Window::set_focus cannot do this: tao returns early when the
    // window is already foreground, and it never reaches the child anyway.
    if foreground {
        let _ = AsRef::<tauri::Webview>::as_ref(&window).set_focus();
    }
    state.cfg.lock_or_recover().scale = s;
    state.dirty.store(true, Ordering::Relaxed);
}

#[tauri::command]
fn get_pet_scale(state: tauri::State<AppState>) -> f64 {
    state.cfg.lock_or_recover().scale
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

fn launch_claude_url(url: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Open a fresh Claude Code session in the desktop app, via the deep link
/// the app itself registers (found in its bundle: claude://code/new).
#[tauri::command]
fn open_new_code_session() {
    launch_claude_url("claude://code/new?source=desktop_action");
}

/// Open any claude:// deep link (jump-to-session etc.). Strictly validated:
/// the app's own protocol only, no shell metacharacters — the url ends up as
/// an argument to `cmd start`.
#[tauri::command]
fn open_claude_link(url: String) {
    let safe = url.starts_with("claude://")
        && url
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/:?=&-_.%".contains(c));
    if !safe {
        eprintln!("clawdbot: refused deep link: {url}");
        return;
    }
    launch_claude_url(&url);
}

fn main() {
    let cfg = load_config();

    tauri::Builder::default()
        .manage(AppState {
            bounds: Mutex::new(boot_bounds(cfg.scale.clamp(0.5, 3.0), 1.0)),
            cfg: Mutex::new(cfg),
            dirty: AtomicBool::new(false),
            drag: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![set_opaque_bounds, start_drag, end_drag, set_pet_scale, get_pet_scale, quit_app, open_new_code_session, open_claude_link, state::get_pet_state])
        .setup(move |app| {
            // event pipeline: hook server thread -> mpsc -> state thread.
            // A failed bind never exits and never moves ports (the installed
            // hooks target 4317 statically) — the reducer runs blind instead:
            // a deaf pet that says it's deaf.
            let (tx, rx) = mpsc::channel::<state::PetEvent>();
            // the published state: shared between the `get_pet_state`
            // command and the hook server's `GET /state` test door
            let store = Arc::new(Mutex::new(state::PetStatePayload::initial(true)));
            let cowork_cfg = app.state::<AppState>().cfg.lock_or_recover().cowork.clone();
            let server_ok = match hooks::spawn_server(tx.clone(), store.clone(), cowork_cfg.debug_injection) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("clawdbot: cannot bind {}: {e} — running blind", hooks::HOOK_ADDR);
                    false
                }
            };
            if !server_ok {
                *store.lock_or_recover() = state::PetStatePayload::initial(false);
            }
            app.manage(state::PetStateStore(store));
            // second producer on the same channel: the Cowork watchers
            cowork::spawn(tx.clone(), &cowork_cfg);
            state::spawn_state_thread(rx, tx, app.handle().clone(), server_ok);

            let win = app.get_webview_window("pet").expect("pet window missing");
            let state = app.state::<AppState>();

            // WebView2 keeps its browser accelerators (F5/Ctrl+R reload,
            // Ctrl+F find bar, Ctrl+P print) enabled by default; on a
            // transparent pet those replay the boot wave or surface browser
            // chrome. Tauri 2.11 has no config switch, so flip it on the COM
            // settings object once the webview exists. Best effort: any
            // failure just leaves the default behaviour.
            let _ = win.with_webview(|webview| {
                use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
                use windows::core::Interface;
                // SAFETY: COM calls on the controller Tauri owns, made on the
                // webview's own thread by with_webview
                unsafe {
                    if let Ok(core) = webview.controller().CoreWebView2() {
                        if let Ok(settings) = core.Settings() {
                            if let Ok(s3) = settings.cast::<ICoreWebView2Settings3>() {
                                let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
                            }
                        }
                    }
                }
            });
            let saved = state.cfg.lock_or_recover().clone();

            // Restore scale, then position (clamped: only if the saved point
            // still lands on a live monitor), then show — no flash-then-jump.
            let s = saved.scale.clamp(0.5, 3.0);
            let sf = win.scale_factor().unwrap_or(1.0);
            let _ = win.set_size(window_size(s));
            // clickable from the first frame: the sprite rect stands in for
            // the OS hit region until the webview reports the real one
            *state.bounds.lock_or_recover() = boot_bounds(s, sf);
            if let (Some(x), Some(y)) = (saved.x, saved.y) {
                // the BUDDY (not the padded window) must land on some monitor
                let (cx, cy) = buddy_center(x as f64, y as f64, s, sf);
                let on_screen = win
                    .available_monitors()
                    .map(|monitors| on_some_monitor(&monitors, cx, cy))
                    .unwrap_or(false);
                if on_screen {
                    let _ = win.set_position(PhysicalPosition::new(x, y));
                }
            }
            let _ = win.show();

            // Persist position/scale, debounced: events mark dirty, a saver
            // thread writes at most once a second.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                let state = handle.state::<AppState>();
                if state.dirty.swap(false, Ordering::Relaxed) {
                    let cfg = state.cfg.lock_or_recover().clone();
                    save_config(&cfg);
                }
            });

            // Click-through hit test: Tauri's set_ignore_cursor_events is
            // unreliable per-region on Windows (tauri#11461), so poll the
            // cursor at ~60fps and toggle ignore based on the sprite bounds.
            let poller_win = win.clone();
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut ignoring = false;
                let mut hover_sent: Option<bool> = None;
                let mut inside = true; // on an API failure the last verdict stands
                // per-drag cache: monitors and scale factor are round-trips too
                let mut drag_env: Option<(Vec<tauri::Monitor>, f64)> = None;
                let mut last_target: Option<(i32, i32)> = None;
                loop {
                    let state = handle.state::<AppState>();
                    let sample = (|| {
                        let cursor = handle.cursor_position().ok()?; // physical, global
                        let pos = poller_win.outer_position().ok()?; // physical
                        Some((cursor, pos))
                    })();

                    let grab = *state.drag.lock_or_recover();
                    if let Some(g) = grab {
                        // app-driven drag: the window follows the cursor at the
                        // grabbed offset. Click-through is never toggled
                        // mid-drag, and the buddy is kept on a monitor.
                        if !left_button_down() {
                            *state.drag.lock_or_recover() = None; // lost pointerup
                        } else if let Some((cursor, _)) = sample {
                            let (monitors, sf) = drag_env.get_or_insert_with(|| {
                                (
                                    poller_win.available_monitors().unwrap_or_default(),
                                    poller_win.scale_factor().unwrap_or(1.0),
                                )
                            });
                            let pet_scale = state.cfg.lock_or_recover().scale;
                            let (tx, ty) = clamp_to_monitors(monitors, cursor.x - g.dx, cursor.y - g.dy, pet_scale, *sf);
                            let target = (tx.round() as i32, ty.round() as i32);
                            if last_target != Some(target) {
                                last_target = Some(target);
                                let _ = poller_win.set_position(PhysicalPosition::new(target.0, target.1));
                            }
                        }
                        inside = true;
                    } else {
                        drag_env = None;
                        last_target = None;
                        if let Some((cursor, pos)) = sample {
                            // bounds arrive already in physical px — no scaling
                            let b = *state.bounds.lock_or_recover();
                            let (bx, by) = (pos.x as f64 + b.x, pos.y as f64 + b.y);
                            inside = cursor.x >= bx
                                && cursor.x < bx + b.w
                                && cursor.y >= by
                                && cursor.y < by + b.h;
                        }
                    }

                    let should_ignore = !inside;
                    if should_ignore != ignoring {
                        ignoring = should_ignore;
                        let _ = poller_win.set_ignore_cursor_events(ignoring);
                    }
                    // hover ground truth for the frontend: once the window
                    // ignores cursor events, the webview can never see the
                    // mouse leave — this event is how the "+" gets hidden
                    if hover_sent != Some(inside) {
                        hover_sent = Some(inside);
                        use tauri::Emitter;
                        let _ = handle.emit_to("pet", "pet-hover", inside);
                    }
                    std::thread::sleep(Duration::from_millis(16));
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Moved(pos) = event {
                let state = window.state::<AppState>();
                let mut cfg = state.cfg.lock_or_recover();
                cfg.x = Some(pos.x);
                cfg.y = Some(pos.y);
                state.dirty.store(true, Ordering::Relaxed);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running clawdbot");
}
