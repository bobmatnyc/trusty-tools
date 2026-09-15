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

/// How long a parked record may wait before anything calls it a problem.
///
/// Why: the daemon drains the spool from its reap loop, which ticks every 60 s
/// (`daemon::REAP_INTERVAL_SECS`), so a record written seconds ago is inside the
/// NORMAL park-to-tick window and warning on it trains an operator to ignore the
/// row (critic LOW on PR #8052). Two ticks is the first age at which a drain has
/// provably not happened. The same window bounds [`sweep_stale_temp_files`]: a
/// temp file older than two ticks cannot belong to a live writer, which finishes
/// in milliseconds. Spelled here rather than derived from the daemon constant
/// because `core` compiles without the `daemon` feature.
/// Test: `a_record_inside_the_drain_window_is_an_ok_row`, `parked_records_warn`.
pub const SPOOL_DRAIN_GRACE_SECS: u64 = 120;

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

/// Remove the temp files a killed writer left behind.
///
/// Why: [`record_unposted_stop`] writes temp-then-rename, so a SIGKILL between
/// the two leaves a `.tmpXXXXXX` file that no reader parses, no drain discards
/// and [`count_records`] does not see — litter that only accumulates (critic LOW
/// on PR #8052).
/// What: removes every non-record file directly under the spool older than
/// [`SPOOL_DRAIN_GRACE_SECS`], returning how many it removed. A `.json` record is
/// never touched, and neither is a temp file young enough to belong to a writer
/// still running. Called from the daemon's drain, which is the one process that
/// visits the directory on a schedule.
/// Test: `stale_temp_litter_is_swept`, `a_fresh_temp_file_survives_the_sweep`.
pub fn sweep_stale_temp_files(root: &Path) -> usize {
    let dir = unposted_stops_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let grace = std::time::Duration::from_secs(SPOOL_DRAIN_GRACE_SECS);
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| !is_record(p))
        .filter(|p| file_age(p).is_some_and(|age| age >= grace))
        .filter(|p| std::fs::remove_file(p).is_ok())
        .count()
}

/// How long ago `path` was last modified.
///
/// What: `None` when the file is gone, or when its modified time is in the
/// future — an unusable answer either way, and every caller treats it as "no
/// evidence this is stale".
fn file_age(path: &Path) -> Option<std::time::Duration> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()?
        .elapsed()
        .ok()
}

/// How long the oldest record in `dir` has been waiting.
///
/// What: the largest modified-time age across the `.json` records, or `None`
/// when the directory holds none the filesystem can date.
/// Test: `a_record_inside_the_drain_window_is_an_ok_row`.
fn oldest_record_age(dir: &Path) -> Option<std::time::Duration> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_record(p))
        .filter_map(|p| file_age(&p))
        .max()
}

/// Can THIS process write into `dir`?
///
/// Why: `Permissions::readonly()` is `mode & 0o222 == 0` on Unix — it asks
/// whether ANYBODY may write, not whether we may (critic MEDIUM on PR #8052). A
/// spool whose owner bits deny write while its group/other bits grant it (the
/// shape a root-owned 0755 directory has for every other process), and a full
/// filesystem, both answered "writable", so the row went green while every stop
/// was being dropped.
/// What: creates and drops a temp file through the same call
/// [`record_unposted_stop`] uses, so the probe fails on exactly what the writer
/// fails on — permissions, `ENOSPC`, a read-only mount. Costs one create plus one
/// unlink per doctor run, and leaves nothing behind.
/// Test: `an_unwritable_spool_warns_even_when_the_mode_bits_look_writable`.
fn spool_is_writable(dir: &Path) -> bool {
    tempfile::NamedTempFile::new_in(dir).is_ok()
}

/// `tm doctor` row for the undelivered-stop spool (#6556).
///
/// Why: the lossy branch — a spool that cannot be written or is at
/// [`MAX_UNPOSTED_STOPS`] — is the one place a stop is dropped outright, and its
/// only alarm was a stderr line on an exit-0 hook, which Claude Code does not
/// surface (critic MEDIUM on PR #8052). A doctor row is the surface an operator
/// actually reads.
/// What: `Ok` when the directory is absent, empty, or holding only records
/// younger than [`SPOOL_DRAIN_GRACE_SECS`] — a park waiting out the reap tick is
/// the mechanism working, not a fault, and the count still names it. `Warn`
/// unconditionally when the spool cannot be written or sits at
/// [`MAX_UNPOSTED_STOPS`] (both drop stops outright), and for a record that has
/// outlived two reap ticks without being drained. Creates and removes one temp
/// file to probe writability; leaves nothing behind.
/// Test: `an_empty_spool_is_an_ok_row`, `a_record_inside_the_drain_window_is_an_ok_row`,
/// `parked_records_warn`, `a_saturated_spool_names_the_cap`,
/// `an_unwritable_spool_warns_even_when_the_mode_bits_look_writable`.
#[must_use]
pub fn check_stop_spool(root: &Path) -> crate::core::doctor::DoctorCheck {
    use crate::core::doctor::{CheckStatus, DoctorCheck};

    let dir = unposted_stops_dir(root);
    let empty = || {
        DoctorCheck::new(
            "stop_spool",
            CheckStatus::Ok,
            format!("no undelivered SubagentStop records ({})", dir.display()),
        )
    };
    if !dir.exists() {
        return empty();
    }
    let parked = count_records(&dir);
    let warn = |detail: String| DoctorCheck::new("stop_spool", CheckStatus::Warn, detail);
    if !spool_is_writable(&dir) {
        return warn(format!(
            "the undelivered-stop spool {} is not writable by this process ({parked} record(s) \
             present) — a stop the daemon refuses is LOST, and its delegation stays Running for \
             six hours",
            dir.display()
        ));
    }
    if parked >= MAX_UNPOSTED_STOPS {
        return warn(format!(
            "{parked} undelivered SubagentStop record(s) at the {MAX_UNPOSTED_STOPS} cap in {} — \
             further stops are DROPPED; the daemon's reap loop is not draining them",
            dir.display()
        ));
    }
    if parked == 0 {
        return empty();
    }
    let waited = oldest_record_age(&dir).unwrap_or_default();
    if waited < std::time::Duration::from_secs(SPOOL_DRAIN_GRACE_SECS) {
        return DoctorCheck::new(
            "stop_spool",
            CheckStatus::Ok,
            format!(
                "{parked} undelivered SubagentStop record(s) parked in {}, the oldest {}s ago — \
                 inside the daemon's 60 s reap tick",
                dir.display(),
                waited.as_secs()
            ),
        );
    }
    warn(format!(
        "{parked} undelivered SubagentStop record(s) waiting in {}, the oldest {}s ago — past two \
         reap ticks, so the daemon's drain is not running",
        dir.display(),
        waited.as_secs()
    ))
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

    /// Age `path` by `secs`, so a test can cross a threshold without sleeping.
    fn backdate(path: &Path, secs: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open for set_times");
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("backdate");
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

    /// A park the daemon has not reached yet is the mechanism working.
    ///
    /// Why (critic LOW on PR #8052): every park spends up to 60 s waiting for
    /// the reap tick, so warning on arrival made the row red for the normal
    /// case and trained an operator to ignore it. The count is still reported —
    /// the row says how many are parked, it just does not call it a fault.
    #[test]
    fn a_record_inside_the_drain_window_is_an_ok_row() {
        let dir = tempfile::tempdir().expect("temp dir");
        record_unposted_stop(dir.path(), &body("agent-1")).expect("recorded");
        let row = check_stop_spool(dir.path());
        assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
        assert!(row.message.contains("1 undelivered"), "{}", row.message);
    }

    /// Past two reap ticks the drain has provably not run, which IS the fault.
    #[test]
    fn parked_records_warn() {
        let dir = tempfile::tempdir().expect("temp dir");
        let written = record_unposted_stop(dir.path(), &body("agent-1")).expect("recorded");
        backdate(&written, SPOOL_DRAIN_GRACE_SECS + 5);
        let row = check_stop_spool(dir.path());
        assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
        assert!(row.message.contains("1 undelivered"), "{}", row.message);
    }

    /// 🔴 REGRESSION (critic MEDIUM on PR #8052): the row must ask whether THIS
    /// process can write the spool, not whether the mode bits grant write to
    /// anybody.
    ///
    /// Why: the old probe was `!metadata.permissions().readonly()`, which on
    /// Unix is `mode & 0o222 != 0`. A spool whose OWNER bits deny write while
    /// its group/other bits grant it reads back "writable" under that test —
    /// the same split a root-owned 0755 spool has for every non-root process,
    /// where the row returned Ok with 0 records while every stop was dropped.
    /// 0o522 reproduces it without a second uid: we own the directory, so only
    /// the owner bits (`r-x`) apply to us, and `mode & 0o222` is still 0o022.
    /// Fails at 11e0032d4, which reads Ok here.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_spool_warns_even_when_the_mode_bits_look_writable() {
        use std::os::unix::fs::PermissionsExt as _;

        if nix_running_as_root() {
            return; // root bypasses the mode bits this case is built from.
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let spool = unposted_stops_dir(dir.path());
        std::fs::create_dir_all(&spool).expect("dir");
        std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o522)).expect("chmod");

        let row = check_stop_spool(dir.path());

        // Restore before asserting, so a failure still leaves a removable dir.
        std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
        assert!(row.message.contains("not writable"), "{}", row.message);
    }

    /// Whether this process would bypass the mode bits the test above needs.
    #[cfg(unix)]
    fn nix_running_as_root() -> bool {
        // SAFETY: `geteuid` takes no arguments, reads a process property and
        // cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    /// 🔴 A SIGKILL between the temp write and the rename leaves a file nothing
    /// else in this module counts, parses or removes (critic LOW on PR #8052).
    #[test]
    fn stale_temp_litter_is_swept() {
        let dir = tempfile::tempdir().expect("temp dir");
        let spool = unposted_stops_dir(dir.path());
        std::fs::create_dir_all(&spool).expect("dir");
        let litter = spool.join(".tmpAbCdEf");
        std::fs::write(&litter, "half a record").expect("write");
        backdate(&litter, SPOOL_DRAIN_GRACE_SECS + 5);
        let kept = record_unposted_stop(dir.path(), &body("agent-1")).expect("recorded");
        backdate(&kept, SPOOL_DRAIN_GRACE_SECS + 5);

        assert_eq!(sweep_stale_temp_files(dir.path()), 1);

        assert!(!litter.exists(), "the litter is gone");
        assert!(kept.exists(), "a record is never swept, however old");
    }

    /// The other half of the sweep: a temp file young enough to belong to a
    /// writer still running must survive, or the sweep races the write it is
    /// cleaning up after.
    #[test]
    fn a_fresh_temp_file_survives_the_sweep() {
        let dir = tempfile::tempdir().expect("temp dir");
        let spool = unposted_stops_dir(dir.path());
        std::fs::create_dir_all(&spool).expect("dir");
        let in_flight = spool.join(".tmpZzZzZz");
        std::fs::write(&in_flight, "being written now").expect("write");

        assert_eq!(sweep_stale_temp_files(dir.path()), 0);
        assert!(in_flight.exists());
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
