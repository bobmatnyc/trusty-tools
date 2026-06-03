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
//! Fail-safe: every method returns a typed `DedupError`, but the caller (the
//! runner) is expected to *log and proceed* on error — a store failure must
//! never crash or block a review.
//!
//! Test: `claim_then_skip_after_complete`, `claim_allows_after_release`,
//! `stale_in_progress_is_reclaimable`, `different_sha_not_skipped`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

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

/// Open the dedup redb at `path`, recreating it empty on an incompatible
/// (redb-2.x) format (issue #702).
///
/// Why: redb 4.x cannot open a `dedup.redb` written by redb 2.x. The dedup
/// store is a best-effort idempotency cache, so on that error we move the stale
/// file aside (`*.v2-incompatible`) and create a fresh empty store rather than
/// crashing — losing the history at most causes one duplicate review.
/// What: on `UpgradeRequired` / `RepairAborted` it renames the file aside, logs
/// an `ERROR`, and retries the create; other errors map to `DedupError::Open`.
/// Test: `incompatible_dedup_db_is_recreated`.
fn open_dedup_db_or_recreate(path: &Path) -> Result<Database, DedupError> {
    match Database::create(path) {
        Ok(db) => Ok(db),
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
            Database::create(path).map_err(|e| DedupError::Open(e.to_string()))
        }
        Err(e) => Err(DedupError::Open(e.to_string())),
    }
}

// ─── Store ──────────────────────────────────────────────────────────────────────

/// A redb-backed SHA-keyed dedup claim store.
///
/// Why: provides cross-process, durable idempotency for reviews keyed by head
/// SHA so retries and restarts do not produce duplicate reviews.
/// What: owns a redb `Database`; all methods open short transactions so the
/// store is safe to share across tasks behind an `Arc`.
/// Test: see module-level tests, all of which use a tempfile-backed store.
pub struct DedupStore {
    db: Database,
}

impl DedupStore {
    /// Open (or create) the dedup store at `path`.
    ///
    /// Why: the store lives under the review log dir so it persists across
    /// daemon restarts (spec: `{LOG_DIR}/dedup.redb`). Issue #702: redb 4.x
    /// cannot open a `dedup.redb` written by redb 2.x — without a guard the
    /// daemon would crash on the first warm boot after the binary upgrade.
    /// What: creates the redb database file (recreating it empty via
    /// [`open_dedup_db_or_recreate`] if the existing file is in an
    /// incompatible/old format) and ensures the claims table exists. Losing the
    /// dedup history is harmless — at worst a previously-reviewed SHA is
    /// re-reviewed once.
    /// Test: `open_creates_file`, `incompatible_dedup_db_is_recreated`.
    pub fn open(path: &Path) -> Result<Self, DedupError> {
        if let Some(parent) = path.parent() {
            // Best-effort dir creation; a real failure surfaces from Database::create.
            let _ = std::fs::create_dir_all(parent);
        }
        let db = open_dedup_db_or_recreate(path)?;
        // Ensure the table exists so first-read transactions don't error.
        {
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
        }
        Ok(Self { db })
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

        let write = self
            .db
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
                    let rec: ClaimRecord =
                        serde_json::from_str(&raw).map_err(|e| DedupError::Serde(e.to_string()))?;
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
                let json =
                    serde_json::to_string(&rec).map_err(|e| DedupError::Serde(e.to_string()))?;
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
        let write = self
            .db
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
        let write = self
            .db
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

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (DedupStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dedup.redb");
        let store = DedupStore::open(&path).expect("open store");
        (store, dir)
    }

    #[test]
    fn open_creates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("dedup.redb");
        let _store = DedupStore::open(&path).expect("open");
        assert!(path.exists(), "redb file must be created");
    }

    /// Why: #702 graceful-handling — a `dedup.redb` redb 4.x cannot open (a
    /// stale redb-2.x file, simulated with garbage bytes) must NOT crash the
    /// daemon; it is moved aside and replaced with a fresh empty store so the
    /// reviewer keeps working (at worst one duplicate review).
    /// What: writes garbage to `dedup.redb`, opens via `DedupStore::open`,
    /// asserts the open succeeds and the backup file exists.
    /// Test: this test.
    #[test]
    fn incompatible_dedup_db_is_recreated() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.redb");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&[0xABu8; 4096]))
            .unwrap();

        let store = DedupStore::open(&path).expect("incompatible dedup db must recover, not error");
        assert!(
            path.with_file_name("dedup.redb.v2-incompatible").exists(),
            "incompatible dedup file must be backed up"
        );
        // Fresh store: a claim against any SHA succeeds (no stale history).
        assert_eq!(
            store.claim("o", "r", 1, "sha").unwrap(),
            ClaimOutcome::Claimed
        );
    }

    #[test]
    fn first_claim_succeeds() {
        let (store, _d) = temp_store();
        let outcome = store.claim("acme", "backend", 42, "sha-abc").unwrap();
        assert_eq!(outcome, ClaimOutcome::Claimed);
    }

    #[test]
    fn concurrent_in_progress_skips() {
        // A second claim for the same SHA while the first is still in-progress
        // (and not stale) is skipped — another worker owns it.
        let (store, _d) = temp_store();
        assert_eq!(
            store.claim("acme", "backend", 42, "sha-abc").unwrap(),
            ClaimOutcome::Claimed
        );
        assert_eq!(
            store.claim("acme", "backend", 42, "sha-abc").unwrap(),
            ClaimOutcome::Skipped
        );
    }

    #[test]
    fn claim_then_skip_after_complete() {
        let (store, _d) = temp_store();
        assert_eq!(
            store.claim("acme", "backend", 42, "sha-abc").unwrap(),
            ClaimOutcome::Claimed
        );
        store.complete("acme", "backend", 42, "sha-abc").unwrap();
        // After completion, re-claiming the same SHA must be skipped.
        assert_eq!(
            store.claim("acme", "backend", 42, "sha-abc").unwrap(),
            ClaimOutcome::Skipped
        );
    }

    #[test]
    fn claim_allows_after_release() {
        let (store, _d) = temp_store();
        assert_eq!(
            store.claim("acme", "backend", 42, "sha-abc").unwrap(),
            ClaimOutcome::Claimed
        );
        // Release (e.g. review aborted) → the SHA can be claimed again.
        store.release("acme", "backend", 42, "sha-abc").unwrap();
        assert_eq!(
            store.claim("acme", "backend", 42, "sha-abc").unwrap(),
            ClaimOutcome::Claimed
        );
    }

    #[test]
    fn different_sha_not_skipped() {
        let (store, _d) = temp_store();
        store.claim("acme", "backend", 42, "sha-abc").unwrap();
        store.complete("acme", "backend", 42, "sha-abc").unwrap();
        // A new head SHA on the same PR is a fresh review.
        assert_eq!(
            store.claim("acme", "backend", 42, "sha-def").unwrap(),
            ClaimOutcome::Claimed
        );
    }

    #[test]
    fn stale_in_progress_is_reclaimable() {
        // Simulate a crashed worker by writing an in-progress claim with an old
        // timestamp directly, then verify a new claim reclaims it.
        let (store, _d) = temp_store();
        let key = DedupStore::key("acme", "backend", 42, "sha-stale");
        let stale = ClaimRecord {
            state: ClaimState::InProgress,
            updated_at: now_secs().saturating_sub(DEDUP_STALE_SECS + 10),
        };
        let json = serde_json::to_string(&stale).unwrap();
        let write = store.db.begin_write().unwrap();
        {
            let mut t = write.open_table(CLAIMS).unwrap();
            t.insert(key.as_str(), json.as_str()).unwrap();
        }
        write.commit().unwrap();

        assert_eq!(
            store.claim("acme", "backend", 42, "sha-stale").unwrap(),
            ClaimOutcome::Claimed,
            "a stale in-progress claim must be reclaimable"
        );
    }
}
