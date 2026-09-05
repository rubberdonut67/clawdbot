// Claude Code hook receiver: a tiny HTTP server on the port the user's
// global hooks already point at. The non-negotiable contract (proven in
// spikes/phase0): answer `200 {}` unconditionally and fast, so a slow or
// broken pet can never stall a Claude turn. Anything malformed is answered
// 200 and dropped — this server never becomes Claude's problem. The only
// work before the answer is a bounded serde parse and a non-blocking channel
// send (microseconds); the body READ, the one step that can stall, must
// precede the answer anyway.
// Each request is served on its own thread: tiny_http hands over larger
// bodies as a lazy socket reader, so a client that sends headers and then
// stalls would otherwise park the accept loop and silence every session.

use std::io::Read;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::cowork::CoworkEvent;
use crate::state::{PetEvent, PetStatePayload};
use crate::PoisonTolerant;

pub const HOOK_ADDR: &str = "127.0.0.1:4317";
const MAX_BODY: u64 = 4 * 1024 * 1024; // tool_response can embed whole files

/// One hook POST, fully optional-ized: hooks are an undocumented internal
/// surface, so no field is trusted to exist and unknown fields are ignored.
/// Field set observed across the 676 captured payloads (spikes/phase0).
#[derive(Debug, Clone, Deserialize)]
pub struct HookEvent {
    pub hook_event_name: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    #[allow(dead_code)] // stage-2 watchdog ground truth
    pub transcript_path: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub tool_use_id: Option<String>,
    pub prompt: Option<String>,
    pub message: Option<String>,
    pub notification_type: Option<String>,
    pub error: Option<String>,
    pub is_interrupt: Option<bool>,
    pub last_assistant_message: Option<String>,
    // captured on SessionStart ("startup"/"resume"); the mapping treats any
    // existing-sid SessionStart as touch-only, which subsumes the resume rule
    #[allow(dead_code)]
    pub source: Option<String>,
}

/// Bind the hook port (3 attempts, 1s apart) and pump parsed events into the
/// state thread. Returns Err if the port cannot be bound — the caller then
/// runs the pet in `blind` mode rather than exiting or moving ports (the
/// installed hooks point at 4317 statically; a deaf pet must say it's deaf).
/// `store` is the published pet state, served read-only at `GET /state`.
/// `inject` opens `POST /cowork-event` (config `cowork.debug_injection`):
/// synthetic Cowork events from loopback, the test door for the reducer.
pub fn spawn_server(
    tx: Sender<PetEvent>,
    store: Arc<Mutex<PetStatePayload>>,
    inject: bool,
) -> Result<(), String> {
    let mut last_err = String::new();
    let mut server = None;
    for attempt in 0..3 {
        match tiny_http::Server::http(HOOK_ADDR) {
            Ok(s) => {
                server = Some(s);
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                if attempt < 2 {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
    let server = match server {
        Some(s) => s,
        None => return Err(last_err),
    };

    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let tx = tx.clone();
            let store = store.clone();
            // a fallible spawn: `std::thread::spawn` would PANIC if the OS
            // cannot give us a thread, unwinding this loop and dropping the
            // listener (a deaf pet with no log line). On failure the closure
            // — and the request it owns — is dropped, which answers 500: the
            // cheapest safe outcome under resource exhaustion.
            let spawned = std::thread::Builder::new()
                .name("hook-serve".into())
                .spawn(move || serve(request, &tx, &store, inject));
            if let Err(e) = spawned {
                eprintln!("clawdbot: hook thread spawn failed ({e}); request answered 500");
            }
        }
    });
    Ok(())
}

/// Read (capped), forward, then answer 200. Forwarding BEFORE answering keeps
/// same-session event order by construction across per-request threads:
/// Claude Code fires the next hook only after it has seen this answer, so
/// the next event cannot reach the channel before this one.
///
/// `GET /state` is the read-only test door: the current pet-state payload
/// as JSON (the server binds loopback only). It shares the read-capped,
/// answer-always shape; `/event` behavior is unchanged.
fn serve(mut request: tiny_http::Request, tx: &Sender<PetEvent>, store: &Mutex<PetStatePayload>, inject: bool) {
    let is_post = *request.method() == tiny_http::Method::Post;
    let is_event_post = is_post && request.url().starts_with("/event");
    let is_inject_post = inject && is_post && request.url().starts_with("/cowork-event");
    let is_state_get =
        *request.method() == tiny_http::Method::Get && request.url().starts_with("/state");

    // read capped; a body that blows the cap just fails to parse
    let mut body = String::new();
    let read_ok = request
        .as_reader()
        .take(MAX_BODY)
        .read_to_string(&mut body)
        .is_ok();

    if is_event_post && read_ok {
        match serde_json::from_str::<HookEvent>(&body) {
            Ok(ev) => {
                // receiver gone = app shutting down; just stop caring
                let _ = tx.send(PetEvent::Hook(ev));
            }
            Err(e) => eprintln!("clawdbot: dropped unparseable hook body: {e}"),
        }
    }
    if is_inject_post && read_ok {
        match serde_json::from_str::<CoworkEvent>(&body) {
            Ok(ev) => {
                let _ = tx.send(PetEvent::Cowork(ev));
            }
            Err(e) => eprintln!("clawdbot: dropped unparseable cowork-event body: {e}"),
        }
    }

    let reply = if is_state_get {
        // snapshot under the lock, serialize outside it
        let snap = store.lock_or_recover().clone();
        serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into())
    } else {
        "{}".into()
    };

    // 200 {} unconditionally — malformed input is never Claude's problem
    let response = tiny_http::Response::from_string(reply).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
            .expect("static header"),
    );
    let _ = request.respond(response);
}
