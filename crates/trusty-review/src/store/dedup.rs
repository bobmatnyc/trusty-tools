//! redb-backed SHA-keyed dedup claim store (issue #582 work-item b, REV-621).
//!
//! Why: GitHub retries webhook deliveries and a PR can be re-requested, so the
//! same (owner, repo, pr, head_sha) review can be triggered multiple times —
//! possibly across separate processes/restarts.  A durable claim store makes a
//! completed review idempotent: a second attempt at the same head SHA is
//! skipped rather than re-run (re-posting a duplicate comment / re-spending
//! tokens).
//!
//! What: `DedupStore` wraps a redb database with one table keyed by a composite
//! `owner/repo/pr/sha` string.  `claim` atomically inserts an in-progress claim
//! (returning `Skipped` if a *completed* claim already exists for that SHA);
//! `complete`/`release` finalise or drop a claim; stale in-progress claims older
//! than `DEDUP_STALE_SECS` are treated as abandoned and may be reclaimed.
//!
//! Locking (#5064): redb takes an **exclusive** advisory file lock for as long
//! as its `Database` is alive, so a store held open for a process's lifetime
//! locks every other process out of the same `dedup.redb`. `DedupStore`
//! therefore holds only a path: it opens redb for the duration of one operation
//! and drops it again, so concurrent holders serialise instead of colliding.
//!
//! Fail-safe: every method returns a typed `DedupError`, but the caller (the
//! runner) is expected to *log and proceed* on error — a store failure must
//! never crash or block a review.
//!
//! Test: `claim_then_skip_after_complete`, `claim_allows_after_release`,
//! `stale_in_progress_is_reclaimable`, `different_sha_not_skipped`,
//! `two_stores_on_one_path_both_work`, `concurrent_threads_claim_exactly_once`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::config::constants::DEDUP_STALE_SECS;

/// How long an operation waits for another holder to release `dedup.redb`
/// before failing loudly (#5064).
///
/// Each holder now keeps the lock only for a single open + write transaction +
/// fsync, so a wait longer than a second means something is genuinely wrong
/// rather than merely busy. A bounded budget also keeps the blocking window on
/// an async runtime worker short.
const LOCK_WAIT_BUDGET: Duration = Duration::from_secs(2);

/// Poll interval while waiting out a concurrent holder's lock.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// redb table: composite key → serialised `ClaimRecord` (JSON).
///
/// Why: a single table keyed by the dedup tuple is the simplest durable shape
/// and matches the Python predecessor's `in_flight_reviews` table.
/// What: key is `"{owner}/{repo}/{pr}/{sha}"`; value is JSON-encoded `ClaimRecord`.
/// Test: exercised by all store tests.
const CLAIMS: TableDefinition<&str, &str> = TableDefinition::new("dedup_claims");

// ─── Errors ─────────────────────────────────────────────────────────────────────

/// Errors produced by the dedup store.
///
/// Why: a typed enum lets the caller distinguish "store unavailable" from
/// "serialisation bug" in logs, even though the policy for both is the same
/// (log + proceed).
/// What: wraps redb's database/transaction/table/commit errors plus JSON
/// (de)serialisation failures.
/// Test: error variants are surfaced via the public methods; `Display` is
/// derived by thiserror.
#[derive(Debug, thiserror::Error)]
pub enum DedupError {
    /// Opening or creating the redb database failed.
    #[error("dedup store open failed: {0}")]
    Open(String),
    /// A read/write transaction failed (begin/commit/table-open).
    #[error("dedup store transaction failed: {0}")]
    Transaction(String),
    /// Serialising or deserialising a claim record failed.
    #[error("dedup store (de)serialisation failed: {0}")]
    Serde(String),
    /// Another holder kept redb's exclusive lock for the whole retry budget.
    ///
    /// #5064: distinct from [`DedupError::Open`] because contention is
    /// transient and retryable, while `Open` means the file itself is unusable.
    /// The caller must never treat this as "dedup is off" — losing the claim
    /// gate is what produces a duplicate review comment.
    #[error("dedup store at {path} stayed locked by another holder for {waited_ms} ms")]
    Contended {
        /// Filesystem path of the contended `dedup.redb`.
        path: String,
        /// Wall-clock milliseconds spent waiting before giving up.
        waited_ms: u64,
    },
}

// ─── Claim record ───────────────────────────────────────────────────────────────

/// Lifecycle state of a dedup claim.
///
/// Why: distinguishing in-progress from completed is what makes the store
/// idempotent — only a *completed* claim suppresses a re-run; an in-progress
/// claim older than the stale window is assumed abandoned and reclaimable.
/// What: `InProgress` is written at review start; `Completed` at review finish.
/// Test: `claim_then_skip_after_complete`, `stale_in_progress_is_reclaimable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimState {
    /// A review for this SHA is currently running.
    InProgress,
    /// A review for this SHA has completed.
    Completed,
}

/// A single durable dedup claim.
///
/// Why: the store must remember both the lifecycle state and when the claim was
/// written so stale in-progress claims can be aged out.
/// What: `state` + a Unix-seconds `updated_at` timestamp.
/// Test: round-tripped through JSON by every store method.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClaimRecord {
    state: ClaimState,
    updated_at: u64,
}

/// Outcome of a `claim` attempt.
///
/// Why: the runner branches on whether it owns the review or should skip a
/// duplicate.
/// What: `Claimed` means this caller should proceed; `Skipped` means a completed
/// review already exists for this SHA.
/// Test: `claim_then_skip_after_complete`, `different_sha_not_skipped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The claim was acquired; the caller owns this review.
    Claimed,
    /// A completed review already exists for this SHA; skip.
    Skipped,
}

/// Outcome of one non-blocking attempt to open `dedup.redb`.
///
/// Why: #5064 needs "someone else holds the lock right now" to be a retryable
/// state, not an error — collapsing it into `DedupError::Open` is what let the
/// caller mistake contention for an unusable store and continue without one.
/// What: `Ready` carries the opened database; `Locked` means redb reported
/// `DatabaseAlreadyOpen` and the caller may retry.
/// Test: `two_stores_on_one_path_both_work` exercises the `Locked` → `Ready`
/// transition; `held_lock_that_never_releases_reports_contention` the timeout.
enum OpenAttempt {
    /// The database was opened; the caller owns the exclusive lock.
    Ready(Box<Database>),
    /// Another holder has the exclusive lock; retryable.
    Locked,
}

/// Try once to open the dedup redb at `path`, recreating it empty on an
/// incompatible (redb-2.x) format (issue #702).
///
/// Why: redb 4.x cannot open a `dedup.redb` written by redb 2.x. The dedup
/// store is a best-effort idempotency cache, so on that error we move the stale
/// file aside (`*.v2-incompatible`) and create a fresh empty store rather than
/// crashing — losing the history at most causes one duplicate review.
/// What: on `DatabaseAlreadyOpen` returns [`OpenAttempt::Locked`]; on
/// `UpgradeRequired` / `RepairAborted` it renames the file aside, logs an
/// `ERROR`, and retries the create; other errors map to `DedupError::Open`.
/// Test: `incompatible_dedup_db_is_recreated`.
fn try_open_dedup_db(path: &Path) -> Result<OpenAttempt, DedupError> {
    match Database::create(path) {
        Ok(db) => Ok(OpenAttempt::Ready(Box::new(db))),
        // #5064: contention is transient — hand it back for the retry loop.
        Err(redb::DatabaseError::DatabaseAlreadyOpen) => Ok(OpenAttempt::Locked),
        Err(e) if super::redb_error_is_incompatible_format(&e) => {
            let mut backup = path.as_os_str().to_os_string();
            backup.push(".v2-incompatible");
            let backup = std::path::PathBuf::from(backup);
            std::fs::rename(path, &backup).map_err(|io| {
                DedupError::Open(format!(
                    "incompatible-format dedup redb at {} could not be backed up: {io}",
                    path.display()
                ))
            })?;
            tracing::error!(
                path = %path.display(),
                backup = %backup.display(),
                error = %e,
                "dedup redb is in an incompatible/old format (redb 2.x); moved it aside and \
                 creating a fresh empty dedup store"
            );
            Database::create(path)
                .map(|db| OpenAttempt::Ready(Box::new(db)))
                .map_err(|e| DedupError::Open(e.to_string()))
        }
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
/// What: retries [`try_open_dedup_db`] every [`LOCK_POLL_INTERVAL`] until it
/// succeeds or [`LOCK_WAIT_BUDGET`] elapses, then returns
/// [`DedupError::Contended`]. Non-contention errors propagate immediately.
/// Test: `two_stores_on_one_path_both_work`,
/// `held_lock_that_never_releases_reports_contention`.
fn open_dedup_db_waiting(path: &Path) -> Result<Box<Database>, DedupError> {
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

// ─── Store ──────────────────────────────────────────────────────────────────────

/// A redb-backed SHA-keyed dedup claim store.
///
/// Why: provides cross-process, durable idempotency for reviews keyed by head
/// SHA so retries and restarts do not produce duplicate reviews. #5064: because
/// redb's file lock is exclusive and process-wide, a store that held its
/// `Database` open for the process's lifetime locked out every sibling opener —
/// the HTTP daemon, a `serve --stdio` session, and (under ADR-0034) a
/// console-spawned webhook worker all open the same `{LOG_DIR}/dedup.redb`.
///
/// What: owns the *path*, not an open database. Each operation opens redb,
/// runs one write transaction, and drops the database again, so the exclusive
/// lock is held for microseconds instead of for the process's lifetime. An
/// in-process `gate` mutex serialises this crate's own callers (redb's lock is
/// per open-file-description, so two handles inside one process contend exactly
/// as two processes do); cross-process contention is waited out by
/// [`open_dedup_db_waiting`]. Safe to share across tasks behind an `Arc`.
///
/// Contract: every method is either authoritative or returns `Err`. There is no
/// state in which a `DedupStore` exists but silently stops deduplicating.
///
/// Test: `dedup_tests.rs` — in particular `two_stores_on_one_path_both_work`
/// and `concurrent_threads_claim_exactly_once`.
#[derive(Debug)]
pub struct DedupStore {
    /// Path to the `dedup.redb` file this store operates on.
    path: PathBuf,
    /// Serialises this process's own operations so in-process callers queue
    /// rather than burn the cross-process retry budget against each other.
    gate: Mutex<()>,
}

impl DedupStore {
    /// Open (or create) the dedup store at `path`.
    ///
    /// Why: the store lives under the review log dir so it persists across
    /// daemon restarts (spec: `{LOG_DIR}/dedup.redb`). Issue #702: redb 4.x
    /// cannot open a `dedup.redb` written by redb 2.x — without a guard the
    /// daemon would crash on the first warm boot after the binary upgrade.
    /// What: creates the redb database file (recreating it empty if the
    /// existing file is in an incompatible/old format), ensures the claims
    /// table exists, then **releases the lock again** (#5064) so the returned
    /// store holds no lock between operations. Losing the dedup history is
    /// harmless — at worst a previously-reviewed SHA is re-reviewed once — but
    /// failing to open the file at all is not, so it is returned as `Err`.
    /// Test: `open_creates_file`, `incompatible_dedup_db_is_recreated`,
    /// `two_stores_on_one_path_both_work`.
    pub fn open(path: &Path) -> Result<Self, DedupError> {
        if let Some(parent) = path.parent() {
            // Best-effort dir creation; a real failure surfaces from Database::create.
            let _ = std::fs::create_dir_all(parent);
        }
        let store = Self {
            path: path.to_path_buf(),
            gate: Mutex::new(()),
        };
        // Probe the file once so an unusable path fails here rather than at the
        // first claim, and ensure the claims table exists.
        store.with_db(|db| {
            let write = db
                .begin_write()
                .map_err(|e| DedupError::Transaction(e.to_string()))?;
            {
                write
                    .open_table(CLAIMS)
                    .map_err(|e| DedupError::Transaction(e.to_string()))?;
            }
            write
                .commit()
                .map_err(|e| DedupError::Transaction(e.to_string()))?;
            Ok(())
        })?;
        Ok(store)
    }

    /// Run `f` against a freshly-opened database, releasing the lock on return.
    ///
    /// Why: this is where #5064's "hold the lock for one operation, not for the
    /// process" contract is enforced. Every public method routes through it, so
    /// no code path can accidentally retain the exclusive lock.
    /// What: takes the in-process `gate`, opens redb (waiting out a concurrent
    /// holder up to [`LOCK_WAIT_BUDGET`]), runs `f`, then drops the database.
    /// A poisoned `gate` is recovered rather than propagated: the guarded state
    /// is `()`, and the durable state lives in redb, which is re-opened here.
    /// Test: `concurrent_threads_claim_exactly_once`,
    /// `held_lock_that_never_releases_reports_contention`.
    fn with_db<T>(
        &self,
        f: impl FnOnce(&Database) -> Result<T, DedupError>,
    ) -> Result<T, DedupError> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let db = open_dedup_db_waiting(&self.path)?;
        let out = f(&db);
        drop(db);
        out
    }

    /// Attempt to claim a review for `(owner, repo, pr, head_sha)`.
    ///
    /// Why: this is the idempotency gate — it must atomically decide whether the
    /// caller runs the review or skips because a completed one already exists.
    /// What: within one write transaction, reads any existing record: a
    /// `Completed` record → `Skipped`; a fresh `InProgress` record → `Skipped`
    /// (another worker owns it); a stale `InProgress` record or no record →
    /// writes a fresh `InProgress` claim and returns `Claimed`.
    /// Test: `claim_then_skip_after_complete`, `concurrent_in_progress_skips`,
    /// `stale_in_progress_is_reclaimable`.
    pub fn claim(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<ClaimOutcome, DedupError> {
        let key = Self::key(owner, repo, pr, head_sha);
        let now = now_secs();

        self.with_db(|db| {
            let write = db
                .begin_write()
                .map_err(|e| DedupError::Transaction(e.to_string()))?;
            let outcome = {
                let mut table = write
                    .open_table(CLAIMS)
                    .map_err(|e| DedupError::Transaction(e.to_string()))?;

                let existing = table
                    .get(key.as_str())
                    .map_err(|e| DedupError::Transaction(e.to_string()))?
                    .map(|v| v.value().to_string());

                let should_claim = match existing {
                    None => true,
                    Some(raw) => {
                        let rec: ClaimRecord = serde_json::from_str(&raw)
                            .map_err(|e| DedupError::Serde(e.to_string()))?;
                        match rec.state {
                            ClaimState::Completed => false,
                            // In-progress: reclaim only if stale (assume abandoned).
                            ClaimState::InProgress => {
                                now.saturating_sub(rec.updated_at) > DEDUP_STALE_SECS
                            }
                        }
                    }
                };

                if should_claim {
                    let rec = ClaimRecord {
                        state: ClaimState::InProgress,
                        updated_at: now,
                    };
                    let json = serde_json::to_string(&rec)
                        .map_err(|e| DedupError::Serde(e.to_string()))?;
                    table
                        .insert(key.as_str(), json.as_str())
                        .map_err(|e| DedupError::Transaction(e.to_string()))?;
                    ClaimOutcome::Claimed
                } else {
                    ClaimOutcome::Skipped
                }
            };
            write
                .commit()
                .map_err(|e| DedupError::Transaction(e.to_string()))?;
            Ok(outcome)
        })
    }

    /// Mark a claimed review as completed (idempotency-defining state).
    ///
    /// Why: only a completed claim suppresses future re-runs; this is called on
    /// successful review finish.
    /// What: overwrites the record with a `Completed` state and fresh timestamp.
    /// Test: `claim_then_skip_after_complete`.
    pub fn complete(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<(), DedupError> {
        self.write_state(owner, repo, pr, head_sha, ClaimState::Completed)
    }

    /// Release an in-progress claim without marking it completed.
    ///
    /// Why: if a review aborts (error, panic-recovery, shutdown) the claim must
    /// be dropped so a later attempt can re-run instead of being suppressed.
    /// What: removes the record for the key entirely.
    /// Test: `claim_allows_after_release`.
    pub fn release(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<(), DedupError> {
        let key = Self::key(owner, repo, pr, head_sha);
        self.with_db(|db| {
            let write = db
                .begin_write()
                .map_err(|e| DedupError::Transaction(e.to_string()))?;
            {
                let mut table = write
                    .open_table(CLAIMS)
                    .map_err(|e| DedupError::Transaction(e.to_string()))?;
                table
                    .remove(key.as_str())
                    .map_err(|e| DedupError::Transaction(e.to_string()))?;
            }
            write
                .commit()
                .map_err(|e| DedupError::Transaction(e.to_string()))?;
            Ok(())
        })
    }

    /// Overwrite the record for a key with the given state.
    fn write_state(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        head_sha: &str,
        state: ClaimState,
    ) -> Result<(), DedupError> {
        let key = Self::key(owner, repo, pr, head_sha);
        let rec = ClaimRecord {
            state,
            updated_at: now_secs(),
        };
        let json = serde_json::to_string(&rec).map_err(|e| DedupError::Serde(e.to_string()))?;
        self.with_db(|db| {
            let write = db
                .begin_write()
                .map_err(|e| DedupError::Transaction(e.to_string()))?;
            {
                let mut table = write
                    .open_table(CLAIMS)
                    .map_err(|e| DedupError::Transaction(e.to_string()))?;
                table
                    .insert(key.as_str(), json.as_str())
                    .map_err(|e| DedupError::Transaction(e.to_string()))?;
            }
            write
                .commit()
                .map_err(|e| DedupError::Transaction(e.to_string()))?;
            Ok(())
        })
    }

    /// Build the composite key string for a review.
    fn key(owner: &str, repo: &str, pr: u64, head_sha: &str) -> String {
        format!("{owner}/{repo}/{pr}/{head_sha}")
    }
}

/// Current Unix time in whole seconds (saturating at epoch).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "dedup_tests.rs"]
mod tests;
