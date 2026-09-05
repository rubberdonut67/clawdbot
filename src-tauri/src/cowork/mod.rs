// Cowork as a second event source (phase 3a). Cloud Cowork sessions leave no
// hooks and no transcripts on this machine; the two host-side signals are the Windows
// notification store (the desktop app's toasts, session ids in their tags)
// and the desktop app's live log. Both watchers are threads that hold a
// `Sender<PetEvent>` clone and feed the same state thread as the hooks.
//
// The classification rules were derived from a live Cowork run on
// 2026-09-05; `apply_cowork` in state.rs holds them.

pub mod log;
pub mod toasts;

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::state::PetEvent;
use crate::PoisonTolerant;

/// One observation from a Cowork watcher. `Deserialize` because the gated
/// `POST /cowork-event` test door injects these straight from JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)] // consumed by apply_cowork (plan step 4)
pub enum CoworkEvent {
    /// A new row of the desktop app's toast handler appeared. Title/body are
    /// empty for rows the platform has already condensed (cap eviction
    /// blanks the payload but keeps the row; the tag stays authoritative).
    ToastAdded {
        row: i64,
        tag: String,
        group: String,
        title: String,
        body: String,
        #[serde(default = "SystemTime::now")]
        arrival: SystemTime,
        /// true for rows that already existed when the pet started: a row's
        /// presence is no proof the ask is still open (answering in the app
        /// never deletes it), so seeded rows become idle rows, never asks
        #[serde(default)]
        seeded: bool,
    },
    /// A previously seen row was deleted. `evicted` = the platform did it
    /// (expiry, or the oldest slot of a full store); NOT an answer from the
    /// user. Otherwise the app closed it: the user answered, opened, or
    /// dismissed the prompt.
    ToastGone { row: i64, evicted: bool },
    /// A cloud session drove a host tool (desktop log). `sid` = "cse_<id>"
    /// lowercased; `folders` = the granted mount names.
    Activity { sid: String, folders: Vec<String> },
    /// Folder grants for a cloud session were created (`cleared` = false) or
    /// cleared. `sid_display` keeps the mixed-case id for deep links.
    Grant { sid: String, sid_display: String, cleared: bool },
    /// The desktop app's own id for a hook-tracked Claude Code session
    /// (`Mapping internal session local_<uuid> to CLI session <uuid>` in the
    /// desktop log). The app's `claude://code/continue?session=` deep link
    /// accepts only `local_…` ids, never the CLI uuid the hooks report.
    Mapped { cli_sid: String, app_sid: String },
    /// Watcher health, sent on change and every 30 s.
    Health(CoworkHealth),
}

/// `Some(reason)` = that watcher cannot currently see Cowork.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoworkHealth {
    pub toasts: Option<String>,
    pub log: Option<String>,
}

impl CoworkHealth {
    /// The popover header line; empty when both watchers are fine.
    pub fn summary(&self) -> String {
        match (&self.toasts, &self.log) {
            (None, None) => String::new(),
            (Some(t), None) => t.clone(),
            (None, Some(l)) => l.clone(),
            (Some(t), Some(l)) => format!("{t}; {l}"),
        }
    }
}

/// `cowork` block of config.json. Absent = defaults, so existing configs
/// deserialize unchanged. The path overrides exist for the blindness
/// drills (a bogus log path, a copied store with a renamed column).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct CoworkConfig {
    pub enabled: bool,
    pub db_path: Option<String>,
    pub log_path: Option<String>,
    /// opens `POST /cowork-event` (synthetic events from loopback); off by
    /// default because it lets any local process puppet the pet
    pub debug_injection: bool,
}

impl Default for CoworkConfig {
    fn default() -> Self {
        CoworkConfig { enabled: true, db_path: None, log_path: None, debug_injection: false }
    }
}

/// Shared health record: each watcher owns one field, every change sends
/// the whole struct so the reducer never has to merge.
#[derive(Clone)]
pub struct HealthSlot {
    inner: Arc<Mutex<CoworkHealth>>,
    tx: Sender<PetEvent>,
}

impl HealthSlot {
    fn new(tx: Sender<PetEvent>) -> Self {
        HealthSlot { inner: Arc::new(Mutex::new(CoworkHealth::default())), tx }
    }

    /// Set the toast watcher's verdict; emits only on change.
    pub fn set_toasts(&self, v: Option<String>) {
        let mut h = self.inner.lock_or_recover();
        if h.toasts != v {
            h.toasts = v;
            let snap = h.clone();
            drop(h);
            let _ = self.tx.send(PetEvent::Cowork(CoworkEvent::Health(snap)));
        }
    }

    /// Set the log tailer's verdict; emits only on change.
    pub fn set_log(&self, v: Option<String>) {
        let mut h = self.inner.lock_or_recover();
        if h.log != v {
            h.log = v;
            let snap = h.clone();
            drop(h);
            let _ = self.tx.send(PetEvent::Cowork(CoworkEvent::Health(snap)));
        }
    }

    /// Periodic re-send (the reducer treats a stale health as fine only
    /// while it keeps hearing it).
    pub fn resend(&self) {
        let snap = self.inner.lock_or_recover().clone();
        let _ = self.tx.send(PetEvent::Cowork(CoworkEvent::Health(snap)));
    }
}

/// Start the Cowork watchers. A disabled config starts nothing and sends
/// nothing: the popover then simply has no Cowork rows and no health line.
pub fn spawn(tx: Sender<PetEvent>, cfg: &CoworkConfig) {
    if !cfg.enabled {
        return;
    }
    let health = HealthSlot::new(tx.clone());
    let db_path = cfg
        .db_path
        .as_ref()
        .map(std::path::PathBuf::from)
        .or_else(toasts::default_db_path);
    match db_path {
        Some(p) => toasts::spawn(tx.clone(), p, health.clone()),
        None => health.set_toasts(Some("notification store path unknown (no LOCALAPPDATA)".into())),
    }
    let log_path = cfg
        .log_path
        .as_ref()
        .map(std::path::PathBuf::from)
        .or_else(log::default_log_path);
    match log_path {
        Some(p) => log::spawn(tx, p, health),
        None => health.set_log(Some("desktop log path unknown (no LOCALAPPDATA)".into())),
    }
}
