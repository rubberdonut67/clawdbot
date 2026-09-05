// Desktop-log tailer: the "Cowork is working" signal. Cloud Cowork sessions
// run in a container, but every host command they execute is logged by the
// desktop app (measured live 2026-09-05, app 1.46388.4):
//
//   [info] [remote-bash] user=rcw-<cse id, lowercased> mounts=<A>:rwd,<B>:rw cmdLen=<n>
//   [info] [remote-file] listed <n> entries | staged … | committed …      (no session id)
//   [info] LocalAgentModeSessions.grantRemoteSessionFolders: cse_<Id> [<n>]
//   [info] LocalAgentModeSessions.clearRemoteSessionFolderGrants: cse_<Id>
//
// Mount names are the granted folder names (spaces and parentheses
// included), suffixed with a mode (`rw`, `ro`, `rwd` once deletes were
// approved). A folder granted mid-session produces no grant line of its own:
// it simply appears in the next remote-bash mount list. Cloud-only turns
// (thinking, waiting for the user) leave NO line at all, so log-derived
// "working" is a lower bound and decays in the reducer.
//
// `clearRemoteSessionFolderGrants` also fires with `session_<Id>` ids (the
// Remote-Control twin of the same session); only the `cse_` form is used.
//
// One line about LOCAL sessions is parsed too:
//   [info] Mapping internal session local_<uuid> to CLI session <uuid>
// (logged on every message the user sends). It is the only bridge between
// the hook session id and the id the app's Claude Code deep link accepts.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::{CoworkEvent, HealthSlot};
use crate::state::PetEvent;

const POLL: Duration = Duration::from_secs(1);
/// a remote-file line is attributed to the session of the last remote-bash
/// this recent; older than that the reducer gets an empty sid (resolved to
/// the single known cloud session, else dropped)
const FILE_ATTRIBUTION: Duration = Duration::from_secs(120);
const UNHEALTHY_AFTER: Duration = Duration::from_secs(15);
/// one poll's read is capped so a 10 MiB rotation cannot stall the thread
const MAX_READ: u64 = 2 * 1024 * 1024;
/// at startup this much of the log's tail is scanned ONLY for mapping
/// lines, so sessions already running get their deep link at once
const STARTUP_SCAN: u64 = 512 * 1024;

pub fn default_log_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(r"Claude\logs\main.log"))
}

// --- line rules (pure; unit-tested) ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Activity { sid: String, folders: Vec<String> },
    RemoteFile,
    Grant { sid: String, sid_display: String, cleared: bool },
    Mapped { cli_sid: String, app_sid: String },
}

pub fn parse_line(line: &str) -> Option<Line> {
    if let Some(rest) = find_after(line, "[remote-bash] user=rcw-") {
        let id_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let id = &rest[..id_end];
        if id.is_empty() {
            return None;
        }
        let folders = find_after(rest, "mounts=")
            .map(|m| {
                let m = m.find(" cmdLen=").map(|i| &m[..i]).unwrap_or(m).trim_end();
                m.split(',').map(strip_mode).filter(|f| !f.is_empty()).collect()
            })
            .unwrap_or_default();
        return Some(Line::Activity { sid: format!("cse_{}", id.to_ascii_lowercase()), folders });
    }
    if let Some(rest) = find_after(line, "[remote-file] ") {
        if rest.starts_with("staged") || rest.starts_with("committed") || rest.starts_with("listed") {
            return Some(Line::RemoteFile);
        }
        return None;
    }
    if let Some(rest) = find_after(line, "Mapping internal session local_") {
        let app_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let app_id = &rest[..app_end];
        let cli = find_after(&rest[app_end..], " to CLI session ")
            .map(|c| c.trim_end())
            .map(|c| &c[..c.find(char::is_whitespace).unwrap_or(c.len())]);
        return match cli {
            Some(cli) if !app_id.is_empty() && !cli.is_empty() => Some(Line::Mapped {
                cli_sid: cli.to_string(),
                app_sid: format!("local_{app_id}"),
            }),
            _ => None,
        };
    }
    for (needle, cleared) in [
        ("LocalAgentModeSessions.grantRemoteSessionFolders: cse_", false),
        ("LocalAgentModeSessions.clearRemoteSessionFolderGrants: cse_", true),
    ] {
        if let Some(rest) = find_after(line, needle) {
            let id_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let id = &rest[..id_end];
            if id.is_empty() {
                return None;
            }
            return Some(Line::Grant {
                sid: format!("cse_{}", id.to_ascii_lowercase()),
                sid_display: format!("cse_{id}"),
                cleared,
            });
        }
    }
    None
}

fn find_after<'a>(hay: &'a str, needle: &str) -> Option<&'a str> {
    hay.find(needle).map(|i| &hay[i + needle.len()..])
}

/// `Project folder:rwd` -> `Project folder`. The mode is the
/// short alphabetic suffix after the LAST colon; names never end in one.
fn strip_mode(mount: &str) -> String {
    let m = mount.trim();
    match m.rfind(':') {
        Some(i) if m[i + 1..].len() <= 4 && m[i + 1..].chars().all(|c| c.is_ascii_alphabetic()) => {
            m[..i].trim().to_string()
        }
        _ => m.to_string(),
    }
}

// --- the tailer (pure over a path; unit-tested on temp files) ---

pub struct Tailer {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl Tailer {
    /// Starts at the current end of the file (or 0 if it does not exist yet).
    pub fn new(path: PathBuf) -> Self {
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Tailer { path, offset, partial: String::new() }
    }

    /// Complete new lines since the last poll. Size shrinking below the
    /// offset means rotation/truncation: reread from the start.
    pub fn poll(&mut self) -> std::io::Result<Vec<String>> {
        let len = std::fs::metadata(&self.path)?.len();
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        let mut f = std::fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        (&mut f).take(MAX_READ).read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let mut lines: Vec<String> = self.partial.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
        self.partial = lines.pop().unwrap_or_default();
        Ok(lines)
    }
}

// --- the thread ---

pub fn spawn(tx: Sender<PetEvent>, path: PathBuf, health: HealthSlot) {
    let spawned = std::thread::Builder::new()
        .name("cowork-log".into())
        .spawn(move || run(tx, path, health));
    if let Err(e) = spawned {
        eprintln!("clawdbot: log tailer thread spawn failed ({e})");
    }
}

/// Mapping lines from the tail of the log as it is at startup (nothing
/// else is replayed: activity that old is not "working" any more).
fn startup_mappings(path: &Path) -> Vec<CoworkEvent> {
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(STARTUP_SCAN);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(Line::Mapped { cli_sid, app_sid }) = parse_line(line) {
            if seen.insert((cli_sid.clone(), app_sid.clone())) {
                out.push(CoworkEvent::Mapped { cli_sid, app_sid });
            }
        }
    }
    out
}

fn run(tx: Sender<PetEvent>, path: PathBuf, health: HealthSlot) {
    let mut tailer = Tailer::new(path.clone());
    for ev in startup_mappings(&path) {
        if tx.send(PetEvent::Cowork(ev)).is_err() {
            return;
        }
    }
    let mut failing_since: Option<Instant> = None;
    let mut last_bash: Option<(String, Instant)> = None;
    loop {
        match tailer.poll() {
            Ok(lines) => {
                failing_since = None;
                health.set_log(None);
                for line in lines {
                    let ev = match parse_line(&line) {
                        Some(Line::Activity { sid, folders }) => {
                            last_bash = Some((sid.clone(), Instant::now()));
                            CoworkEvent::Activity { sid, folders }
                        }
                        Some(Line::RemoteFile) => {
                            let sid = match &last_bash {
                                Some((sid, at)) if at.elapsed() < FILE_ATTRIBUTION => sid.clone(),
                                _ => String::new(),
                            };
                            CoworkEvent::Activity { sid, folders: Vec::new() }
                        }
                        Some(Line::Grant { sid, sid_display, cleared }) => {
                            CoworkEvent::Grant { sid, sid_display, cleared }
                        }
                        Some(Line::Mapped { cli_sid, app_sid }) => CoworkEvent::Mapped { cli_sid, app_sid },
                        None => continue,
                    };
                    if tx.send(PetEvent::Cowork(ev)).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let since = *failing_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= UNHEALTHY_AFTER {
                    health.set_log(Some(format!("desktop log missing ({}: {e})", short_path(&path))));
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

fn short_path(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_bash_with_multi_mounts_and_modes() {
        let l = "2026-09-05 00:08:27 [info] [remote-bash] user=rcw-01examplesessionidxxxxxx mounts=Project folder:rwd,Second folder (notes):rw cmdLen=253";
        assert_eq!(
            parse_line(l),
            Some(Line::Activity {
                sid: "cse_01examplesessionidxxxxxx".into(),
                folders: vec!["Project folder".into(), "Second folder (notes)".into()],
            })
        );
        let l = "2026-09-04 23:55:16 [info] [remote-bash] user=rcw-01oldersessionidxxxxxxxx mounts=Downloads:rw cmdLen=256";
        assert_eq!(parse_line(l), Some(Line::Activity { sid: "cse_01oldersessionidxxxxxxxx".into(), folders: vec!["Downloads".into()] }));
        // the egress line right after it is not activity
        assert_eq!(parse_line("2026-09-04 23:55:16 [info] [remote-bash] egress source=bridge_stamp hosts=9"), None);
    }

    #[test]
    fn parses_remote_file_and_grants() {
        assert_eq!(parse_line("2026-09-05 00:08:24 [info] [remote-file] listed 6 entries"), Some(Line::RemoteFile));
        assert_eq!(parse_line("[info] [remote-file] staged 3/3 files (120 bytes; 0 files-api 429s retried)"), Some(Line::RemoteFile));
        assert_eq!(parse_line("[info] [remote-file] committed 3 files, 0 rejected"), Some(Line::RemoteFile));
        assert_eq!(parse_line("[info] [remote-file] something else"), None);
        assert_eq!(
            parse_line("2026-09-04 23:57:24 [info] LocalAgentModeSessions.grantRemoteSessionFolders: cse_01EXAMPLEsessionIDxxxxxx [1]"),
            Some(Line::Grant { sid: "cse_01examplesessionidxxxxxx".into(), sid_display: "cse_01EXAMPLEsessionIDxxxxxx".into(), cleared: false })
        );
        assert_eq!(
            parse_line("2026-09-04 23:56:20 [info] LocalAgentModeSessions.clearRemoteSessionFolderGrants: cse_01OLDERsessionIDxxxxxxxx"),
            Some(Line::Grant { sid: "cse_01oldersessionidxxxxxxxx".into(), sid_display: "cse_01OLDERsessionIDxxxxxxxx".into(), cleared: true })
        );
        // the Remote-Control twin id is ignored
        assert_eq!(parse_line("[info] LocalAgentModeSessions.clearRemoteSessionFolderGrants: session_01OLDERsessionIDxxxxxxxx"), None);
        // bridge chatter is ignored
        assert_eq!(parse_line("[info] [remote-tools-device] served get_device_info"), None);
        assert_eq!(parse_line("[info] [sessions-bridge] cse_01X connected"), None);
    }

    #[test]
    fn parses_session_mapping_lines() {
        assert_eq!(
            parse_line("2026-09-05 01:51:27 [info] Mapping internal session local_11111111-2222-4333-8444-555555555555 to CLI session 01234567-89ab-4cde-8f01-23456789abcd"),
            Some(Line::Mapped { cli_sid: "01234567-89ab-4cde-8f01-23456789abcd".into(), app_sid: "local_11111111-2222-4333-8444-555555555555".into() })
        );
        assert_eq!(parse_line("[info] Mapping internal session local_abc to nothing"), None);
        assert_eq!(parse_line("[info] Restore: Skipping local_abc — live session already in memory"), None);
    }

    #[test]
    fn startup_scan_collects_unique_mappings_from_the_tail() {
        let p = temp("scan");
        let mut body = String::new();
        for i in 0..3 {
            body.push_str(&format!("[info] Mapping internal session local_aaa to CLI session cli-{i}\n"));
            body.push_str("[info] Mapping internal session local_aaa to CLI session cli-0\n");
            body.push_str("[info] [remote-bash] user=rcw-x mounts=F:rw cmdLen=1\n");
        }
        std::fs::write(&p, body).unwrap();
        let evs = startup_mappings(&p);
        assert_eq!(evs.len(), 3, "activity is not replayed, duplicates collapse");
        assert!(matches!(&evs[0], CoworkEvent::Mapped { cli_sid, app_sid } if cli_sid == "cli-0" && app_sid == "local_aaa"));
        let _ = std::fs::remove_file(&p);
        assert!(startup_mappings(Path::new("C:/nonexistent/main.log")).is_empty());
    }

    #[test]
    fn strip_mode_cases() {
        assert_eq!(strip_mode("Downloads:rw"), "Downloads");
        assert_eq!(strip_mode("a b (c):rwd"), "a b (c)");
        assert_eq!(strip_mode("C:notes:ro"), "C:notes");
        assert_eq!(strip_mode("plain"), "plain");
    }

    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("clawdbot-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn tailer_starts_at_eof_and_handles_partials_and_truncation() {
        let p = temp("tail");
        std::fs::write(&p, "old line 1\nold line 2\n").unwrap();
        let mut t = Tailer::new(p.clone());
        assert!(t.poll().unwrap().is_empty(), "history must not replay");

        // a partial line stays buffered until its newline arrives
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        use std::io::Write;
        write!(f, "new line A\r\nnew li").unwrap();
        assert_eq!(t.poll().unwrap(), vec!["new line A".to_string()]);
        write!(f, "ne B\n").unwrap();
        assert_eq!(t.poll().unwrap(), vec!["new line B".to_string()]);
        assert!(t.poll().unwrap().is_empty());
        drop(f);

        // rotation: the file is truncated and restarted -> reread from 0
        std::fs::write(&p, "fresh 1\n").unwrap();
        assert_eq!(t.poll().unwrap(), vec!["fresh 1".to_string()]);

        // missing file is an error the thread turns into health
        std::fs::remove_file(&p).unwrap();
        assert!(t.poll().is_err());
    }
}
