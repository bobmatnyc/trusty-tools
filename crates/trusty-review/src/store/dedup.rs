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
//! The open/lock/recovery plumbing lives in `dedup_open.rs`.
//!
//! Error contract (#5064): a failed operation means the claim gate did NOT
//! engage — the caller does not know whether this head SHA was already
//! reviewed, and cannot record that it is reviewing it now. Callers must
//! therefore **abort without posting**, never log-and-proceed. The dropped
//! review is genuinely lost: the webhook handler acks with 202 before the
//! review runs, so GitHub does not redeliver and a human must re-request it.
//! That is still the better half of the trade — a dropped review can be
//! re-requested, a duplicate comment cannot be retracted.
//! `pipeline::runner::classify_claim` is the single place that decision is
//! made.
//!
//! Blocking: the `*_blocking` methods sleep while waiting for the file lock and
//! must never run on an async runtime worker. Async callers use the `async`
//! methods, which move the work to a blocking thread.
//!
//! Test: `claim_then_skip_after_complete`, `claim_allows_after_release`,
//! `stale_in_progress_is_reclaimable`, `different_sha_not_skipped`,
//! `two_stores_on_one_path_both_work`, `concurrent_threads_claim_exactly_once`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use super::dedup_open::open_dedup_db_waiting;
use crate::config::constants::DEDUP_STALE_SECS;

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
/// "serialisation bug" in logs. #5064: the *policy* is the same for every
/// variant — the claim gate did not engage, so the caller aborts without
/// posting — but the diagnosis differs, and `Contended` in particular tells an
/// operator to look for a second holder rather than a corrupt file.
/// What: wraps redb's database/transaction/table/commit errors plus JSON
/// (de)serialisation failures.
/// Test: error variants are surfaced via the public methods; `Display` is
/// derived by thiserror.
///
/// `#[non_exhaustive]`: adding `Contended` cost trusty-review a 0.x breaking
/// bump purely because an external exhaustive `match` could no longer compile.
/// The attribute makes every future variant additive instead, so the next one
/// costs nothing — the same hardening #5065 applied to `trusty-common`'s
/// `IndexOptions`. It only constrains matching; external callers can still
/// construct existing variants.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    /// **Blocking** — probes the file, so it can wait out a concurrent holder
    /// for up to the lock-wait budget. Today the only `Required` caller is the
    /// HTTP daemon's synchronous startup, before it accepts traffic; the stdio
    /// path declares `DedupNeed::NotNeeded` and never reaches here. A future
    /// caller on a hot async path must wrap this the way the claim/complete/
    /// release wrappers do (#5064).
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
    /// holder up to the lock-wait budget), runs `f`, then drops the database.
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
    /// **Blocking** — waits on the redb file lock. Async callers must use
    /// [`DedupStore::claim`], never this directly (#5064).
    ///
    /// Why: this is the idempotency gate — it must atomically decide whether the
    /// caller runs the review or skips because a completed one already exists.
    /// What: within one write transaction, reads any existing record: a
    /// `Completed` record → `Skipped`; a fresh `InProgress` record → `Skipped`
    /// (another worker owns it); a stale `InProgress` record or no record →
    /// writes a fresh `InProgress` claim and returns `Claimed`.
    /// Test: `claim_then_skip_after_complete`, `concurrent_in_progress_skips`,
    /// `stale_in_progress_is_reclaimable`.
    pub fn claim_blocking(
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
    /// **Blocking** — see [`DedupStore::complete`] for the async form.
    ///
    /// Why: only a completed claim suppresses future re-runs; this is called on
    /// successful review finish.
    /// What: overwrites the record with a `Completed` state and fresh timestamp.
    /// Test: `claim_then_skip_after_complete`.
    pub fn complete_blocking(
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
    /// **Blocking** — see [`DedupStore::release`] for the async form.
    ///
    /// Why: if a review aborts (error, panic-recovery, shutdown) the claim must
    /// be dropped so a later attempt can re-run instead of being suppressed.
    /// What: removes the record for the key entirely.
    /// Test: `claim_allows_after_release`.
    pub fn release_blocking(
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

// ─── Async surface ──────────────────────────────────────────────────────────────

/// Async wrappers for every store operation (#5064).
///
/// Why: the blocking methods sleep for up to two seconds waiting on the redb
/// file lock, and every production caller runs inside tokio — the webhook
/// review runs in a `tokio::spawn`ed task. Blocking a runtime worker for that
/// long stalls every other task on it. These wrappers move the work to a
/// blocking thread so the wait costs a task, not a worker.
/// What: each clones the `Arc` and the string arguments (`spawn_blocking`
/// requires `'static`) and forwards to the matching `*_blocking` method. A
/// join failure — a panic in the closure, or runtime shutdown — surfaces as
/// `DedupError::Transaction` rather than being unwrapped, so it reaches the
/// same fail-closed path as any other store failure.
/// Test: `async_claim_runs_off_the_runtime_worker`,
/// `async_claim_complete_release_round_trip`.
impl DedupStore {
    /// Async [`DedupStore::claim_blocking`].
    pub async fn claim(
        self: &Arc<Self>,
        owner: &str,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<ClaimOutcome, DedupError> {
        let (this, owner, repo, sha) = self.owned_args(owner, repo, head_sha);
        spawn_store_op(move || this.claim_blocking(&owner, &repo, pr, &sha)).await
    }

    /// Async [`DedupStore::complete_blocking`].
    pub async fn complete(
        self: &Arc<Self>,
        owner: &str,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<(), DedupError> {
        let (this, owner, repo, sha) = self.owned_args(owner, repo, head_sha);
        spawn_store_op(move || this.complete_blocking(&owner, &repo, pr, &sha)).await
    }

    /// Async [`DedupStore::release_blocking`].
    pub async fn release(
        self: &Arc<Self>,
        owner: &str,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<(), DedupError> {
        let (this, owner, repo, sha) = self.owned_args(owner, repo, head_sha);
        spawn_store_op(move || this.release_blocking(&owner, &repo, pr, &sha)).await
    }

    /// Clone the arguments `spawn_blocking`'s `'static` bound requires.
    fn owned_args(
        self: &Arc<Self>,
        owner: &str,
        repo: &str,
        head_sha: &str,
    ) -> (Arc<Self>, String, String, String) {
        (
            Arc::clone(self),
            owner.to_string(),
            repo.to_string(),
            head_sha.to_string(),
        )
    }
}

/// Run one blocking store operation off the async runtime's worker threads.
///
/// Why: see the `impl` block above — the file-lock wait must not occupy a
/// tokio worker.
/// What: `spawn_blocking` plus join-error mapping; the closure's own
/// `DedupError` passes through untouched.
/// Test: `async_claim_runs_off_the_runtime_worker`.
async fn spawn_store_op<T: Send + 'static>(
    op: impl FnOnce() -> Result<T, DedupError> + Send + 'static,
) -> Result<T, DedupError> {
    match tokio::task::spawn_blocking(op).await {
        Ok(result) => result,
        Err(join) => Err(DedupError::Transaction(format!(
            "dedup store task did not complete: {join}"
        ))),
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
