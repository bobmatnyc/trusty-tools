//! Opening `dedup.redb`: lock waiting, format recovery, and the per-mode
//! open/skip decision (#5064).
//!
//! Why: redb takes an exclusive advisory file lock for as long as its
//! `Database` is alive. `DedupStore` therefore opens redb once per operation
//! (see `dedup.rs`), which turns two problems that used to be impossible into
//! ones this module has to solve: several holders now legitimately contend for
//! the same file, and the #702 incompatible-format recovery — which renames the
//! old file aside — can now run while another process is mid-recovery.
//!
//! What: [`open_dedup_db_waiting`] waits out a concurrent holder within a
//! bounded budget; [`recover_incompatible_db`] serialises the rename-aside
//! recovery behind a sidecar lock file so two processes cannot rename away each
//! other's fresh database; [`DedupNeed`] and [`open_for`] are the single entry
//! point every `AppState` builder uses to declare whether it needs the store at
//! all.
//!
//! Test: `dedup_tests.rs` — `held_lock_that_never_releases_reports_contention`,
//! `cross_process_holder_serialises_rather_than_locking_out`,
//! `concurrent_recovery_waits_and_adopts_the_winners_database`,
//! `open_for_not_needed_touches_nothing`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use redb::{Database, DatabaseError};

use super::dedup::{DedupError, DedupStore};

/// How long an operation waits for another holder to release `dedup.redb`
/// before failing loudly (#5064).
///
/// Each holder keeps the lock only for a single open + write transaction +
/// fsync, so a wait longer than a second means something is genuinely wrong
/// rather than merely busy.
pub(super) const LOCK_WAIT_BUDGET: Duration = Duration::from_secs(2);

/// Poll interval while waiting out a concurrent holder's lock.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Outcome of one non-blocking attempt to open `dedup.redb`.
///
/// Why: #5064 needs "someone else holds the lock right now" to be a retryable
/// state, not an error — collapsing it into `DedupError::Open` is what let the
/// caller mistake contention for an unusable store and continue without one.
/// What: `Ready` carries the opened database; `Locked` means redb reported
/// `DatabaseAlreadyOpen` and the caller may retry.
/// Test: `held_lock_that_never_releases_reports_contention` drives `Locked`
/// until the budget expires; `cross_process_holder_serialises_rather_than_locking_out`
/// drives the `Locked` → `Ready` transition across a real process boundary.
enum OpenAttempt {
    /// The database was opened; the caller owns the exclusive lock.
    Ready(Box<Database>),
    /// Another holder has the exclusive lock; retryable.
    Locked,
}

/// Suffix appended to `dedup.redb` for the recovery mutex file.
pub(super) const RECOVERY_LOCK_SUFFIX: &str = ".recovery-lock";

/// Suffix used when an unreadable database is moved aside.
const INCOMPATIBLE_SUFFIX: &str = ".v2-incompatible";

/// Build a sibling path by appending `suffix` to the file name.
pub(super) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Try once to open the dedup redb at `path`.
///
/// Why: separating a single attempt from the retry policy keeps the lock-wait
/// loop readable and makes the three distinct outcomes — open, locked,
/// unusable — explicit rather than folded into one error type.
/// What: `DatabaseAlreadyOpen` becomes [`OpenAttempt::Locked`]; an
/// incompatible/old format is routed to [`recover_incompatible_db`]; anything
/// else is a `DedupError::Open`.
/// Test: `incompatible_dedup_db_is_recreated`, `open_creates_file`.
fn try_open_dedup_db(path: &Path) -> Result<OpenAttempt, DedupError> {
    match Database::create(path) {
        Ok(db) => Ok(OpenAttempt::Ready(Box::new(db))),
        // #5064: contention is transient — hand it back for the retry loop.
        Err(DatabaseError::DatabaseAlreadyOpen) => Ok(OpenAttempt::Locked),
        Err(e) if super::redb_error_is_incompatible_format(&e) => recover_incompatible_db(path, &e),
        Err(e) => Err(DedupError::Open(e.to_string())),
    }
}

/// Rebuild an unreadable `dedup.redb`, with at most one recoverer at a time.
///
/// Why: redb 4.x cannot open a file written by redb 2.x (#702), so an
/// unreadable file is moved aside and replaced with an empty one — losing the
/// dedup history costs at most one duplicate review. #5064 made that recovery
/// reachable from several processes at once, and an unguarded rename is a
/// data-loss race: the second recoverer renames away the fresh database the
/// first has already committed claims into.
/// What: takes an exclusive lock on a `dedup.redb.recovery-lock` sidecar, then
/// **re-checks** — a recoverer that lost the race finds a healthy database and
/// simply uses it, or finds it locked and retries — and only rebuilds if the
/// file is still unreadable.
/// Test: `incompatible_dedup_db_is_recreated`,
/// `concurrent_recovery_waits_and_adopts_the_winners_database`.
fn recover_incompatible_db(path: &Path, cause: &DatabaseError) -> Result<OpenAttempt, DedupError> {
    let lock_path = sibling(path, RECOVERY_LOCK_SUFFIX);
    let lock = File::create(&lock_path).map_err(|io| {
        DedupError::Open(format!(
            "could not create dedup recovery lock at {}: {io}",
            lock_path.display()
        ))
    })?;
    // Blocking exclusive lock: recovery is rare and must not run concurrently.
    lock.lock().map_err(|io| {
        DedupError::Open(format!(
            "could not lock dedup recovery lock at {}: {io}",
            lock_path.display()
        ))
    })?;

    // Re-check under the lock: another process may have already rebuilt the
    // file (and may even be using it right now).
    match Database::create(path) {
        Ok(db) => return Ok(OpenAttempt::Ready(Box::new(db))),
        Err(DatabaseError::DatabaseAlreadyOpen) => return Ok(OpenAttempt::Locked),
        Err(e) if super::redb_error_is_incompatible_format(&e) => {}
        Err(e) => return Err(DedupError::Open(e.to_string())),
    }

    // #5063: the classification is trusty-common's; this rename-aside policy
    // stays here because it must run under the sidecar recovery lock (#5064).
    let backup = sibling(path, INCOMPATIBLE_SUFFIX);
    std::fs::rename(path, &backup).map_err(|io| {
        DedupError::Open(format!(
            "incompatible-format dedup redb at {} could not be backed up: {io}",
            path.display()
        ))
    })?;
    tracing::error!(
        path = %path.display(),
        backup = %backup.display(),
        error = %cause,
        "dedup redb is in an incompatible/old format (redb 2.x); moved it aside and \
         creating a fresh empty dedup store"
    );
    match Database::create(path) {
        Ok(db) => Ok(OpenAttempt::Ready(Box::new(db))),
        // #5064: still retryable rather than fatal — a sibling may have grabbed
        // the fresh file between our create and theirs.
        Err(DatabaseError::DatabaseAlreadyOpen) => Ok(OpenAttempt::Locked),
        Err(e) => Err(DedupError::Open(e.to_string())),
    }
}

/// Open the dedup redb at `path`, waiting out a concurrent holder's exclusive
/// lock (#5064).
///
/// Why: `dedup.redb` now has several legitimate openers — the HTTP daemon, a
/// console-spawned webhook worker, and the CLI — and redb's lock is exclusive.
/// Waiting turns a collision into a short serialisation instead of a lost
/// store, which is the only outcome that preserves the claim gate.
/// What: retries every [`LOCK_POLL_INTERVAL`] until it succeeds or
/// [`LOCK_WAIT_BUDGET`] elapses, then returns [`DedupError::Contended`].
/// Non-contention errors propagate immediately.
///
/// Blocking: sleeps. Callers on an async runtime must reach this through
/// `DedupStore`'s async methods, which run it on a blocking thread.
/// Test: `held_lock_that_never_releases_reports_contention`,
/// `cross_process_holder_serialises_rather_than_locking_out`.
pub(super) fn open_dedup_db_waiting(path: &Path) -> Result<Box<Database>, DedupError> {
    let started = Instant::now();
    loop {
        match try_open_dedup_db(path)? {
            OpenAttempt::Ready(db) => return Ok(db),
            OpenAttempt::Locked => {
                let waited = started.elapsed();
                if waited >= LOCK_WAIT_BUDGET {
                    return Err(DedupError::Contended {
                        path: path.display().to_string(),
                        waited_ms: u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
                    });
                }
                std::thread::sleep(LOCK_POLL_INTERVAL);
            }
        }
    }
}

// ─── Per-mode open decision ──────────────────────────────────────────────────

/// Whether a server mode needs the durable dedup claim store (#5064).
///
/// Why: `dedup.redb` guards against posting the same review comment twice, so
/// a mode that can post must have it and a mode that cannot post has no reason
/// to touch the file at all. Before #5064 every builder attempted the open and
/// swallowed the failure at `warn!`, which meant a posting-capable mode could
/// start with no idempotency and still look healthy.
/// What: a two-state declaration made at each `AppState` construction site and
/// consumed by [`open_for`]. Every site declares one; none may pass `None`
/// with a prose justification instead.
/// Test: `open_for_not_needed_touches_nothing`, `open_for_required_opens`,
/// `open_for_required_propagates_failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupNeed {
    /// This mode may post GitHub comments; a store it cannot open is fatal.
    Required,
    /// This mode never posts (`allow_posting: false` on every code path), so
    /// the store is not opened.
    NotNeeded,
}

/// Open the durable dedup claim store for a `log_dir`, or decline to (#5064).
///
/// Why: the single place where "does this mode need dedup" turns into a
/// filesystem action, so no construction site can drift into the other's
/// behaviour. `NotNeeded` must not create or lock the file — that is what put
/// `serve --stdio` in contention with the HTTP daemon and, under ADR-0034,
/// with a console-spawned webhook worker. `Required` must not swallow a
/// failure — a posting-capable server without a claim gate double-posts on
/// redelivery while every health signal still reads green.
/// What: returns `Ok(None)` without touching the filesystem for `NotNeeded`;
/// for `Required` returns the opened store or propagates the error.
/// Test: `open_for_not_needed_touches_nothing`, `open_for_required_opens`,
/// `open_for_required_propagates_failure`, `two_posting_servers_share_a_log_dir`.
pub fn open_for(log_dir: &Path, need: DedupNeed) -> Result<Option<Arc<DedupStore>>, DedupError> {
    let path = log_dir.join("dedup.redb");
    match need {
        DedupNeed::NotNeeded => {
            tracing::debug!(
                path = %path.display(),
                "dedup store not opened — this mode never posts (#5064)"
            );
            Ok(None)
        }
        DedupNeed::Required => {
            let store = DedupStore::open(&path)?;
            tracing::info!(path = %path.display(), "dedup store opened");
            Ok(Some(Arc::new(store)))
        }
    }
}
