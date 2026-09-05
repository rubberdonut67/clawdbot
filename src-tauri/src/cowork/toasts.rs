// Notification-store watcher: a 1 s read-only poll of the desktop app's
// rows in Windows' Action Center database, diffed by row id into
// ToastAdded / ToastGone events. Facts this rests on (measured on this
// machine, build 26200, app 1.46388.3):
//
// - `wpndatabase.db` is SQLite in WAL mode; opening with URI `mode=ro`
//   while the platform writes works and sees fresh WAL rows. `immutable=1`
//   would NOT (it ignores the WAL), so it is never used.
// - The app's handler is `PrimaryId = 'Claude_<pfn>!Claude'`. The Phone
//   Link relay handler (`Microsoft.YourPhone_…!…com.anthropic.claude`)
//   carries mobile-app toasts and must not match.
// - `toast:maxCount = 20`: the 21st toast does not delete the oldest row,
//   it flips its Type to `toastCondensed` and blanks the payload (no
//   `<text>` left). Condensed rows live on until their 3-day ExpiryTime
//   (or the 80-row condensed cap). So a DELETED row means the app closed
//   it (answered / opened / dismissed) unless it had expired or was the
//   oldest of a full class — those are `evicted`.
// - The platform keeps its own per-app heartbeat in the registry
//   (`LastNotificationAddedTime`, FILETIME): if it advances past the newest
//   row this watcher has seen, the watcher is deaf, and says so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

use super::{CoworkEvent, HealthSlot};
use crate::state::PetEvent;

const POLL: Duration = Duration::from_secs(1);
/// Cloud Cowork rows younger than this at startup are replayed as
/// ToastAdded (flagged `seeded`: the reducer makes idle rows of them, with
/// the ask text as detail, never a hop — a row survives its answer, so
/// its presence proves nothing). Local (`session-local_`) rows are never replayed: those
/// sessions are hook-tracked, and at launch their hook twins are gone, so
/// a replay would resurrect every answered ask of the day as a Cowork row
/// (seen live 2026-09-05: seven stale asks, pet hopping at startup).
const SEED_MAX: Duration = Duration::from_secs(2 * 60 * 60);
/// query/open failures shorter than this are retried silently
const UNHEALTHY_AFTER: Duration = Duration::from_secs(15);
const HEARTBEAT_CADENCE: Duration = Duration::from_secs(30);
/// the registry heartbeat may run ahead of the row by a poll or two
const HEARTBEAT_SLACK: Duration = Duration::from_secs(15);
/// per-class caps, overridden by the store's own Metadata when readable
const TOAST_CAP_DEFAULT: usize = 20;
const CONDENSED_CAP_DEFAULT: usize = 80;
/// the retry ladder after a failure (seconds)
const BACKOFF: [u64; 3] = [1, 2, 5];

const HANDLER_PATTERN: &str = r"Claude\_%!Claude";
const REG_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";
const REG_PUSH: &str = r"Software\Microsoft\Windows\CurrentVersion\PushNotifications";

/// FILETIME epoch (1601-01-01) to Unix epoch, in 100 ns ticks
const FT_UNIX_OFFSET: i64 = 116_444_736_000_000_000;

pub fn default_db_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join(r"Microsoft\Windows\Notifications\wpndatabase.db"))
}

fn ft_to_system(ft: i64) -> SystemTime {
    let unix_100ns = ft - FT_UNIX_OFFSET;
    if unix_100ns <= 0 {
        return UNIX_EPOCH;
    }
    UNIX_EPOCH + Duration::from_micros((unix_100ns / 10) as u64)
}

fn system_to_ft(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_micros() as i64) * 10 + FT_UNIX_OFFSET,
        Err(_) => FT_UNIX_OFFSET,
    }
}

// --- rows and the differ (pure; unit-tested on an in-memory store) ---

#[derive(Debug, Clone, PartialEq)]
struct Row {
    id: i64,
    condensed: bool,
    tag: String,
    group: String,
    arrival: i64,
    expiry: i64,
    payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Seen {
    condensed: bool,
    arrival: i64,
    expiry: i64,
}

type Snapshot = BTreeMap<i64, Seen>;

#[derive(Debug, Clone, Copy)]
struct Caps {
    toast: usize,
    condensed: usize,
}

/// A row that belongs to a cloud Cowork session (tag or group says so).
fn cloud_row(tag: &str, group: &str) -> bool {
    tag.starts_with("cowork-") || group.starts_with("session-cse_") || group.starts_with("session-session_")
}

/// Diff the previous snapshot against the rows just read. `seed_cutoff`
/// (first poll only) suppresses ToastAdded for rows older than it and for
/// every non-cloud row — they are recorded, so their later disappearance
/// is ignored too.
fn diff(
    prev: &Snapshot,
    cur: &[Row],
    now_ft: i64,
    caps: Caps,
    seed_cutoff: Option<i64>,
) -> (Snapshot, Vec<CoworkEvent>) {
    let mut next = Snapshot::new();
    let mut events = Vec::new();

    for r in cur {
        next.insert(r.id, Seen { condensed: r.condensed, arrival: r.arrival, expiry: r.expiry });
        if prev.contains_key(&r.id) {
            // a toast→condensed flip is cap eviction: the row (and the
            // ask it stands for) is still there — nothing to report
            continue;
        }
        if seed_cutoff.map_or(false, |c| r.arrival < c || !cloud_row(&r.tag, &r.group)) {
            continue;
        }
        let (title, body) = if r.condensed { (String::new(), String::new()) } else { parse_texts(&r.payload) };
        events.push(CoworkEvent::ToastAdded {
            row: r.id,
            tag: r.tag.clone(),
            group: r.group.clone(),
            title,
            body,
            arrival: ft_to_system(r.arrival),
            seeded: seed_cutoff.is_some(),
        });
    }

    // the oldest row of a class that sat at its cap may have been pushed
    // out by the platform rather than closed by the app
    let class_count = |condensed: bool| prev.values().filter(|s| s.condensed == condensed).count();
    let oldest_of = |condensed: bool| {
        prev.iter()
            .filter(|(_, s)| s.condensed == condensed)
            .min_by_key(|(_, s)| s.arrival)
            .map(|(id, _)| *id)
    };
    let full_toast = class_count(false) >= caps.toast;
    let full_condensed = class_count(true) >= caps.condensed;
    let oldest_toast = oldest_of(false);
    let oldest_condensed = oldest_of(true);

    for (id, seen) in prev {
        if next.contains_key(id) {
            continue;
        }
        let expired = seen.expiry > 0 && now_ft >= seen.expiry;
        let capped = if seen.condensed {
            full_condensed && oldest_condensed == Some(*id)
        } else {
            full_toast && oldest_toast == Some(*id)
        };
        events.push(CoworkEvent::ToastGone { row: *id, evicted: expired || capped });
    }

    (next, events)
}

/// First two `<text>` elements of the ToastGeneric binding, entity-decoded.
/// No XML crate: the payload is generated by one known Electron call and
/// the two-element shape has held across every captured row.
pub fn parse_texts(payload: &str) -> (String, String) {
    let scope = match payload.find("<binding") {
        Some(i) => &payload[i..],
        None => payload,
    };
    let scope = match scope.find("</binding>") {
        Some(i) => &scope[..i],
        None => scope,
    };
    let mut texts = Vec::with_capacity(2);
    let mut rest = scope;
    while texts.len() < 2 {
        let Some(start) = rest.find("<text") else { break };
        let after_tag = &rest[start..];
        let Some(gt) = after_tag.find('>') else { break };
        // a self-closing <text/> carries nothing
        if after_tag[..gt].ends_with('/') {
            texts.push(String::new());
            rest = &after_tag[gt + 1..];
            continue;
        }
        let inner = &after_tag[gt + 1..];
        let Some(end) = inner.find("</text>") else { break };
        texts.push(decode_entities(inner[..end].trim()));
        rest = &inner[end + 7..];
    }
    let mut it = texts.into_iter();
    (it.next().unwrap_or_default(), it.next().unwrap_or_default())
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let ent = &tail[1..semi];
        let decoded = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]), // unknown entity: keep verbatim
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

// --- the store ---

const NOTIFICATION_COLS: [&str; 8] =
    ["Id", "HandlerId", "Type", "Payload", "Tag", "Group", "ArrivalTime", "ExpiryTime"];
const HANDLER_COLS: [&str; 2] = ["RecordId", "PrimaryId"];

/// `file:///C:/Users/…/wpndatabase.db` — URI form so `mode=ro` applies.
fn db_uri(path: &Path) -> String {
    let mut s = String::from("file:///");
    for ch in path.to_string_lossy().chars() {
        match ch {
            '\\' => s.push('/'),
            '%' => s.push_str("%25"),
            '?' => s.push_str("%3F"),
            '#' => s.push_str("%23"),
            ' ' => s.push_str("%20"),
            c => s.push(c),
        }
    }
    s.push_str("?mode=ro");
    s
}

fn open(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        db_uri(path),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("notification store unreadable: {e}"))?;
    validate_schema(&conn)?;
    Ok(conn)
}

fn columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| format!("notification store unreadable: {e}"))?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("notification store unreadable: {e}"))?
        .filter_map(Result::ok)
        .collect();
    Ok(cols)
}

fn validate_schema(conn: &Connection) -> Result<(), String> {
    for (table, needed) in [("Notification", &NOTIFICATION_COLS[..]), ("NotificationHandler", &HANDLER_COLS[..])] {
        let have = columns(conn, table)?;
        if let Some(missing) = needed.iter().find(|c| !have.iter().any(|h| h.eq_ignore_ascii_case(c))) {
            return Err(format!("notification store schema changed ({table}.{missing} missing)"));
        }
    }
    Ok(())
}

fn read_caps(conn: &Connection) -> Caps {
    let mut caps = Caps { toast: TOAST_CAP_DEFAULT, condensed: CONDENSED_CAP_DEFAULT };
    let get = |key: &str| -> Option<usize> {
        conn.query_row("SELECT Value FROM Metadata WHERE Key = ?1", [key], |r| r.get::<_, i64>(0))
            .ok()
            .filter(|v| *v > 0)
            .map(|v| v as usize)
    };
    if let Some(v) = get("toast:maxCount") {
        caps.toast = v;
    }
    if let Some(v) = get("toastCondensed:maxCount") {
        caps.condensed = v;
    }
    caps
}

/// The app's AUMID as the store spells it (registry key name for the
/// heartbeat); None until the handler row exists.
fn handler_aumid(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT PrimaryId FROM NotificationHandler WHERE PrimaryId LIKE ?1 ESCAPE '\\' LIMIT 1",
        [HANDLER_PATTERN],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

fn poll(conn: &Connection) -> Result<Vec<Row>, String> {
    let mut stmt = conn
        .prepare_cached(
            r#"SELECT n.Id, n.Type, n.Tag, n."Group", n.ArrivalTime, n.ExpiryTime, n.Payload
               FROM Notification n JOIN NotificationHandler h ON n.HandlerId = h.RecordId
               WHERE h.PrimaryId LIKE ?1 ESCAPE '\' ORDER BY n.Id"#,
        )
        .map_err(|e| format!("notification store unreadable: {e}"))?;
    let rows = stmt
        .query_map([HANDLER_PATTERN], |r| {
            let ty: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
            let payload: Option<Vec<u8>> = r.get(6)?;
            Ok(Row {
                id: r.get(0)?,
                condensed: !ty.eq_ignore_ascii_case("toast"),
                tag: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                group: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                arrival: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                expiry: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                payload: payload.map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default(),
            })
        })
        .map_err(|e| format!("notification store unreadable: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("notification store unreadable: {e}"))?;
    Ok(rows)
}

// --- the platform's own heartbeat (registry) ---

fn reg_u64(subkey: &str, value: &str) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_QWORD};
    let key: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let val: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = [0u8; 8];
    let mut len: u32 = buf.len() as u32;
    // SAFETY: plain RegGetValueW into a stack buffer whose length is passed
    // along; both strings are NUL-terminated UTF-16 that outlive the call
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_DWORD | RRF_RT_REG_QWORD,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    };
    if status.is_err() {
        return None;
    }
    match len {
        4 => Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64),
        8 => Some(u64::from_le_bytes(buf)),
        _ => None,
    }
}

/// Some(reason) when Windows says Claude's toasts are off or it counted a
/// notification this watcher never saw.
fn heartbeat_check(aumid: Option<&str>, newest_seen_ft: i64, started_ft: i64) -> Option<String> {
    if reg_u64(REG_PUSH, "ToastEnabled") == Some(0) {
        return Some("Windows notifications are off".into());
    }
    let aumid = aumid?;
    let key = format!(r"{REG_SETTINGS}\{aumid}");
    if reg_u64(&key, "Enabled") == Some(0) {
        return Some("Claude notifications are off in Windows".into());
    }
    let last_added = reg_u64(&key, "LastNotificationAddedTime")? as i64;
    let slack = HEARTBEAT_SLACK.as_micros() as i64 * 10;
    // only notifications added since we started can be "missed"; the
    // verdict clears by itself once a newer row is seen
    if last_added > started_ft && last_added > newest_seen_ft + slack {
        return Some("toast watcher missed a notification".into());
    }
    None
}

// --- the thread ---

pub fn spawn(tx: Sender<PetEvent>, path: PathBuf, health: HealthSlot) {
    let spawned = std::thread::Builder::new()
        .name("cowork-toasts".into())
        .spawn(move || run(tx, path, health));
    if let Err(e) = spawned {
        eprintln!("clawdbot: toast watcher thread spawn failed ({e})");
    }
}

fn run(tx: Sender<PetEvent>, path: PathBuf, health: HealthSlot) {
    let started_ft = system_to_ft(SystemTime::now());
    let mut conn: Option<Connection> = None;
    let mut caps = Caps { toast: TOAST_CAP_DEFAULT, condensed: CONDENSED_CAP_DEFAULT };
    let mut aumid: Option<String> = None;
    let mut snapshot = Snapshot::new();
    let mut seeded = false;
    let mut newest_seen_ft: i64 = 0;
    let mut failing_since: Option<Instant> = None;
    let mut failures: usize = 0;
    let mut last_heartbeat = Instant::now();
    let mut store_reason: Option<String> = None;
    let mut heartbeat_reason: Option<String> = None;

    loop {
        // (re)open
        if conn.is_none() {
            match open(&path) {
                Ok(c) => {
                    caps = read_caps(&c);
                    aumid = handler_aumid(&c);
                    conn = Some(c);
                }
                Err(e) => store_reason = Some(e),
            }
        }

        // poll + diff
        let mut ok = false;
        if let Some(c) = conn.as_ref() {
            match poll(c) {
                Ok(rows) => {
                    ok = true;
                    if aumid.is_none() {
                        aumid = handler_aumid(c);
                    }
                    let now_ft = system_to_ft(SystemTime::now());
                    let seed_cutoff = if seeded { None } else { Some(now_ft - SEED_MAX.as_micros() as i64 * 10) };
                    let (next, events) = diff(&snapshot, &rows, now_ft, caps, seed_cutoff);
                    seeded = true;
                    snapshot = next;
                    if let Some(m) = rows.iter().map(|r| r.arrival).max() {
                        newest_seen_ft = newest_seen_ft.max(m);
                    }
                    for ev in events {
                        if tx.send(PetEvent::Cowork(ev)).is_err() {
                            return; // receiver gone = app shutting down
                        }
                    }
                }
                Err(e) => {
                    store_reason = Some(e);
                    conn = None; // reopen next round (rotation, schema, locks)
                }
            }
        }

        if ok {
            failing_since = None;
            failures = 0;
            store_reason = None;
        } else {
            failures += 1;
            let since = *failing_since.get_or_insert_with(Instant::now);
            if since.elapsed() < UNHEALTHY_AFTER {
                store_reason = None; // still inside the silent-retry window
            }
        }

        // the platform's own heartbeat, every 30 s
        if last_heartbeat.elapsed() >= HEARTBEAT_CADENCE {
            last_heartbeat = Instant::now();
            heartbeat_reason = heartbeat_check(aumid.as_deref(), newest_seen_ft, started_ft);
            health.resend();
        }

        health.set_toasts(store_reason.clone().or_else(|| heartbeat_reason.clone()));

        let sleep = if ok { POLL } else { Duration::from_secs(BACKOFF[failures.min(BACKOFF.len()) - 1]) };
        std::thread::sleep(sleep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOAST: &str = r#"<toast>
 <visual>
  <binding template="ToastGeneric">
   <text>Example app</text>
   <text>Allow Claude to Run &quot;cargo build&quot; &amp; more &#8230; &lt;now&gt;?</text>
  </binding>
 </visual>
 <actions><action content="Allow once" arguments="allow"/></actions>
 <audio silent="true"/>
</toast>"#;

    #[test]
    fn parses_two_texts_with_entities_and_actions() {
        let (t, b) = parse_texts(TOAST);
        assert_eq!(t, "Example app");
        assert_eq!(b, "Allow Claude to Run \"cargo build\" & more \u{2026} <now>?");
    }

    #[test]
    fn parses_missing_and_empty_texts() {
        assert_eq!(parse_texts(""), (String::new(), String::new()));
        assert_eq!(parse_texts("<toast><visual><binding template=\"ToastGeneric\"><text>Only</text></binding></visual></toast>"),
                   ("Only".into(), String::new()));
        assert_eq!(parse_texts("<binding><text/><text>Body</text></binding>"), (String::new(), "Body".into()));
        // unknown entity kept verbatim, stray ampersand kept
        assert_eq!(decode_entities("a &nbsp; b & c"), "a &nbsp; b & c");
        assert_eq!(decode_entities("&#x41;&#66;"), "AB");
    }

    #[test]
    fn filetime_roundtrip() {
        let now = SystemTime::now();
        let back = ft_to_system(system_to_ft(now));
        let delta = now.duration_since(back).unwrap_or_else(|e| e.duration());
        assert!(delta < Duration::from_millis(1));
    }

    fn row(id: i64, condensed: bool, tag: &str, arrival: i64, expiry: i64) -> Row {
        Row {
            id,
            condensed,
            tag: tag.into(),
            group: format!("session-{tag}"),
            arrival,
            expiry,
            payload: if condensed { String::new() } else { TOAST.into() },
        }
    }

    fn caps() -> Caps {
        Caps { toast: 3, condensed: 4 }
    }

    #[test]
    fn seed_replays_only_fresh_cloud_rows_but_records_all() {
        let mut local = row(3, false, "permission-x", 550, 900);
        local.group = "session-local_abc".into();
        let rows = vec![row(1, false, "cowork-idle-cse_old", 100, 900), row(2, false, "cowork-awaiting-cse_fresh", 500, 900), local];
        let caps = Caps { toast: 10, condensed: 10 }; // below the cap: deletions are answers
        let (snap, ev) = diff(&Snapshot::new(), &rows, 600, caps, Some(300));
        assert_eq!(snap.len(), 3);
        assert_eq!(ev.len(), 1, "old cloud row and fresh LOCAL row are both silent at seed");
        match &ev[0] {
            CoworkEvent::ToastAdded { row, tag, title, body, seeded, .. } => {
                assert!(*seeded);
                assert_eq!(*row, 2);
                assert_eq!(tag, "cowork-awaiting-cse_fresh");
                assert_eq!(title, "Example app");
                assert!(body.starts_with("Allow Claude"));
            }
            other => panic!("{other:?}"),
        }
        // the old row vanishing later is a plain ToastGone (not evicted) —
        // apply_cowork ignores it because it never saw the ToastAdded
        let (snap2, ev) = diff(&snap, &rows[1..], 601, caps, None);
        assert!(matches!(ev[..], [CoworkEvent::ToastGone { row: 1, evicted: false }]));
        // after the seed, a new local row IS reported (the reducer dedupes it)
        let mut fresh_local = row(4, false, "idle-local_zzz", 650, 900);
        fresh_local.group = "session-local_zzz".into();
        let mut cur: Vec<Row> = rows[1..].to_vec();
        cur.push(fresh_local);
        let (_, ev) = diff(&snap2, &cur, 700, caps, None);
        assert!(matches!(&ev[..], [CoworkEvent::ToastAdded { row: 4, seeded: false, .. }]));
    }

    #[test]
    fn added_condensed_row_has_empty_texts() {
        let rows = vec![row(7, true, "cowork-idle-cse_X", 500, 900)];
        let (_, ev) = diff(&Snapshot::new(), &rows, 600, caps(), None);
        match &ev[0] {
            CoworkEvent::ToastAdded { tag, title, body, .. } => {
                assert_eq!(tag, "cowork-idle-cse_X");
                assert!(title.is_empty() && body.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn toast_to_condensed_flip_is_silent_and_deletion_is_an_answer() {
        let a = vec![row(1, false, "a", 100, 900)];
        let (s1, _) = diff(&Snapshot::new(), &a, 200, caps(), None);
        let flipped = vec![row(1, true, "a", 100, 900)];
        let (s2, ev) = diff(&s1, &flipped, 300, caps(), None);
        assert!(ev.is_empty());
        assert!(s2[&1].condensed);
        let (_, ev) = diff(&s2, &[], 400, caps(), None);
        assert!(matches!(ev[..], [CoworkEvent::ToastGone { row: 1, evicted: false }]));
    }

    #[test]
    fn expired_row_is_evicted() {
        let a = vec![row(1, false, "a", 100, 350)];
        let (s1, _) = diff(&Snapshot::new(), &a, 200, caps(), None);
        let (_, ev) = diff(&s1, &[], 400, caps(), None);
        assert!(matches!(ev[..], [CoworkEvent::ToastGone { row: 1, evicted: true }]));
    }

    #[test]
    fn oldest_of_a_full_class_is_evicted_others_are_answers() {
        // toast cap 3: three fresh rows, then the oldest AND a middle one go
        let rows = vec![row(1, false, "a", 100, 900), row(2, false, "b", 200, 900), row(3, false, "c", 300, 900)];
        let (s1, _) = diff(&Snapshot::new(), &rows, 400, caps(), None);
        let (_, mut ev) = diff(&s1, &rows[2..], 500, caps(), None);
        ev.sort_by_key(|e| match e {
            CoworkEvent::ToastGone { row, .. } => *row,
            _ => 0,
        });
        assert!(matches!(ev[0], CoworkEvent::ToastGone { row: 1, evicted: true }));
        assert!(matches!(ev[1], CoworkEvent::ToastGone { row: 2, evicted: false }));
        // below the cap the oldest going is an answer
        let two = vec![row(1, false, "a", 100, 900), row(2, false, "b", 200, 900)];
        let (s2, _) = diff(&Snapshot::new(), &two, 400, caps(), None);
        let (_, ev) = diff(&s2, &two[1..], 500, caps(), None);
        assert!(matches!(ev[..], [CoworkEvent::ToastGone { row: 1, evicted: false }]));
    }

    /// The real query against an in-memory store with the measured schema:
    /// the Phone Link relay handler must not match, NULL tags/groups are
    /// tolerated, Type decides `condensed`, payload is a BLOB.
    #[test]
    fn poll_filters_handlers_on_the_real_schema() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"CREATE TABLE [NotificationHandler] ( [RecordId] INTEGER PRIMARY KEY, [PrimaryId] TEXT NOT NULL COLLATE NOCASE, [WNSId] TEXT COLLATE NOCASE, [HandlerType] TEXT, [WNFEventName] INT64, [SystemDataPropertySet] BLOB, [CreatedTime] DATETIME, [ModifiedTime] DATETIME, [ParentId] TEXT COLLATE NOCASE, [ContainerSid] TEXT COLLATE NOCASE);
               CREATE TABLE [Notification]( [Order] INTEGER NOT NULL PRIMARY KEY, [Id] INTEGER NOT NULL, [HandlerId] INTEGER, [ActivityId] GUID,[Type] TEXT NOT NULL, [Payload] BLOB, [Tag] TEXT, [Group] TEXT, [ExpiryTime] INT64, [ArrivalTime] INT64, [DataVersion] INT64 DEFAULT '0', [PayloadType] TEXT NOT NULL, [BootId] INT64 DEFAULT '0', [ExpiresOnReboot] BOOLEAN DEFAULT 'FALSE', UNIQUE([Id]) ON CONFLICT REPLACE);
               CREATE TABLE [Metadata]( [Key] TEXT, [Value] INT64, PRIMARY KEY([Key]) ON CONFLICT REPLACE);
               INSERT INTO Metadata VALUES ('toast:maxCount', 20), ('toastCondensed:maxCount', 80);
               INSERT INTO NotificationHandler (RecordId, PrimaryId, HandlerType) VALUES
                 (227, 'Claude_pzs8sxrjxfjjc!Claude', 'app:immersive'),
                 (391, 'Microsoft.YourPhone_8wekyb3d8bbwe!YourPhoneNotifications_com.anthropic.claude', 'app:immersive');
               INSERT INTO Notification ([Order], Id, HandlerId, Type, Payload, Tag, [Group], ExpiryTime, ArrivalTime, PayloadType) VALUES
                 (1, 10, 227, 'toast', CAST('<toast><visual><binding template="ToastGeneric"><text>T</text><text>B</text></binding></visual></toast>' AS BLOB), 'permission-1', 'session-cse_A', 900, 100, 'Xml'),
                 (2, 11, 227, 'toastCondensed', NULL, 'cowork-idle-cse_B', NULL, 900, 200, 'Xml'),
                 (3, 12, 391, 'toast', CAST('<toast><visual><binding template="ToastGeneric"><text>phone</text></binding></visual></toast>' AS BLOB), 'x', 'y', 900, 300, 'Xml');"#,
        )
        .unwrap();
        validate_schema(&c).unwrap();
        let caps = read_caps(&c);
        assert_eq!((caps.toast, caps.condensed), (20, 80));
        assert_eq!(handler_aumid(&c).as_deref(), Some("Claude_pzs8sxrjxfjjc!Claude"));
        let rows = poll(&c).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], Row { id: 10, condensed: false, tag: "permission-1".into(), group: "session-cse_A".into(), arrival: 100, expiry: 900,
            payload: "<toast><visual><binding template=\"ToastGeneric\"><text>T</text><text>B</text></binding></visual></toast>".into() });
        assert_eq!(rows[1], Row { id: 11, condensed: true, tag: "cowork-idle-cse_B".into(), group: String::new(), arrival: 200, expiry: 900, payload: String::new() });
        // schema drift is a named failure
        c.execute_batch("ALTER TABLE Notification RENAME COLUMN Tag TO Tagg").unwrap();
        assert_eq!(validate_schema(&c).unwrap_err(), "notification store schema changed (Notification.Tag missing)");
    }

    #[test]
    fn uri_form() {
        assert_eq!(
            db_uri(Path::new(r"C:\Users\a b\AppData\Local\Microsoft\Windows\Notifications\wpndatabase.db")),
            "file:///C:/Users/a%20b/AppData/Local/Microsoft/Windows/Notifications/wpndatabase.db?mode=ro"
        );
    }
}
