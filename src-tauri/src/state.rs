// Session registry + reducer: turns the hook event stream into the single
// pet state. Every mapping decision below is grounded in the 676 captured
// payloads under spikes/phase0/captured-events.jsonl (cited as "captured").
//
// Governing rules:
// - worst state wins across sessions: needs_input > error > blind > working
//   > done > idle > sleeping(no sessions)
// - NeedsInput never decays on a timer — it clears only on evidence the user
//   responded (the next real event for that session replaces the state)
// - a deaf pet must say it's deaf (blind), never look peacefully idle

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::cowork::{CoworkEvent, CoworkHealth};
use crate::hooks::HookEvent;
use crate::PoisonTolerant;

/// Everything that can arrive on the state thread's channel: one channel,
/// two producers (hook server, Cowork watchers), one registry. Only `Hook`
/// feeds the deafness watchdog: a Cowork observation says nothing about
/// whether the hook pipe is alive and must never mask its silence.
pub enum PetEvent {
    Hook(HookEvent),
    Cowork(CoworkEvent),
}

/// Which observer owns a session row. Hook rows are Claude Code sessions
/// (CLI or desktop) seen through the hook server; Cowork rows come from the
/// toast store / desktop log and carry no hooks at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Surface {
    Hook,
    /// a cloud Cowork session (`cse_...`), keyed on the lowercased id
    CoworkCloud,
    /// a local-VM Cowork session (`local_<uuid>` with no hook twin)
    CoworkLocal,
}

impl Surface {
    /// the popover's surface pill: `code` rows have no pill, `cowork` rows do
    fn name(self) -> &'static str {
        match self {
            Surface::Hook => "code",
            Surface::CoworkCloud | Surface::CoworkLocal => "cowork",
        }
    }
}

const DETAIL_MAX: usize = 120;
// 10s (was the plan's 20s): the user found 20s of flag-waving too long —
// done is a courtesy signal, needs_input is the state that must linger
const DONE_TO_IDLE: Duration = Duration::from_secs(10);
const ERROR_TO_IDLE: Duration = Duration::from_secs(30);
const ORPHAN_DROP: Duration = Duration::from_secs(10 * 60);
// a legal 10-minute Bash must not get its session reaped mid-run
const ORPHAN_DROP_OPEN_TOOLS: Duration = Duration::from_secs(15 * 60);
// Working splits into two DISPLAY names: "working" while a tool executes,
// "thinking" while Claude generates. The lag keeps short between-tool gaps
// from flapping the clips (working's wind-up intro replays on each entry).
const THINK_LAG: Duration = Duration::from_secs(3);
// A stopped/abandoned generation leaves NO hook trace (interrupting pure
// generation fires nothing — is_interrupt exists only for tool interrupts),
// so a toolless Working session falls back to Idle after this much silence.
// Documented cost: a marathon no-tool generation idles early too; its
// eventual Stop still lands Done correctly.
const THINK_STALE: Duration = Duration::from_secs(3 * 60);
// A session that only ever fired SessionStart (opened view, nothing typed)
// reaps fast — it should not squat in the popover (user request).
const UNENGAGED_DROP: Duration = Duration::from_secs(2 * 60);
// --- Cowork (phase 3a; rules measured in the 2026-09-05 live drill) ---
// Cloud-only turns leave no host line, so log-derived Working is a lower
// bound: this much silence after the last host command means idle.
const COWORK_WORK_STALE: Duration = Duration::from_secs(2 * 60);
// A Cowork ask has no "user came back" event (the toast row is never
// deleted when the user answers), so it is dropped after this long.
const COWORK_ASK_MAX: Duration = Duration::from_secs(12 * 60 * 60);
// Host activity resuming is the only evidence the user answered a Cowork
// ask — but a command logged a second before the toast must not clear it,
// so activity only clears asks older than this.
const COWORK_ASK_SETTLE: Duration = Duration::from_secs(5);
// A local (`session-local_`) toast within this much of a hook session's
// own transition is the same ask, seen twice.
const COWORK_DEDUPE: Duration = Duration::from_secs(10);
// A toast with no session in it attaches to the single cloud session that
// was active this recently.
const COWORK_ATTACH_WINDOW: Duration = Duration::from_secs(60);
// Deaf toast watcher + a cloud session active this recently = prompts
// could be missed right now: that escalates to the global blind state.
const COWORK_BLIND_WINDOW: Duration = Duration::from_secs(10 * 60);
// Answering a Cowork ask inside the app leaves no host-side trace at all
// (measured 2026-09-05: the toast row stays, no log line). The app itself
// treats "my window is visible" as "the user sees the prompt" (it
// suppresses the toast then), so the pet mirrors that rule: Claude's
// window in the foreground for this long while a Cowork ask is pending
// clears the ask. Cowork rows only — hook asks keep their event-driven
// clearing. (User decision, 2026-09-05.)
const COWORK_SEEN_AFTER: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum S {
    NeedsInput,
    Error,
    Working,
    Done,
    Idle,
}

impl S {
    fn rank(self) -> u8 {
        match self {
            S::NeedsInput => 6,
            S::Error => 5,
            // blind is global rank 4, injected by the reducer
            S::Working => 3,
            S::Done => 2,
            S::Idle => 1,
        }
    }
    fn name(self) -> &'static str {
        match self {
            S::NeedsInput => "needs_input",
            S::Error => "error",
            S::Working => "working",
            S::Done => "done",
            S::Idle => "idle",
        }
    }
}

struct Session {
    cwd: String,
    state: S,
    kind: String,
    detail: String,
    since: Instant,        // when the current (state, kind) was entered
    last_event_at: Instant,
    last_tool_at: Instant, // last Pre/PostToolUse — drives thinking hysteresis
    /// (tool_use_id, tool_name) in START order, so the popover can name the
    /// tool that began most recently (a HashMap lost that)
    open_tools: Vec<(String, String)>,
    /// false until any event beyond SessionStart arrives: a "new session"
    /// view the user opened but never used reaps quickly instead of
    /// squatting in the popover for the full orphan window
    engaged: bool,
    surface: Surface,
    /// toast title (Cowork rows): the session's own name, shown instead of
    /// the cwd basename when present
    title: String,
    /// display-case `cse_...` id for the desktop deep link (Cowork cloud rows)
    link_id: String,
    /// the toast row that set the current ask; its deletion (user dismissed
    /// it in Action Center) clears a Cowork NeedsInput
    ask_row: Option<i64>,
    /// last host-side Activity from a cloud session: drives the Cowork
    /// working-to-idle decay (cloud-only turns are invisible)
    last_activity_at: Instant,
}

impl Session {
    fn new(cwd: String, state: S) -> Self {
        let now = Instant::now();
        Session {
            cwd,
            state,
            kind: String::new(),
            detail: String::new(),
            since: now,
            last_event_at: now,
            // far enough in the past that a fresh prompt reads as thinking
            // immediately (generating, no tool yet). checked: on Windows an
            // Instant counts from boot, and `now - D` panics inside the first
            // D seconds after boot (an autostarted pet would kill this thread)
            last_tool_at: now.checked_sub(THINK_LAG).unwrap_or(now),
            open_tools: Vec::new(),
            engaged: false,
            surface: Surface::Hook,
            title: String::new(),
            link_id: String::new(),
            ask_row: None,
            last_activity_at: now,
        }
    }

    /// Same-(state,kind) re-entry preserves `since` so duplicate signals are
    /// true no-ops and ask-age stays honest. A non-empty detail always
    /// refreshes; empty details never erase a richer one.
    fn set(&mut self, state: S, kind: &str, detail: Option<String>) {
        if self.state != state || self.kind != kind {
            self.state = state;
            self.kind = kind.to_string();
            self.since = Instant::now();
        }
        if let Some(d) = detail {
            if !d.is_empty() {
                self.detail = truncate(&d);
            }
        }
        self.touch();
    }

    fn touch(&mut self) {
        self.last_event_at = Instant::now();
    }
}

#[derive(Clone, Serialize, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub cwd: String,
    pub state: String,
    pub kind: String,
    pub detail: String,
    pub since_ms: u64,
    pub busy: bool,
    /// names of currently-open tools (popover fodder; also the ground truth
    /// behind `busy` when debugging stuck-working sessions)
    pub tools: Vec<String>,
    /// `"code"` (hook-tracked Claude Code) or `"cowork"`
    pub surface: &'static str,
    /// session title when the source knows one (toast title); else empty
    pub title: String,
    /// display-case `cse_...` id for the Cowork deep link; empty otherwise
    pub link_id: String,
}

#[derive(Clone, Serialize, PartialEq)]
pub struct PetStatePayload {
    pub state: String,
    pub detail: String,
    pub kind: String,
    pub n_sessions: usize,
    pub blind: bool,
    pub sessions: Vec<SessionInfo>,
    /// Cowork watcher health for the popover header; empty = fine. Kept
    /// separate from `blind`, which stays the hook pipe's verdict.
    pub cowork_health: String,
}

impl PetStatePayload {
    pub fn initial(server_ok: bool) -> Self {
        PetStatePayload {
            state: if server_ok { "sleeping" } else { "blind" }.into(),
            detail: if server_ok { String::new() } else { "port 4317 unavailable".into() },
            kind: String::new(),
            n_sessions: 0,
            blind: !server_ok,
            sessions: Vec::new(),
            cowork_health: String::new(),
        }
    }

}

/// Emit-on-change comparison must ignore the volatile since_ms ages, or the
/// 1s decay tick would re-emit an identical state every second.
fn same_display(a: &PetStatePayload, b: &PetStatePayload) -> bool {
    let key = |p: &PetStatePayload| {
        (
            p.state.clone(),
            p.detail.clone(),
            p.kind.clone(),
            p.n_sessions,
            p.blind,
            p.cowork_health.clone(),
            p.sessions
                .iter()
                .map(|s| {
                    (
                        s.id.clone(),
                        s.state.clone(),
                        s.kind.clone(),
                        s.detail.clone(),
                        s.busy,
                        s.tools.clone(),
                        s.surface,
                        s.title.clone(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    key(a) == key(b)
}

/// Managed store so `get_pet_state` can hand the frontend a snapshot at any
/// time — kills the boot race where the state thread emits before the
/// webview's listener registers. The same Arc is what the hook server
/// serves at `GET /state`.
pub struct PetStateStore(pub Arc<Mutex<PetStatePayload>>);

#[tauri::command]
pub fn get_pet_state(store: tauri::State<PetStateStore>) -> PetStatePayload {
    store.0.lock_or_recover().clone()
}

pub fn spawn_state_thread(
    rx: Receiver<PetEvent>,
    // held (never used) so a failed server bind can't hang up the channel:
    // with no live Sender, recv_timeout returns Disconnected and the loop —
    // including the blind watchdog — would die before its first publish
    keepalive: std::sync::mpsc::Sender<PetEvent>,
    handle: AppHandle,
    server_ok: bool,
) {
    std::thread::spawn(move || {
        let _keepalive = keepalive;
        let mut reg: HashMap<String, Session> = HashMap::new();
        let mut wd = Watchdog::new(server_ok);
        let mut cw = CoworkState::default();
        seed_recent_sessions(&mut reg);
        wd.self_test();
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(PetEvent::Hook(ev)) => {
                    wd.heard(); // ANY parsed event (SelfTest included) proves the pipe
                    apply_event(&mut reg, &ev);
                }
                // deliberately no wd.heard(): Cowork sightings are not hook
                // sightings, and must never hide a dead hook pipe
                Ok(PetEvent::Cowork(ev)) => apply_cowork(&mut reg, &ev, &mut cw),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            decay(&mut reg);
            // the Win32 look-up runs only while a Cowork ask is pending
            let front = cowork_ask_pending(&reg) && claude_app_in_front();
            cowork_seen_tick(&mut reg, &mut cw, front, Instant::now());
            attach_links(&mut reg, &cw);
            if wd.no_claude_processes() {
                // the only escape hatch for a NeedsInput session whose CLI
                // was hard-killed (NeedsInput never timer-drops)
                reg.clear();
            }
            let blind = wd.blind_reason().or_else(|| cw.blind_reason());
            publish(&reg, &handle, blind.as_deref(), &cw.health.summary());
        }
    });
}

// --- stage-2 watchdog: the silent-failure principle made executable. The
// dangerous failure is breaking QUIETLY — the pet must never look peacefully
// idle while it is actually deaf. Idle and blind must never look the same. ---

struct Watchdog {
    server_ok: bool,
    started: Instant,
    last_hook_at: Option<Instant>,
    hooks_installed: bool,
    last_settings_scan: Instant,
    transcript_fresh: bool,
    last_transcript_scan: Instant,
    zero_proc_streak: u8,
    last_proc_scan: Instant,
    procs_absent: bool,
}

const SELFTEST_GRACE: Duration = Duration::from_secs(10);
const DEAF_SILENCE: Duration = Duration::from_secs(5 * 60);
const SETTINGS_CADENCE: Duration = Duration::from_secs(60);
const TRANSCRIPT_CADENCE: Duration = Duration::from_secs(30);
const PROC_CADENCE: Duration = Duration::from_secs(30);
const SEED_WINDOW: Duration = Duration::from_secs(10 * 60);

impl Watchdog {
    fn new(server_ok: bool) -> Self {
        let now = Instant::now();
        Watchdog {
            server_ok,
            started: now,
            last_hook_at: None,
            hooks_installed: hooks_point_here(),
            last_settings_scan: now,
            transcript_fresh: false,
            // scan on first tick (checked_sub: see Session::new)
            last_transcript_scan: now.checked_sub(TRANSCRIPT_CADENCE).unwrap_or(now),
            zero_proc_streak: 0,
            last_proc_scan: now,
            procs_absent: false,
        }
    }

    fn heard(&mut self) {
        let now = Instant::now();
        self.last_hook_at = Some(now);
        // a hook event is proof of a live Claude: forget any stale empty
        // census, or reg.clear() would erase the session this very event just
        // created (for up to 30s after a relaunch) and hide a pending ask.
        // Pushing the next census out also spares a tasklist spawn while
        // events are flowing; it resumes after 30s of silence.
        self.zero_proc_streak = 0;
        self.procs_absent = false;
        self.last_proc_scan = now;
    }

    /// Startup self-test: POST a SelfTest event at our own port through the
    /// real TCP path. Its arrival (via `heard`) proves server→channel→state
    /// every launch; nothing arriving within the grace window means blind.
    fn self_test(&self) {
        if !self.server_ok {
            return;
        }
        use std::io::Write;
        if let Ok(mut s) = std::net::TcpStream::connect(crate::hooks::HOOK_ADDR) {
            let body = r#"{"hook_event_name":"SelfTest"}"#;
            let req = format!(
                "POST /event HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                crate::hooks::HOOK_ADDR,
                body.len(),
                body
            );
            let _ = s.write_all(req.as_bytes());
        }
    }

    /// Two consecutive empty process censuses -> clear the registry.
    fn no_claude_processes(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_proc_scan) >= PROC_CADENCE {
            self.last_proc_scan = now;
            match claude_process_count() {
                Some(0) => self.zero_proc_streak = self.zero_proc_streak.saturating_add(1),
                Some(_) => self.zero_proc_streak = 0,
                None => {} // census failed: keep the previous streak, decide nothing new
            }
            self.procs_absent = self.zero_proc_streak >= 2;
        }
        self.procs_absent
    }

    fn blind_reason(&mut self) -> Option<String> {
        let now = Instant::now();
        if !self.server_ok {
            return Some("port 4317 unavailable".into());
        }
        if now.duration_since(self.last_settings_scan) >= SETTINGS_CADENCE {
            self.last_settings_scan = now;
            self.hooks_installed = hooks_point_here();
        }
        if !self.hooks_installed {
            return Some("hooks not installed".into());
        }
        if self.last_hook_at.is_none() {
            if now.duration_since(self.started) > SELFTEST_GRACE {
                return Some("hook self-test failed".into());
            }
            return None; // still inside the grace window
        }
        // deaf-while-active: silence alone is normal (an idle REPL is quiet
        // for hours) — silence WHILE transcripts advance is deafness
        let silent = now.duration_since(self.last_hook_at.unwrap()) >= DEAF_SILENCE;
        if silent {
            if now.duration_since(self.last_transcript_scan) >= TRANSCRIPT_CADENCE {
                self.last_transcript_scan = now;
                self.transcript_fresh = any_transcript_fresh(DEAF_SILENCE);
            }
            if self.transcript_fresh {
                return Some("transcripts active but no hook events".into());
            }
        }
        None
    }
}

fn claude_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE").map(|h| std::path::PathBuf::from(h).join(".claude"))
}

/// Does ~/.claude/settings.json still route hooks at this pet?
fn hooks_point_here() -> bool {
    claude_home()
        .and_then(|h| std::fs::read_to_string(h.join("settings.json")).ok())
        .map(|s| s.contains(crate::hooks::HOOK_ADDR))
        .unwrap_or(false)
}

fn for_each_transcript(mut f: impl FnMut(&std::path::Path, std::time::SystemTime)) {
    let root = match claude_home() {
        Some(h) => h.join("projects"),
        None => return,
    };
    let dirs = match std::fs::read_dir(root) {
        Ok(d) => d,
        Err(_) => return,
    };
    for dir in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
        for file in files.flatten() {
            let p = file.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(m) = file.metadata() {
                if let Ok(t) = m.modified() {
                    f(&p, t);
                }
            }
        }
    }
}

fn any_transcript_fresh(window: Duration) -> bool {
    let mut fresh = false;
    for_each_transcript(|_, mtime| {
        if mtime.elapsed().map(|e| e < window).unwrap_or(false) {
            fresh = true;
        }
    });
    fresh
}

/// Startup seed: transcripts touched in the last 10 min become Idle sessions
/// (recovered). No state inference from transcript CONTENT — the format is
/// high-churn internal. Accepted gap: a permission prompt already pending at
/// pet launch stays invisible until the user answers it.
fn seed_recent_sessions(reg: &mut HashMap<String, Session>) {
    for_each_transcript(|path, mtime| {
        if !mtime.elapsed().map(|e| e < SEED_WINDOW).unwrap_or(false) {
            return;
        }
        let Some(sid) = path.file_stem().and_then(|s| s.to_str()) else { return };
        let slug = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        reg.entry(sid.to_string()).or_insert_with(|| {
            let mut s = Session::new(slug, S::Idle);
            s.detail = "recovered".into();
            // engaged stays FALSE: unused new-session views leave transcript
            // stubs that seed as ghosts (user caught one). A recovered
            // session that shows no real event within UNENGAGED_DROP reaps;
            // any live session reappears on its next event anyway.
            s
        });
    });
}

/// Count live claude.exe processes with a Toolhelp snapshot: sub-millisecond
/// and in-process, where the old `tasklist` spawn stalled the reducer thread
/// for ~0.4s every census (a needs_input hop arriving mid-census showed late).
fn claude_process_count() -> Option<usize> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    // SAFETY: plain Toolhelp calls over a locally owned entry struct whose
    // dwSize is set; the snapshot handle is closed on every path
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut n = 0;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                // image-name match is exact ("claude.exe" CLI / "Claude.exe"
                // MSIX app) so clawdbot.exe itself never counts as Claude
                if name.eq_ignore_ascii_case("claude.exe") {
                    n += 1;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        Some(n)
    }
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= DETAIL_MAX {
        return s.to_string();
    }
    let cut: String = s.chars().take(DETAIL_MAX - 1).collect();
    format!("{cut}\u{2026}")
}

/// tool_name plus the juiciest human-readable scrap of tool_input.
fn tool_detail(ev: &HookEvent) -> Option<String> {
    let name = ev.tool_name.as_deref().unwrap_or("");
    let scrap = ev.tool_input.as_ref().and_then(|ti| {
        ti.get("description")
            .or_else(|| ti.get("command"))
            .or_else(|| ti.get("file_path"))
            .or_else(|| ti.get("url"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });
    match (name.is_empty(), scrap) {
        (true, None) => None,
        (true, Some(s)) => Some(s),
        (false, None) => Some(name.to_string()),
        (false, Some(s)) => Some(format!("{name}: {s}")),
    }
}

fn apply_event(reg: &mut HashMap<String, Session>, ev: &HookEvent) {
    let name = match ev.hook_event_name.as_deref() {
        Some(n) => n,
        None => return,
    };
    let sid = match ev.session_id.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        // missing session_id never creates or mutates a session
        _ => return,
    };
    let cwd = ev.cwd.clone().unwrap_or_default();

    // SessionEnd and unknown event names must never create sessions
    // (captured: ~15 sessions appeared ONLY as SessionEnd)
    let creates = matches!(
        name,
        "SessionStart"
            | "UserPromptSubmit"
            | "PreToolUse"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "PermissionRequest"
            | "Notification"
            | "Elicitation"
            | "Stop"
            | "StopFailure"
    );

    if name == "SessionEnd" {
        reg.remove(&sid);
        return;
    }
    if !creates {
        if let Some(s) = reg.get_mut(&sid) {
            s.touch();
        }
        return;
    }

    let is_new = !reg.contains_key(&sid);
    let s = reg.entry(sid).or_insert_with(|| {
        // a session first seen mid-turn (Pre/PostToolUse etc.) is active;
        // captured: zero SessionStart in headless runs, so first-seen
        // creation from any event is the normal path, not an edge case
        let initial = match name {
            "SessionStart" => S::Idle,
            _ => S::Working,
        };
        Session::new(cwd.clone(), initial)
    });
    if !cwd.is_empty() {
        s.cwd = cwd;
    }
    if name != "SessionStart" {
        s.engaged = true;
    }

    match name {
        "SessionStart" => {
            // existing sid: source:"resume" must not stomp live state
            if is_new {
                s.set(S::Idle, "", None);
            } else {
                s.touch();
            }
        }
        "UserPromptSubmit" => {
            // clears NeedsInput/Error/Done; deliberately does NOT clear
            // open_tools (a queued prompt lands while tools may still run)
            s.set(S::Working, "", ev.prompt.clone());
        }
        "PreToolUse" => {
            if let Some(id) = ev.tool_use_id.clone() {
                if !s.open_tools.iter().any(|(i, _)| *i == id) {
                    s.open_tools.push((id, ev.tool_name.clone().unwrap_or_default()));
                }
            }
            s.last_tool_at = Instant::now();
            s.set(S::Working, "", tool_detail(ev));
        }
        "PostToolUse" | "PostToolUseFailure" => {
            // the tool is over either way; an early return below must not
            // leave a ghost open tool (busy flag, no "thinking", 15-min leash)
            if let Some(id) = ev.tool_use_id.as_deref() {
                s.open_tools.retain(|(i, _)| i != id);
            }
            if name == "PostToolUseFailure" && ev.is_interrupt == Some(true) {
                // Escape pressed: the user is present, nothing is broken, and
                // the whole tool batch is aborted with it
                s.open_tools.clear();
                s.set(S::Idle, "", None);
                return;
            }
            // PostToolUseFailure maps like PostToolUse, NOT Error: all 10
            // captured failures were routine mid-turn events (timeouts,
            // exit-code-1) in sessions that finished normally. Error belongs
            // to StopFailure alone.
            s.last_tool_at = Instant::now();
            match s.state {
                S::Working | S::NeedsInput => s.set(S::Working, "", tool_detail(ev)),
                // a post-Stop straggler (captured: Stop→PostToolUse in
                // 0.376s) must not wedge Done back into un-decaying Working
                S::Done | S::Idle | S::Error => s.touch(),
            }
        }
        "PermissionRequest" => {
            // richest detail source; note: carries NO tool_use_id (captured)
            s.set(S::NeedsInput, "permission", tool_detail(ev));
        }
        "Notification" => match ev.notification_type.as_deref() {
            Some("permission_prompt") => {
                // the ~6s-later double-signal of PermissionRequest: keep the
                // existing since + richer detail, only fill if empty
                let fill = if s.detail.is_empty() { ev.message.clone() } else { None };
                s.set(S::NeedsInput, "permission", fill);
            }
            Some(k @ ("idle_prompt" | "agent_needs_input" | "elicitation_dialog")) => {
                s.set(S::NeedsInput, k, ev.message.clone());
            }
            Some("agent_completed") => {
                if !matches!(s.state, S::NeedsInput | S::Error) {
                    s.set(S::Done, "", ev.message.clone());
                } else {
                    s.touch();
                }
            }
            // unknown or missing type: fail LOUD per spec — an unrecognized
            // "Claude wants something" beats silently looking idle
            _ => s.set(S::NeedsInput, "unknown", ev.message.clone()),
        },
        "Elicitation" => {
            // shape never captured — fully defensive
            let detail = ev.message.clone().or_else(|| ev.prompt.clone());
            s.set(S::NeedsInput, "elicitation", detail);
        }
        "Stop" => {
            // "next real signal" — clears NeedsInput per spec
            s.open_tools.clear();
            s.set(S::Done, "", ev.last_assistant_message.clone());
        }
        "StopFailure" => {
            s.set(S::Error, "", ev.error.clone());
        }
        _ => unreachable!("gated by `creates`"),
    }
}

// --- Cowork reducer. What the 2026-09-05 live drill established: a cloud
// session's "needs you" arrives as a
// `cowork-awaiting-cse_<Id>` / `cowork-idle-cse_<Id>` toast (title = session
// name, body = a localized "Claude needs your answer"); the row is NOT
// deleted when the user answers, so resumed host activity is the clearing
// evidence; in-app permission prompts produce no toast and no log line. ---

#[derive(Default)]
struct CoworkState {
    health: CoworkHealth,
    /// toast rows that duplicate a hook-tracked ask: their removal is ignored
    hook_owned: HashSet<i64>,
    /// last Activity/Grant from any cloud session
    last_cloud_seen: Option<Instant>,
    /// since when the Claude desktop window has been the foreground window
    /// (None = it is not, or nobody is asking so nobody checked)
    app_front_since: Option<Instant>,
    /// CLI session uuid -> the app's `local_…` id (from the desktop log),
    /// kept so hook sessions that appear later still get their deep link
    cli_to_app: HashMap<String, String>,
}

impl CoworkState {
    /// Deaf-while-active, the Cowork way: the hook watchdog's reasons stay
    /// global; toast blindness only escalates while a cloud session was
    /// recently active (prompts could be going unseen right now).
    fn blind_reason(&self) -> Option<String> {
        let t = self.health.toasts.as_ref()?;
        let recent = self.last_cloud_seen.map_or(false, |at| at.elapsed() < COWORK_BLIND_WINDOW);
        recent.then(|| format!("Cowork prompts invisible: {t}"))
    }
}

/// The foreground-window rule (see COWORK_SEEN_AFTER). `front` = the
/// Claude desktop window is the foreground window right now; the caller
/// only bothers to look while a Cowork ask is pending.
fn cowork_seen_tick(reg: &mut HashMap<String, Session>, cw: &mut CoworkState, front: bool, now: Instant) {
    if !front {
        cw.app_front_since = None;
        return;
    }
    let since = *cw.app_front_since.get_or_insert(now);
    if now.duration_since(since) < COWORK_SEEN_AFTER {
        return;
    }
    for s in reg.values_mut() {
        if s.surface != Surface::Hook && s.state == S::NeedsInput {
            s.ask_row = None;
            s.set(S::Idle, "", None);
        }
    }
}

/// Give hook rows the app's own session id as their deep-link target.
/// Runs every tick: cheap (only rows with an empty link are looked up).
fn attach_links(reg: &mut HashMap<String, Session>, cw: &CoworkState) {
    if cw.cli_to_app.is_empty() {
        return;
    }
    for (id, s) in reg.iter_mut() {
        if s.surface == Surface::Hook && s.link_id.is_empty() {
            if let Some(app) = cw.cli_to_app.get(id) {
                s.link_id = app.clone();
            }
        }
    }
}

fn cowork_ask_pending(reg: &HashMap<String, Session>) -> bool {
    reg.values().any(|s| s.surface != Surface::Hook && s.state == S::NeedsInput)
}

/// Is the foreground window owned by the Claude desktop app? The CLI's
/// `claude.exe` never owns a foreground window (it lives in a terminal),
/// so an image name match on the foreground window's process is enough.
fn claude_app_in_front() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: plain Win32 queries; the snapshot handle is closed on every path
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return false;
        }
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
                    let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                    found = String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case("claude.exe");
                    break;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

fn cowork_session(surface: Surface, state: S) -> Session {
    let mut s = Session::new(String::new(), state);
    s.surface = surface;
    s.engaged = true;
    s
}

fn apply_cowork(reg: &mut HashMap<String, Session>, ev: &CoworkEvent, cw: &mut CoworkState) {
    let now = Instant::now();
    match ev {
        CoworkEvent::Health(h) => cw.health = h.clone(),
        CoworkEvent::Mapped { cli_sid, app_sid } => {
            cw.cli_to_app.insert(cli_sid.clone(), app_sid.clone());
            attach_links(reg, cw);
        }
        CoworkEvent::ToastAdded { row, tag, group, title, body, arrival, seeded } => {
            toast_added(reg, cw, *row, tag, group, title, body, *arrival, *seeded, now)
        }
        CoworkEvent::ToastGone { row, evicted } => {
            if cw.hook_owned.remove(row) {
                return;
            }
            let Some(s) = reg.values_mut().find(|s| s.ask_row == Some(*row)) else { return };
            if *evicted {
                // Windows ran out of slots: never clear an ask over that
                if !s.detail.ends_with("(toast evicted)") {
                    s.detail = truncate(&format!("{} (toast evicted)", s.detail));
                }
            } else if s.state == S::NeedsInput {
                // the app closed it or the user dismissed it: they responded
                s.ask_row = None;
                s.set(S::Idle, "", None);
            }
        }
        CoworkEvent::Activity { sid, folders } => {
            cw.last_cloud_seen = Some(now);
            let key = if sid.is_empty() {
                // remote-file lines carry no id: only an unambiguous single
                // cloud session takes them
                let mut cloud = reg.iter().filter(|(_, s)| s.surface == Surface::CoworkCloud).map(|(k, _)| k.clone());
                match (cloud.next(), cloud.next()) {
                    (Some(k), None) => k,
                    _ => return,
                }
            } else {
                sid.to_ascii_lowercase()
            };
            let s = reg.entry(key).or_insert_with(|| cowork_session(Surface::CoworkCloud, S::Working));
            s.engaged = true;
            if let Some(f) = folders.first() {
                s.cwd = f.clone();
            }
            s.last_tool_at = now;
            s.last_activity_at = now;
            match s.state {
                // a command logged a beat before the toast is not an answer
                S::NeedsInput if now.duration_since(s.since) < COWORK_ASK_SETTLE => s.touch(),
                S::Error => s.touch(),
                _ => {
                    s.ask_row = None;
                    s.detail.clear();
                    s.set(S::Working, "", None);
                }
            }
        }
        CoworkEvent::Grant { sid, sid_display, cleared } => {
            cw.last_cloud_seen = Some(now);
            let key = sid.to_ascii_lowercase();
            if *cleared {
                if reg.get(&key).map_or(false, |s| s.state != S::NeedsInput) {
                    reg.remove(&key);
                }
            } else {
                let s = reg.entry(key).or_insert_with(|| cowork_session(Surface::CoworkCloud, S::Idle));
                s.link_id = sid_display.clone();
                s.engaged = true;
                s.touch();
            }
        }
    }
}

/// Word stems that mean "Claude wants something" in a toast body of an
/// unrecognized tag (the desktop app is localized: English and Spanish
/// seen so far). Unrecognized asks must fail loud, never silent.
const ASK_WORDS: [&str; 10] = [
    "permi", "allow", "approv", "input", "question", "waiting",
    "aprob", "respuesta", "pregunta", "esperando",
];

fn toast_added(
    reg: &mut HashMap<String, Session>,
    cw: &mut CoworkState,
    row: i64,
    tag: &str,
    group: &str,
    title: &str,
    body: &str,
    arrival: SystemTime,
    seeded: bool,
    now: Instant,
) {
    // cloud Cowork asks: the tag carries the session id (mixed case)
    if let Some(id) = tag.strip_prefix("cowork-awaiting-").or_else(|| tag.strip_prefix("cowork-idle-")) {
        let kind = if tag.starts_with("cowork-awaiting-") { "agent_needs_input" } else { "idle_prompt" };
        let s = reg.entry(id.to_ascii_lowercase()).or_insert_with(|| cowork_session(Surface::CoworkCloud, S::Idle));
        s.link_id = id.to_string();
        if !title.is_empty() {
            s.title = title.to_string();
        }
        s.ask_row = Some(row);
        s.engaged = true;
        if seeded {
            // a row that predates the pet: the popover shows what the
            // session last wanted, but the pet does not hop for history
            // (the user saw the pet launch for hours-old, long-answered
            // asks — 2026-09-05)
            if s.state != S::NeedsInput {
                s.set(S::Idle, "", Some(body.to_string()));
            }
        } else {
            s.set(S::NeedsInput, kind, Some(body.to_string()));
        }
        return;
    }
    if seeded {
        return; // only cloud rows are ever seeded (toasts.rs), and only as idle
    }

    let kind = if tag.starts_with("permission-") || tag.starts_with("cowork-remote-folder-request") {
        "permission"
    } else if tag.starts_with("ask-question-") {
        "elicitation_dialog"
    } else if tag.starts_with("code-prompt-") {
        "agent_needs_input"
    } else if tag.starts_with("idle-") {
        "idle_prompt"
    } else {
        // scheduled-task-*, unknown: ignored unless the body asks for something
        let b = body.to_lowercase();
        if !ASK_WORDS.iter().any(|w| b.contains(w)) {
            return;
        }
        let s = reg
            .entry(format!("cowork:{tag}"))
            .or_insert_with(|| cowork_session(Surface::CoworkCloud, S::Idle));
        if !title.is_empty() {
            s.title = title.to_string();
        }
        s.ask_row = Some(row);
        s.set(S::NeedsInput, "unknown", Some(body.to_string()));
        return;
    };

    let (key, surface) = if let Some(local) = group.strip_prefix("session-local_") {
        // local Claude Code sessions are hook-tracked: a toast raised
        // while some hook session had an event within ±10 s is that
        // session's own ask seen twice. State is deliberately NOT
        // compared: a Stop's Done lasts under a second when the next turn
        // starts at once (a background task re-invoking the session did
        // exactly that live, and the idle toast became a phantom row).
        let age = arrival.elapsed().unwrap_or_default();
        let dup = reg.values().any(|s| {
            if s.surface != Surface::Hook {
                return false;
            }
            let last_event = now.duration_since(s.last_event_at);
            let entered = now.duration_since(s.since);
            last_event.abs_diff(age) <= COWORK_DEDUPE || entered.abs_diff(age) <= COWORK_DEDUPE
        });
        if dup {
            cw.hook_owned.insert(row);
            return;
        }
        // no hook twin: a local-VM Cowork session (no hooks inside the VM)
        (format!("local_{local}"), Surface::CoworkLocal)
    } else if let Some(id) = group.strip_prefix("session-") {
        // `session-cse_…` / `session-session_…`: a cloud session's own prompt
        (id.to_ascii_lowercase(), Surface::CoworkCloud)
    } else {
        // no session in the toast: the single recently active cloud
        // session owns it; otherwise a pseudo row keyed on the tag
        let mut active = reg
            .iter()
            .filter(|(_, s)| s.surface == Surface::CoworkCloud && now.duration_since(s.last_activity_at) < COWORK_ATTACH_WINDOW)
            .map(|(k, _)| k.clone());
        match (active.next(), active.next()) {
            (Some(k), None) => (k, Surface::CoworkCloud),
            _ => (format!("cowork:{tag}"), Surface::CoworkCloud),
        }
    };

    let s = reg.entry(key).or_insert_with(|| cowork_session(surface, S::Idle));
    if s.surface == Surface::Hook {
        return; // never let a toast rewrite a hook-tracked row
    }
    if group.starts_with("session-cse_") {
        s.link_id = group["session-".len()..].to_string();
    }
    if !title.is_empty() {
        s.title = title.to_string();
    }
    s.ask_row = Some(row);
    s.engaged = true;
    s.set(S::NeedsInput, kind, Some(body.to_string()));
}

fn decay(reg: &mut HashMap<String, Session>) {
    let now = Instant::now();
    for s in reg.values_mut() {
        match s.state {
            S::Done if now.duration_since(s.since) >= DONE_TO_IDLE => s.set(S::Idle, "", None),
            S::Error if now.duration_since(s.since) >= ERROR_TO_IDLE => s.set(S::Idle, "", None),
            // a cloud session's turns are invisible between host commands:
            // Working is a lower bound and must decay on its own
            S::Working
                if s.surface != Surface::Hook
                    && now.duration_since(s.last_activity_at) >= COWORK_WORK_STALE =>
            {
                s.set(S::Idle, "", None)
            }
            // user stopped/abandoned a generation: no event will ever come
            S::Working
                if s.open_tools.is_empty()
                    && now.duration_since(s.last_event_at) >= THINK_STALE =>
            {
                s.set(S::Idle, "", None)
            }
            _ => {}
        }
    }
    // orphan sweep: silence drops a session — but never a pending ask (a
    // prompt waiting on the user is silent by nature), and working sessions
    // with open tools get the long leash
    reg.retain(|_, s| {
        let silent = now.duration_since(s.last_event_at);
        match s.state {
            // a Cowork ask never gets a "user came back" event: it expires
            S::NeedsInput => s.surface == Surface::Hook || now.duration_since(s.since) < COWORK_ASK_MAX,
            S::Working if !s.open_tools.is_empty() => silent < ORPHAN_DROP_OPEN_TOOLS,
            // opened-but-never-used session views reap fast
            S::Idle if !s.engaged => silent < UNENGAGED_DROP,
            _ => silent < ORPHAN_DROP,
        }
    });
}

fn publish(reg: &HashMap<String, Session>, handle: &AppHandle, blind: Option<&str>, cowork_health: &str) {
    let payload = reduce(reg, blind, cowork_health);
    let store = handle.state::<PetStateStore>();
    let mut cur = store.0.lock_or_recover();
    if same_display(&cur, &payload) {
        return;
    }
    *cur = payload.clone();
    drop(cur);
    let _ = handle.emit_to("pet", "pet-state", payload);
}

/// Working displays as "thinking" while no tool has run for a beat — Claude
/// is generating, not executing. Everything else displays its own name.
fn display_name(s: &Session, now: Instant) -> &'static str {
    if s.state == S::Working
        && s.open_tools.is_empty()
        && now.duration_since(s.last_tool_at) >= THINK_LAG
    {
        "thinking"
    } else {
        s.state.name()
    }
}

fn reduce(reg: &HashMap<String, Session>, blind_reason: Option<&str>, cowork_health: &str) -> PetStatePayload {
    let now = Instant::now();
    let mut sessions: Vec<SessionInfo> = reg
        .iter()
        .map(|(id, s)| SessionInfo {
            id: id.clone(),
            cwd: s.cwd.clone(),
            state: display_name(s, now).into(),
            kind: s.kind.clone(),
            detail: s.detail.clone(),
            since_ms: now.duration_since(s.since).as_millis() as u64,
            busy: !s.open_tools.is_empty(),
            // newest first: tools[0] is the tool that started most recently,
            // which is what the popover row names
            tools: s.open_tools.iter().rev().map(|(_, n)| n.clone()).collect(),
            surface: s.surface.name(),
            title: s.title.clone(),
            link_id: s.link_id.clone(),
        })
        .collect();
    // stable order: HashMap iteration is nondeterministic, and a shuffled
    // list would defeat the emit-on-change comparison
    sessions.sort_by(|a, b| a.id.cmp(&b.id));

    // worst session wins; ties: the ask you're most overdue on (oldest since)
    // for needs_input, freshest activity for everything else
    let top = reg.values().max_by(|a, b| {
        a.state.rank().cmp(&b.state.rank()).then_with(|| {
            if a.state == S::NeedsInput {
                now.duration_since(a.since).cmp(&now.duration_since(b.since))
            } else {
                a.last_event_at.cmp(&b.last_event_at)
            }
        })
    });

    let (state, detail, kind) = match top {
        None => ("sleeping".to_string(), String::new(), String::new()),
        Some(s) => (display_name(s, now).to_string(), s.detail.clone(), s.kind.clone()),
    };

    // blind is global rank 4: outranked only by a pending ask or real error
    let blind = blind_reason.is_some();
    let (state, detail) = match blind_reason {
        Some(reason) if top.map_or(true, |s| s.state.rank() < 4) => {
            ("blind".to_string(), reason.to_string())
        }
        _ => (state, detail),
    };

    PetStatePayload {
        state,
        detail,
        kind,
        n_sessions: reg.len(),
        blind,
        sessions,
        cowork_health: cowork_health.to_string(),
    }
}

#[cfg(test)]
mod cowork_tests {
    //! The Cowork transition table on a fresh registry (rows from the
    //! 2026-09-05 live drill).
    use super::*;

    fn added(row: i64, tag: &str, group: &str, title: &str, body: &str) -> CoworkEvent {
        CoworkEvent::ToastAdded {
            row,
            tag: tag.into(),
            group: group.into(),
            title: title.into(),
            body: body.into(),
            arrival: SystemTime::now(),
            seeded: false,
        }
    }

    fn seeded(mut ev: CoworkEvent) -> CoworkEvent {
        if let CoworkEvent::ToastAdded { seeded, .. } = &mut ev {
            *seeded = true;
        }
        ev
    }

    #[test]
    fn seeded_rows_are_idle_with_the_ask_as_detail_never_a_hop() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        apply(&mut reg, &mut cw, &[seeded(added(1, &format!("cowork-idle-{SID_DISPLAY}"), "Notifications",
            "Example Cowork session", "Claude est\u{e1} esperando tu respuesta"))]);
        let s = &reg[SID];
        assert_eq!(s.state, S::Idle);
        assert_eq!(s.detail, "Claude est\u{e1} esperando tu respuesta");
        assert_eq!(s.title, "Example Cowork session");
        assert_eq!(s.link_id, SID_DISPLAY);
        assert_eq!(reduce(&reg, None, "").state, "idle");
        // a live toast afterwards is a real ask
        apply(&mut reg, &mut cw, &[added(2, &format!("cowork-awaiting-{SID_DISPLAY}"), "", "Permission request", "B")]);
        assert_eq!(reg[SID].state, S::NeedsInput);
        // a seeded row never downgrades a live ask
        apply(&mut reg, &mut cw, &[seeded(added(3, &format!("cowork-idle-{SID_DISPLAY}"), "", "T", "old"))]);
        assert_eq!(reg[SID].state, S::NeedsInput);
        // seeded non-cloud rows are ignored outright
        apply(&mut reg, &mut cw, &[seeded(added(4, "permission-x", "session-local_zzz", "T", "Allow?"))]);
        assert!(!reg.contains_key("local_zzz"));
    }

    fn activity(sid: &str, folders: &[&str]) -> CoworkEvent {
        CoworkEvent::Activity { sid: sid.into(), folders: folders.iter().map(|f| f.to_string()).collect() }
    }

    fn apply(reg: &mut HashMap<String, Session>, cw: &mut CoworkState, evs: &[CoworkEvent]) {
        for ev in evs {
            apply_cowork(reg, ev, cw);
        }
    }

    const SID: &str = "cse_01examplesessionidxxxxxx";
    const SID_DISPLAY: &str = "cse_01EXAMPLEsessionIDxxxxxx";

    #[test]
    fn awaiting_toast_is_a_needs_input_row_with_title_and_link() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        apply(&mut reg, &mut cw, &[added(28581, &format!("cowork-awaiting-{SID_DISPLAY}"), "Notifications",
            "Example Cowork session", "Claude necesita tu respuesta para continuar")]);
        let s = &reg[SID];
        assert_eq!((s.state, s.kind.as_str()), (S::NeedsInput, "agent_needs_input"));
        assert_eq!(s.surface, Surface::CoworkCloud);
        assert_eq!(s.title, "Example Cowork session");
        assert_eq!(s.link_id, SID_DISPLAY);
        assert_eq!(s.detail, "Claude necesita tu respuesta para continuar");
        assert_eq!(s.ask_row, Some(28581));
        // idle toast on the same session: same row, kind idle_prompt
        apply(&mut reg, &mut cw, &[added(28586, &format!("cowork-idle-{SID_DISPLAY}"), "Notifications",
            "Example Cowork session", "Claude está esperando tu respuesta")]);
        assert_eq!(reg.len(), 1);
        assert_eq!((reg[SID].state, reg[SID].kind.as_str()), (S::NeedsInput, "idle_prompt"));
        let p = reduce(&reg, None, "");
        assert_eq!(p.state, "needs_input");
        assert_eq!(p.sessions[0].surface, "cowork");
        assert_eq!(p.sessions[0].link_id, SID_DISPLAY);
    }

    #[test]
    fn activity_creates_working_rows_and_clears_settled_asks_only() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        apply(&mut reg, &mut cw, &[activity(SID, &["Project folder", "Preguntas (Discussion)"])]);
        let s = &reg[SID];
        assert_eq!(s.state, S::Working);
        assert_eq!(s.cwd, "Project folder");
        assert_eq!(s.surface, Surface::CoworkCloud);
        assert!(cw.last_cloud_seen.is_some());

        // a fresh ask is NOT cleared by a command logged a beat later
        apply(&mut reg, &mut cw, &[added(1, &format!("cowork-awaiting-{SID_DISPLAY}"), "", "T", "B")]);
        apply(&mut reg, &mut cw, &[activity(SID, &[])]);
        assert_eq!(reg[SID].state, S::NeedsInput);
        // a settled ask is: resumed activity is the user's answer
        reg.get_mut(SID).unwrap().since = Instant::now() - COWORK_ASK_SETTLE;
        apply(&mut reg, &mut cw, &[activity(SID, &[])]);
        assert_eq!(reg[SID].state, S::Working);
        assert_eq!(reg[SID].ask_row, None);
        assert!(reg[SID].detail.is_empty());
    }

    #[test]
    fn empty_sid_activity_needs_exactly_one_cloud_session() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        apply(&mut reg, &mut cw, &[activity("", &[])]);
        assert!(reg.is_empty());
        apply(&mut reg, &mut cw, &[CoworkEvent::Grant { sid: SID.into(), sid_display: SID_DISPLAY.into(), cleared: false }]);
        assert_eq!(reg[SID].state, S::Idle);
        assert_eq!(reg[SID].link_id, SID_DISPLAY);
        apply(&mut reg, &mut cw, &[activity("", &[])]);
        assert_eq!(reg[SID].state, S::Working);
        // two cloud sessions: ambiguous, dropped
        apply(&mut reg, &mut cw, &[activity("cse_other", &["X"])]);
        reg.get_mut(SID).unwrap().state = S::Idle;
        apply(&mut reg, &mut cw, &[activity("", &[])]);
        assert_eq!(reg[SID].state, S::Idle);
    }

    #[test]
    fn grant_cleared_removes_unless_asking() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        let grant = |cleared| CoworkEvent::Grant { sid: SID.into(), sid_display: SID_DISPLAY.into(), cleared };
        apply(&mut reg, &mut cw, &[grant(false), grant(true)]);
        assert!(reg.is_empty());
        apply(&mut reg, &mut cw, &[grant(false), added(5, &format!("cowork-idle-{SID_DISPLAY}"), "", "T", "B"), grant(true)]);
        assert_eq!(reg[SID].state, S::NeedsInput);
    }

    #[test]
    fn toast_gone_clears_only_a_real_answer() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        apply(&mut reg, &mut cw, &[added(7, &format!("cowork-awaiting-{SID_DISPLAY}"), "", "T", "B")]);
        apply(&mut reg, &mut cw, &[CoworkEvent::ToastGone { row: 7, evicted: true }]);
        assert_eq!(reg[SID].state, S::NeedsInput);
        assert!(reg[SID].detail.ends_with("(toast evicted)"));
        apply(&mut reg, &mut cw, &[CoworkEvent::ToastGone { row: 7, evicted: false }]);
        assert_eq!(reg[SID].state, S::Idle);
        assert_eq!(reg[SID].ask_row, None);
        // an unknown row is a no-op
        apply(&mut reg, &mut cw, &[CoworkEvent::ToastGone { row: 99, evicted: false }]);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn local_toast_dedupes_against_a_hook_ask_else_becomes_a_local_row() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        // a hook-tracked session entered NeedsInput just now
        let mut hook = Session::new("C:\\proj".into(), S::Working);
        hook.set(S::NeedsInput, "permission", Some("Bash: rm x".into()));
        reg.insert("example-hook".into(), hook);
        apply(&mut reg, &mut cw, &[added(11, "permission-acc8", "session-local_170af1dd",
            "Claw'dbot", "Allow Claude to EnterWorktree …?")]);
        assert_eq!(reg.len(), 1, "same ask, seen twice");
        assert!(cw.hook_owned.contains(&11));
        apply(&mut reg, &mut cw, &[CoworkEvent::ToastGone { row: 11, evicted: false }]);
        assert_eq!(reg["example-hook"].state, S::NeedsInput, "hook rows are never touched by toasts");

        // no hook twin at all: a local-VM Cowork session
        reg.clear();
        apply(&mut reg, &mut cw, &[added(12, "ask-question-fd", "session-local_842d0b96", "Cleanup", "Which sessions?")]);
        let s = &reg["local_842d0b96"];
        assert_eq!((s.surface, s.state, s.kind.as_str()), (Surface::CoworkLocal, S::NeedsInput, "elicitation_dialog"));
        assert_eq!(s.title, "Cleanup");
        assert_eq!(reduce(&reg, None, "").sessions[0].surface, "cowork");
    }

    #[test]
    fn idle_local_toast_dedupes_against_done_and_idle_hook_rows() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        let mut hook = Session::new("C:\\proj".into(), S::Working);
        hook.set(S::Done, "", Some("all done".into()));
        reg.insert("hook".into(), hook);
        apply(&mut reg, &mut cw, &[added(13, "idle-local_b171", "session-local_b171", "Title", "summary")]);
        assert_eq!(reg.len(), 1);
        // the Stop's Done was replaced by the next turn's Working within a
        // second (background task re-invoking the session): still a twin,
        // because the hook row had an event when the toast was raised
        {
            let h = reg.get_mut("hook").unwrap();
            h.set(S::Working, "", Some("next prompt".into()));
        }
        apply(&mut reg, &mut cw, &[added(15, "idle-local_b171", "session-local_b171", "Title", "summary")]);
        assert_eq!(reg.len(), 1, "state-independent dedupe");
        // a hook row that had no event for a minute is not a twin
        {
            let h = reg.get_mut("hook").unwrap();
            h.since = Instant::now() - Duration::from_secs(60);
            h.last_event_at = Instant::now() - Duration::from_secs(60);
        }
        apply(&mut reg, &mut cw, &[added(14, "idle-local_b171", "session-local_b171", "Title", "summary")]);
        assert_eq!(reg.len(), 2);
        assert_eq!(reg["local_b171"].kind, "idle_prompt");
    }

    #[test]
    fn groupless_permission_attaches_to_the_single_active_cloud_session() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        apply(&mut reg, &mut cw, &[activity(SID, &["F"])]);
        apply(&mut reg, &mut cw, &[added(21, "permission-x", "", "Cowork", "Allow Claude to Run ls?")]);
        assert_eq!(reg.len(), 1);
        assert_eq!((reg[SID].state, reg[SID].kind.as_str()), (S::NeedsInput, "permission"));
        // a cse group names the session directly
        reg.clear();
        apply(&mut reg, &mut cw, &[added(22, "permission-y", &format!("session-{SID_DISPLAY}"), "Cowork", "Allow?")]);
        assert_eq!(reg[SID].link_id, SID_DISPLAY);
        // nothing active: pseudo row, still loud
        reg.clear();
        apply(&mut reg, &mut cw, &[added(23, "cowork-remote-folder-request", "", "Claude", "Claude would like access to a folder")]);
        assert_eq!(reg["cowork:cowork-remote-folder-request"].state, S::NeedsInput);
    }

    #[test]
    fn unknown_tags_are_silent_unless_they_ask_for_something() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        apply(&mut reg, &mut cw, &[added(31, "scheduled-task-1", "", "Task", "Report finished")]);
        assert!(reg.is_empty());
        apply(&mut reg, &mut cw, &[added(32, "mystery-2", "", "Task", "Claude está esperando tu respuesta")]);
        assert_eq!((reg["cowork:mystery-2"].state, reg["cowork:mystery-2"].kind.as_str()), (S::NeedsInput, "unknown"));
    }

    #[test]
    fn decay_idles_stale_cloud_work_and_expires_old_asks() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        apply(&mut reg, &mut cw, &[activity(SID, &["F"])]);
        reg.get_mut(SID).unwrap().last_activity_at = Instant::now() - COWORK_WORK_STALE;
        decay(&mut reg);
        assert_eq!(reg[SID].state, S::Idle);
        apply(&mut reg, &mut cw, &[added(41, &format!("cowork-idle-{SID_DISPLAY}"), "", "T", "B")]);
        decay(&mut reg);
        assert_eq!(reg[SID].state, S::NeedsInput, "a fresh ask survives decay");
        reg.get_mut(SID).unwrap().since = Instant::now() - COWORK_ASK_MAX;
        decay(&mut reg);
        assert!(reg.is_empty(), "a 12 h old Cowork ask expires");
    }

    #[test]
    fn health_folds_into_payload_and_escalates_only_while_active() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        let bad = CoworkHealth { toasts: Some("notification store unreadable: x".into()), log: None };
        apply(&mut reg, &mut cw, &[CoworkEvent::Health(bad.clone())]);
        assert_eq!(cw.health.summary(), "notification store unreadable: x");
        assert_eq!(cw.blind_reason(), None, "no cloud activity: a header line, not blind");
        apply(&mut reg, &mut cw, &[activity(SID, &["F"])]);
        assert_eq!(cw.blind_reason().as_deref(), Some("Cowork prompts invisible: notification store unreadable: x"));
        let p = reduce(&reg, cw.blind_reason().as_deref(), &cw.health.summary());
        assert_eq!(p.state, "blind");
        assert!(p.blind);
        assert_eq!(p.cowork_health, "notification store unreadable: x");
        apply(&mut reg, &mut cw, &[CoworkEvent::Health(CoworkHealth::default())]);
        assert_eq!(cw.blind_reason(), None);
    }

    #[test]
    fn claude_window_in_front_clears_cowork_asks_after_the_dwell_never_hook_asks() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        let mut hook = Session::new("C:\\proj".into(), S::Working);
        hook.set(S::NeedsInput, "permission", Some("Bash: rm x".into()));
        reg.insert("hook".into(), hook);
        apply(&mut reg, &mut cw, &[added(1, &format!("cowork-awaiting-{SID_DISPLAY}"), "", "Permission request", "B")]);
        let t0 = Instant::now();
        // window not in front: nothing, and the dwell timer is reset
        cowork_seen_tick(&mut reg, &mut cw, false, t0);
        assert_eq!(reg[SID].state, S::NeedsInput);
        assert!(cw.app_front_since.is_none());
        // in front, but not long enough
        cowork_seen_tick(&mut reg, &mut cw, true, t0);
        cowork_seen_tick(&mut reg, &mut cw, true, t0 + Duration::from_secs(2));
        assert_eq!(reg[SID].state, S::NeedsInput);
        // glanced away: the dwell restarts
        cowork_seen_tick(&mut reg, &mut cw, false, t0 + Duration::from_secs(2));
        cowork_seen_tick(&mut reg, &mut cw, true, t0 + Duration::from_secs(3));
        cowork_seen_tick(&mut reg, &mut cw, true, t0 + Duration::from_secs(5));
        assert_eq!(reg[SID].state, S::NeedsInput);
        // dwell reached: the Cowork ask clears, the hook ask does not
        cowork_seen_tick(&mut reg, &mut cw, true, t0 + Duration::from_secs(6));
        assert_eq!(reg[SID].state, S::Idle);
        assert_eq!(reg[SID].ask_row, None);
        assert_eq!(reg["hook"].state, S::NeedsInput);
        assert!(cowork_ask_pending(&reg) == false);
    }

    #[test]
    fn mapping_gives_hook_rows_the_app_id_as_link_before_or_after_they_appear() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        reg.insert("cli-1".into(), Session::new("C:\\p".into(), S::Working));
        apply(&mut reg, &mut cw, &[CoworkEvent::Mapped { cli_sid: "cli-1".into(), app_sid: "local_a".into() },
                                   CoworkEvent::Mapped { cli_sid: "cli-2".into(), app_sid: "local_b".into() }]);
        assert_eq!(reg["cli-1"].link_id, "local_a");
        // a session that shows up later gets its link on the next tick
        reg.insert("cli-2".into(), Session::new("C:\\q".into(), S::Idle));
        assert_eq!(reg["cli-2"].link_id, "");
        attach_links(&mut reg, &cw);
        assert_eq!(reg["cli-2"].link_id, "local_b");
        let p = reduce(&reg, None, "");
        assert!(p.sessions.iter().all(|s| s.surface == "code" && s.link_id.starts_with("local_")));
    }

    #[test]
    fn hook_rows_are_untouched_by_cowork_events() {
        let (mut reg, mut cw) = (HashMap::new(), CoworkState::default());
        reg.insert("hook".into(), Session::new("C:\\proj".into(), S::Working));
        apply(&mut reg, &mut cw, &[activity(SID, &["F"]), added(1, &format!("cowork-idle-{SID_DISPLAY}"), "", "T", "B")]);
        let h = &reg["hook"];
        assert_eq!((h.surface, h.state), (Surface::Hook, S::Working));
        let p = reduce(&reg, None, "");
        let hook = p.sessions.iter().find(|s| s.id == "hook").unwrap();
        assert_eq!((hook.surface, hook.title.as_str(), hook.link_id.as_str()), ("code", "", ""));
    }
}
