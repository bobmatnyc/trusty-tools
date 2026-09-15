//! On-disk record of a `SubagentStop` the hook could not deliver (#6556).
//!
//! Why: `tm hook` is the only process that ever learns a subagent stopped, and
//! it learns it once. When the daemon cannot answer, every POST attempt fails
//! and the stop is gone — the delegation stays `Running` until the six-hour
//! `RUNNING_STALE_AFTER_SECS` sweep, holding a builder slot and a checkout the
//! whole time. Retrying inside the hook's five-second budget covers a blip; the
//! park covers the longer outage. So the hook writes the undelivered stop where
//! the reclaim can find it, and the daemon's own reap loop replays it.
//!
//! **The case this recovers is a WEDGED-BUT-ALIVE daemon, not one that is down**
//! (critic MEDIUM on PR #8052). Delegations live in memory only, so a daemon
//! that restarts loses the very records a replay would terminalize, and
//! `stop_spool_drain::replay_target` correctly discards a stop no live record
//! answers. A park therefore buys back the minutes a daemon spends unable to
//! serve `POST /hooks` while keeping its map — a saturated request queue, a
//! blocked lock — which is the window the six-hour leak was actually opening in.
//!
//! What: one JSON file per undelivered stop under
//! `<framework-root>/unposted-stops/`, holding the exact `POST /hooks` body the
//! hook could not deliver. Written temp-then-rename so a concurrently draining
//! daemon reads a whole file or none. [`MAX_UNPOSTED_STOPS`] bounds the
//! directory: a daemon down for a week must not fill the disk.
//!
//! Readers: `crate::daemon::services::stop_spool_drain::drain_unposted_stops`,
//! called from the daemon's `reap_loop`.
//!
//! Test: `stop_spool_tests` below; the drain's own suite in
//! `daemon::services::stop_spool_drain`.

use std::path::{Path, PathBuf};

/// Directory, relative to the framework root, holding undelivered stops.
///
/// Why: spelled once so the writing hook and the draining daemon cannot
/// disagree about where the records are. They are separate processes with no
/// other channel — that is the whole point of the file.
pub const UNPOSTED_STOPS_DIR: &str = "unposted-stops";

/// Ceiling on undelivered stop records kept on disk.
///
/// Why: the writer runs when the daemon is unreachable, which is exactly when
/// nothing is draining. Unbounded, a daemon left down would let every hook
/// invocation add a file forever. At the cap the oldest records are the ones
/// worth keeping (they name the delegations that have been stuck longest), so a
/// write over the cap is DROPPED rather than evicting one.
/// Test: `a_full_spool_refuses_further_records`.
pub const MAX_UNPOSTED_STOPS: usize = 256;

/// Where undelivered stop records live under `root`.
///
/// Test: `the_spool_directory_is_under_the_framework_root`.
#[must_use]
pub fn unposted_stops_dir(root: &Path) -> PathBuf {
    root.join(UNPOSTED_STOPS_DIR)
}

/// Record one undelivered `POST /hooks` body under `root`.
///
/// Why: see the module header — this is the hand-off from a hook process that
/// could not reach the daemon to the daemon that will come back.
/// What: writes `body` as one JSON object to
/// `<root>/unposted-stops/<millis>-<pid>-<seq>.json` via a temp file renamed
/// into place. Returns the path written, or `None` when the directory is at
/// [`MAX_UNPOSTED_STOPS`] or any filesystem step fails — the caller logs either
/// way, and a hook must never fail on a bookkeeping write.
/// Test: `a_recorded_stop_reads_back_verbatim`,
/// `a_full_spool_refuses_further_records`.
#[must_use]
pub fn record_unposted_stop(root: &Path, body: &serde_json::Value) -> Option<PathBuf> {
    use std::io::Write as _;

    let dir = unposted_stops_dir(root);
    std::fs::create_dir_all(&dir).ok()?;
    if count_records(&dir) >= MAX_UNPOSTED_STOPS {
        return None;
    }
    let path = dir.join(record_file_name());
    let line = serde_json::to_string(body).ok()?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).ok()?;
    tmp.write_all(line.as_bytes()).ok()?;
    // On failure `PersistError::Drop` removes the temp file.
    tmp.persist(&path).ok()?;
    Some(path)
}

/// A file name no concurrent writer — in this process or another — will pick.
///
/// What: millisecond timestamp, process id, and a per-process counter, so the
/// name is unique across processes and across threads within one, and sorts
/// oldest-first for a drain that wants to replay in arrival order.
fn record_file_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let millis = chrono::Utc::now().timestamp_millis();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{millis:013}-{}-{seq}.json", std::process::id())
}

/// How many `.json` records `dir` currently holds.
///
/// What: an unreadable directory counts as zero — a spool that cannot be listed
/// must not silently refuse every write on top of already failing to drain.
fn count_records(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries.flatten().filter(|e| is_record(&e.path())).count()
}

/// Is this path one of our records, rather than a stray temp file?
fn is_record(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "json")
}

/// Every undelivered stop record under `root`, oldest first.
///
/// Why: the drain replays in arrival order, so a stop and a later correction of
/// the same delegation land in the order the hooks produced them.
/// What: reads `<root>/unposted-stops/*.json`, parsing each. A file that does
/// not parse is returned as `(path, None)` so the drain can discard it rather
/// than leave it wedging the directory forever. Sorted by file name, which the
/// [`record_file_name`] scheme makes chronological.
/// Test: `records_read_back_oldest_first`,
/// `an_unparsable_record_is_reported_so_the_drain_can_discard_it`.
#[must_use]
pub fn read_unposted_stops(root: &Path) -> Vec<(PathBuf, Option<serde_json::Value>)> {
    let dir = unposted_stops_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_record(p))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let parsed = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .filter(serde_json::Value::is_object);
            (path, parsed)
        })
        .collect()
}

/// Discard one record the drain has finished with.
///
/// What: removes `path`, reporting whether it is gone. An already-absent file
/// reads as success: the drain's job is that the record no longer replays.
/// Test: `a_discarded_record_does_not_read_back`.
pub fn discard_unposted_stop(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// `tm doctor` row for the undelivered-stop spool (#6556).
///
/// Why: the lossy branch — a spool that cannot be written or is at
/// [`MAX_UNPOSTED_STOPS`] — is the one place a stop is dropped outright, and its
/// only alarm was a stderr line on an exit-0 hook, which Claude Code does not
/// surface (critic MEDIUM on PR #8052). A doctor row is the surface an operator
/// actually reads.
/// What: `Ok` when the directory is absent or empty — nothing parked is the
/// healthy state. `Warn` when records are waiting (the daemon has not drained
/// them, so something is wrong with the reap loop) or when the directory exists
/// but cannot be written. Read-only; it creates nothing.
/// Test: `an_empty_spool_is_an_ok_row`, `parked_records_warn`,
/// `a_saturated_spool_names_the_cap`.
#[must_use]
pub fn check_stop_spool(root: &Path) -> crate::core::doctor::DoctorCheck {
    use crate::core::doctor::{CheckStatus, DoctorCheck};

    let dir = unposted_stops_dir(root);
    if !dir.exists() {
        return DoctorCheck::new(
            "stop_spool",
            CheckStatus::Ok,
            format!("no undelivered SubagentStop records ({})", dir.display()),
        );
    }
    let parked = count_records(&dir);
    let writable = std::fs::metadata(&dir).is_ok_and(|m| !m.permissions().readonly());
    let (status, detail) = match (parked, writable) {
        (0, true) => (
            CheckStatus::Ok,
            format!("no undelivered SubagentStop records ({})", dir.display()),
        ),
        (n, true) if n >= MAX_UNPOSTED_STOPS => (
            CheckStatus::Warn,
            format!(
                "{n} undelivered SubagentStop record(s) at the {MAX_UNPOSTED_STOPS} cap in {} — \
                 further stops are DROPPED; the daemon's reap loop is not draining them",
                dir.display()
            ),
        ),
        (n, true) => (
            CheckStatus::Warn,
            format!(
                "{n} undelivered SubagentStop record(s) waiting in {} — the daemon's reap loop \
                 should drain them within 60 s",
                dir.display()
            ),
        ),
        (n, false) => (
            CheckStatus::Warn,
            format!(
                "the undelivered-stop spool {} is not writable ({n} record(s) present) — a stop \
                 the daemon refuses is LOST, and its delegation stays Running for six hours",
                dir.display()
            ),
        ),
    };
    DoctorCheck::new("stop_spool", status, detail)
}

#[cfg(test)]
mod stop_spool_tests {
    use super::*;
    use crate::core::doctor::CheckStatus;

    fn body(agent: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": "11111111-1111-1111-1111-111111111111",
            "event": "SubagentStop",
            "payload": {"agent_id": agent},
        })
    }

    #[test]
    fn the_spool_directory_is_under_the_framework_root() {
        let root = Path::new("/tmp/.trusty-mpm");
        assert_eq!(
            unposted_stops_dir(root),
            Path::new("/tmp/.trusty-mpm/unposted-stops")
        );
    }

    #[test]
    fn a_recorded_stop_reads_back_verbatim() {
        let dir = tempfile::tempdir().expect("temp dir");
        let written = record_unposted_stop(dir.path(), &body("agent-1")).expect("recorded");
        assert!(written.exists());
        let read = read_unposted_stops(dir.path());
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].1.as_ref(), Some(&body("agent-1")));
    }

    #[test]
    fn records_read_back_oldest_first() {
        let dir = tempfile::tempdir().expect("temp dir");
        for n in 0..3 {
            record_unposted_stop(dir.path(), &body(&format!("agent-{n}"))).expect("recorded");
        }
        let agents: Vec<String> = read_unposted_stops(dir.path())
            .into_iter()
            .filter_map(|(_, v)| v)
            .filter_map(|v| v["payload"]["agent_id"].as_str().map(str::to_string))
            .collect();
        assert_eq!(agents, vec!["agent-0", "agent-1", "agent-2"]);
    }

    #[test]
    fn an_unparsable_record_is_reported_so_the_drain_can_discard_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(unposted_stops_dir(dir.path())).expect("dir");
        std::fs::write(
            unposted_stops_dir(dir.path()).join("0-0-0.json"),
            "{ not json",
        )
        .expect("write");
        let read = read_unposted_stops(dir.path());
        assert_eq!(read.len(), 1);
        assert!(read[0].1.is_none(), "a corrupt record parses to None");
    }

    #[test]
    fn a_full_spool_refuses_further_records() {
        let dir = tempfile::tempdir().expect("temp dir");
        let spool = unposted_stops_dir(dir.path());
        std::fs::create_dir_all(&spool).expect("dir");
        for n in 0..MAX_UNPOSTED_STOPS {
            std::fs::write(spool.join(format!("{n:05}-0-0.json")), "{}").expect("write");
        }
        assert!(
            record_unposted_stop(dir.path(), &body("overflow")).is_none(),
            "the cap drops the new record rather than evicting an older one"
        );
    }

    #[test]
    fn a_discarded_record_does_not_read_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let written = record_unposted_stop(dir.path(), &body("agent-1")).expect("recorded");
        assert!(discard_unposted_stop(&written));
        assert!(read_unposted_stops(dir.path()).is_empty());
        assert!(
            discard_unposted_stop(&written),
            "an already-absent record is success — it no longer replays"
        );
    }

    #[test]
    fn an_empty_spool_is_an_ok_row() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(check_stop_spool(dir.path()).status, CheckStatus::Ok);
        std::fs::create_dir_all(unposted_stops_dir(dir.path())).expect("dir");
        assert_eq!(check_stop_spool(dir.path()).status, CheckStatus::Ok);
    }

    #[test]
    fn parked_records_warn() {
        let dir = tempfile::tempdir().expect("temp dir");
        record_unposted_stop(dir.path(), &body("agent-1")).expect("recorded");
        let row = check_stop_spool(dir.path());
        assert_eq!(row.status, CheckStatus::Warn);
        assert!(row.message.contains("1 undelivered"), "{}", row.message);
    }

    #[test]
    fn a_saturated_spool_names_the_cap() {
        let dir = tempfile::tempdir().expect("temp dir");
        let spool = unposted_stops_dir(dir.path());
        std::fs::create_dir_all(&spool).expect("dir");
        for n in 0..MAX_UNPOSTED_STOPS {
            std::fs::write(spool.join(format!("{n:05}-0-0.json")), "{}").expect("write");
        }
        let row = check_stop_spool(dir.path());
        assert_eq!(row.status, CheckStatus::Warn);
        assert!(row.message.contains("DROPPED"), "{}", row.message);
    }

    #[test]
    fn an_absent_spool_reads_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(read_unposted_stops(dir.path()).is_empty());
    }
}
