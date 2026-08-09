//! Bounded retry and quarantine for an inbox entry the pipeline refused
//! (#5192, ADR-0034 §5).
//!
//! Why: a drain has exactly two ways to lie about a failure. It can delete the
//! entry — the delivery is gone, the count of held work returns to zero, and
//! health goes green over work that never happened. Or it can retry forever —
//! one poisoned payload pins the drain and every delivery behind it stops
//! moving while every counter still reads "busy, not broken". This module is
//! the third option: count the failures durably, stop at a bound, and move the
//! entry somewhere an operator is told about and nothing deletes.
//!
//! What: [`AttemptRecord`] is a sidecar beside the entry, named `.attempt` so
//! [`super::held_count`] and [`super::Inbox::list`] — both of which filter on
//! `.json` — never mistake bookkeeping for work. [`quarantine`] links the entry
//! into `<inbox>/quarantine/` and then unlinks the original, in that order, so a
//! crash between the two leaves the delivery in both places rather than
//! neither; the re-run's `EEXIST` is treated as success, which makes the whole
//! move idempotent.
//!
//! 🔴 Nothing here deletes a delivery. `remove_processed` is the only path in
//! the drain that unlinks one, and it runs only after a processor said it
//! accepted the work.
//!
//! Test: `tests.rs` — `attempt_*` and `quarantine_*`.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::inbox::{INBOX_DIR_MODE, INBOX_FILE_MODE, InboxError};

/// Subdirectory an entry is moved to once it is out of retries.
///
/// Not `.json`-suffixed and not a file, so `held_count`'s extension filter
/// steps over it — a quarantined delivery must not keep reading as undrained
/// work that a drain is about to pick up, because nothing will.
pub const QUARANTINE_DIR_NAME: &str = "quarantine";

/// Extension of the per-entry attempt sidecar.
pub const ATTEMPT_EXTENSION: &str = "attempt";

/// How many times one entry may fail before it is quarantined.
///
/// Five is enough to ride out a GitHub 5xx, an expired token being refreshed,
/// or a locked dedup store, and few enough that a genuinely poisoned delivery
/// reaches an operator inside one drain interval rather than at the next
/// deploy.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// Durable record of how badly one entry is going.
///
/// Why: the retry bound has to survive the process, because the process is
/// short-lived by design (ADR-0034 §1). An in-memory counter resets on every
/// console-supervised spawn, which turns "bounded retry" into "retry forever,
/// slowly".
/// What: written beside the entry, replaced atomically, and deleted with it.
/// Test: `attempt_record_survives_a_reopen`,
/// `attempt_record_is_removed_with_its_entry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    /// Failed processing attempts so far.
    pub attempts: u32,
    /// The most recent failure, verbatim.
    pub last_error: String,
    /// When the entry first failed.
    pub first_failed_at_unix_ms: u64,
    /// When it last failed.
    pub last_failed_at_unix_ms: u64,
}

/// Sidecar path for an entry.
pub fn attempt_path(entry: &Path) -> PathBuf {
    entry.with_extension(ATTEMPT_EXTENSION)
}

/// Read the sidecar, or a zeroed record when there is none.
///
/// An unreadable or undecodable sidecar reads as zero rather than as an error:
/// losing the count costs at most a few extra attempts, whereas refusing to
/// process the entry because its bookkeeping is corrupt would strand a
/// perfectly good delivery.
pub fn load_attempts(entry: &Path) -> AttemptRecord {
    std::fs::read(attempt_path(entry))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Record one more failure and return the updated count.
///
/// # Errors
///
/// [`InboxError::Write`] when the sidecar cannot be written. The caller must
/// surface this: an unwritable sidecar means the retry bound is not being
/// enforced, which is exactly the "retry forever" failure this module exists to
/// prevent.
///
/// Test: `attempt_record_survives_a_reopen`.
pub fn record_failure(
    entry: &Path,
    reason: &str,
    now_unix_ms: u64,
) -> Result<AttemptRecord, InboxError> {
    let previous = load_attempts(entry);
    let record = AttemptRecord {
        attempts: previous.attempts.saturating_add(1),
        last_error: reason.to_string(),
        first_failed_at_unix_ms: if previous.attempts == 0 {
            now_unix_ms
        } else {
            previous.first_failed_at_unix_ms
        },
        last_failed_at_unix_ms: now_unix_ms,
    };
    let path = attempt_path(entry);
    let bytes = serde_json::to_vec_pretty(&record).map_err(|source| InboxError::Encode {
        delivery_id: path.display().to_string(),
        source,
    })?;
    write_replace(&path, &bytes)?;
    Ok(record)
}

/// Delete an entry the pipeline accepted, and its sidecar.
///
/// 🔴 The only unlink of a live delivery in the whole drain. Called after a
/// processor returned success and never before it — see
/// [`super::drain::drain_once`].
///
/// # Errors
///
/// [`InboxError::Write`] when the entry itself cannot be removed. A leftover
/// sidecar is not an error: it is bookkeeping, and the next write replaces it.
///
/// Test: `drain_removes_an_entry_the_processor_accepted`.
pub fn remove_processed(entry: &Path) -> Result<(), InboxError> {
    std::fs::remove_file(entry).map_err(|source| InboxError::Write {
        path: entry.to_path_buf(),
        source,
    })?;
    let _ = std::fs::remove_file(attempt_path(entry));
    sync_parent(entry);
    Ok(())
}

/// Move an entry out of the drain's way without destroying it.
///
/// What, in order: create `<inbox>/quarantine/` at [`INBOX_DIR_MODE`]; copy the
/// sidecar across so the failure history travels with the delivery;
/// `hard_link` the entry in (an existing link is a completed earlier move, not
/// a failure); fsync the quarantine directory; unlink the original and its
/// sidecar; fsync the inbox.
///
/// # Errors
///
/// [`InboxError`] when the directory cannot be prepared or the link cannot be
/// made. The entry is left where it is — still held, still counted, still
/// visible — rather than removed on a best-effort move.
///
/// Test: `quarantine_moves_the_entry_and_keeps_its_history`,
/// `quarantine_is_idempotent_after_an_interrupted_move`.
pub fn quarantine(inbox_root: &Path, entry: &Path) -> Result<PathBuf, InboxError> {
    let dir = quarantine_dir(inbox_root);
    std::fs::create_dir_all(&dir).map_err(|source| InboxError::PrepareDir {
        path: dir.clone(),
        source,
    })?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(INBOX_DIR_MODE)).map_err(
        |source| InboxError::PrepareDir {
            path: dir.clone(),
            source,
        },
    )?;

    let name = entry
        .file_name()
        .ok_or_else(|| InboxError::Write {
            path: entry.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "inbox entry has no file name",
            ),
        })?
        .to_owned();
    let target = dir.join(&name);

    // Best effort, and deliberately so: the history is diagnostic. Losing it
    // must not stop the delivery itself being preserved.
    if let Ok(bytes) = std::fs::read(attempt_path(entry)) {
        let _ = write_replace(&attempt_path(&target), &bytes);
    }

    match std::fs::hard_link(entry, &target) {
        Ok(()) => {}
        // An earlier move that was interrupted between the link and the unlink.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(InboxError::Commit {
                from: entry.to_path_buf(),
                to: target,
                source,
            });
        }
    }
    sync_dir(&dir)?;

    // A missing original is a completed move, not a failure: an undecodable
    // entry is quarantined without a claim held, so two drainers can both make
    // the (idempotent) move and only one of them can be the unlinker.
    match std::fs::remove_file(entry) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(InboxError::Write {
                path: entry.to_path_buf(),
                source,
            });
        }
    }
    let _ = std::fs::remove_file(attempt_path(entry));
    sync_parent(entry);
    Ok(target)
}

/// Where quarantined deliveries for `inbox_root` live.
pub fn quarantine_dir(inbox_root: &Path) -> PathBuf {
    inbox_root.join(QUARANTINE_DIR_NAME)
}

/// How many deliveries are quarantined under `inbox_root`.
///
/// Why: `trusty-console` renders this as a red health state. A quarantined
/// delivery is work that arrived, was accepted from the sender, and will never
/// be done without a human — the one inbox state that must never read as
/// merely "busy".
/// What: counts `*.json` under `<inbox_root>/quarantine`. An absent directory is
/// `0`, not an error.
///
/// # Errors
///
/// [`InboxError::Read`] when the directory exists but cannot be listed.
///
/// Test: `quarantined_count_reports_zero_when_nothing_is_quarantined`,
/// `quarantine_moves_the_entry_and_keeps_its_history`.
pub fn quarantined_count(inbox_root: &Path) -> Result<usize, InboxError> {
    let dir = quarantine_dir(inbox_root);
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(InboxError::Read { path: dir, source }),
    };
    Ok(read
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count())
}

/// Write `bytes` to `path`, replacing whatever is there, atomically.
fn write_replace(path: &Path, bytes: &[u8]) -> Result<(), InboxError> {
    let tmp = path.with_extension(format!("{ATTEMPT_EXTENSION}.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(INBOX_FILE_MODE))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|source| {
        let _ = std::fs::remove_file(&tmp);
        InboxError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// fsync a directory so an unlink or a new name survives a crash.
fn sync_dir(path: &Path) -> Result<(), InboxError> {
    std::fs::File::open(path)
        .and_then(|d| d.sync_all())
        .map_err(|source| InboxError::SyncDir {
            path: path.to_path_buf(),
            source,
        })
}

/// fsync the directory holding `entry`, ignoring failure.
///
/// An unsynced unlink can only resurrect an already-processed delivery, which
/// the at-least-once contract already requires receivers to tolerate — so this
/// is not worth failing a successful drain over.
fn sync_parent(entry: &Path) {
    if let Some(parent) = entry.parent() {
        let _ = sync_dir(parent);
    }
}
