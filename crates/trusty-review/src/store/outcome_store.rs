//! redb-backed finding-outcome persistence store (issue #1421).
//!
//! Why: persisting per-finding outcomes (accepted/acted-on/dismissed/ignored)
//! enables the suppression-list pipeline — chronically-dismissed finding kinds
//! can be fed back as "what NOT to flag" guardrails (epic #1413, child #1421).
//! A redb store mirrors the DedupStore pattern for consistency and cross-process
//! durability.
//!
//! What: `OutcomeStore` wraps a redb database with one table keyed by
//! `finding_hash`.  `record` persists a `FindingOutcome`; `dismissed_patterns`
//! returns finding `kind`s dismissed more than `threshold` times.
//!
//! Fail-safe: every method returns a typed `OutcomeError`, but callers are
//! expected to *log and proceed* — a store failure must never crash or block
//! a review or outcome-poll cycle.
//!
//! Test: `record_and_dismissed_patterns_threshold`, `record_below_threshold`,
//! `multiple_kinds_independent`.

use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::integrations::github::outcomes::{FindingOutcome, Outcome};

/// redb table: finding_hash → serialised `OutcomeRecord` (JSON).
///
/// Why: a single table keyed by the stable finding hash is the simplest durable
/// shape — one row per finding, updated in place on each observation.
/// What: key is the hex SHA-256 `finding_hash`; value is JSON-encoded `OutcomeRecord`.
/// Test: exercised by all store tests.
const OUTCOMES: TableDefinition<&str, &str> = TableDefinition::new("finding_outcomes");

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Errors produced by the outcome store.
///
/// Why: a typed enum lets callers distinguish "store unavailable" from
/// "serialisation bug" in logs, even though the policy for both is the same
/// (log + proceed).
/// What: wraps redb's database/transaction/table/commit errors plus JSON
/// (de)serialisation failures.
/// Test: error variants are surfaced via the public methods; `Display` is
/// derived by thiserror.
#[derive(Debug, thiserror::Error)]
pub enum OutcomeError {
    /// Opening or creating the redb database failed.
    #[error("outcome store open failed: {0}")]
    Open(String),
    /// A read/write transaction failed.
    #[error("outcome store transaction failed: {0}")]
    Transaction(String),
    /// Serialising or deserialising a record failed.
    #[error("outcome store (de)serialisation failed: {0}")]
    Serde(String),
}

// ─── Record type ─────────────────────────────────────────────────────────────

/// A stored outcome record for a single finding.
///
/// Why: persists the outcome signal alongside context (kind, timestamp) needed
/// for the suppression-list query without requiring a secondary index.
/// What: `finding_hash` is the redb key (also stored in value for self-containedness);
/// `kind` supports the `dismissed_patterns` query; `outcome` is the signal;
/// `count` tracks how many times this outcome was observed (for aggregation).
/// Test: round-tripped through JSON by every store method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRecord {
    /// Stable fingerprint of the finding.
    pub finding_hash: String,
    /// Finding kind (e.g. `"security"`, `"logic-error"`).
    pub kind: String,
    /// Most-recently observed outcome.
    pub outcome: Outcome,
    /// Number of times this hash has been recorded as `Dismissed`.
    pub dismissed_count: u32,
    /// Number of times this hash has been recorded total.
    pub total_count: u32,
    /// ISO-8601 UTC timestamp of the last update.
    pub last_updated: String,
}

// ─── Store ────────────────────────────────────────────────────────────────────

/// A redb-backed finding-outcome persistence store.
///
/// Why: provides cross-process, durable outcome tracking so the suppression-list
/// pipeline can aggregate dismissed patterns across multiple reviews.
/// What: owns a redb `Database`; all methods open short transactions so the
/// store is safe to share across tasks behind an `Arc`.
/// Test: see module-level tests, all of which use a tempfile-backed store.
pub struct OutcomeStore {
    db: Database,
}

impl OutcomeStore {
    /// Open (or create) the outcome store at `path`.
    ///
    /// Why: the store lives under the review log dir alongside `dedup.redb`,
    /// persisting across daemon restarts.
    /// What: creates the redb database file and ensures the outcomes table exists.
    /// On incompatible-format errors, moves the old file aside and creates fresh
    /// (same recovery pattern as `DedupStore`).
    /// Test: `open_creates_file`.
    pub fn open(path: &Path) -> Result<Self, OutcomeError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let db = Self::open_or_recreate(path)?;
        {
            let write = db
                .begin_write()
                .map_err(|e| OutcomeError::Transaction(e.to_string()))?;
            {
                write
                    .open_table(OUTCOMES)
                    .map_err(|e| OutcomeError::Transaction(e.to_string()))?;
            }
            write
                .commit()
                .map_err(|e| OutcomeError::Transaction(e.to_string()))?;
        }
        Ok(Self { db })
    }

    fn open_or_recreate(path: &Path) -> Result<Database, OutcomeError> {
        match Database::create(path) {
            Ok(db) => Ok(db),
            Err(e) if crate::store::redb_error_is_incompatible_format(&e) => {
                let mut backup = path.as_os_str().to_os_string();
                backup.push(".v2-incompatible");
                let backup = std::path::PathBuf::from(backup);
                let _ = std::fs::rename(path, &backup);
                Database::create(path).map_err(|e| OutcomeError::Open(e.to_string()))
            }
            Err(e) => Err(OutcomeError::Open(e.to_string())),
        }
    }

    /// Record a finding outcome, updating the stored record for `outcome.finding_hash`.
    ///
    /// Why: accumulates outcome observations over time so `dismissed_patterns`
    /// can aggregate chronically-dismissed finding kinds.
    /// What: reads any existing record for the hash; increments `dismissed_count`
    /// when the outcome is `Dismissed`; always increments `total_count`; writes
    /// back atomically.
    /// Test: `record_and_dismissed_patterns_threshold`.
    pub fn record(&self, outcome: &FindingOutcome) -> Result<(), OutcomeError> {
        let write = self
            .db
            .begin_write()
            .map_err(|e| OutcomeError::Transaction(e.to_string()))?;
        {
            let mut table = write
                .open_table(OUTCOMES)
                .map_err(|e| OutcomeError::Transaction(e.to_string()))?;

            let key = outcome.finding_hash.as_str();
            let existing = table
                .get(key)
                .map_err(|e| OutcomeError::Transaction(e.to_string()))?
                .map(|v| v.value().to_string());

            let mut record = if let Some(raw) = existing {
                serde_json::from_str::<OutcomeRecord>(&raw)
                    .map_err(|e| OutcomeError::Serde(e.to_string()))?
            } else {
                OutcomeRecord {
                    finding_hash: outcome.finding_hash.clone(),
                    kind: outcome.kind.clone(),
                    outcome: outcome.outcome,
                    dismissed_count: 0,
                    total_count: 0,
                    last_updated: outcome.timestamp.clone(),
                }
            };

            if outcome.outcome == Outcome::Dismissed {
                record.dismissed_count = record.dismissed_count.saturating_add(1);
            }
            record.total_count = record.total_count.saturating_add(1);
            record.outcome = outcome.outcome;
            record.last_updated = outcome.timestamp.clone();

            let json =
                serde_json::to_string(&record).map_err(|e| OutcomeError::Serde(e.to_string()))?;
            table
                .insert(key, json.as_str())
                .map_err(|e| OutcomeError::Transaction(e.to_string()))?;
        }
        write
            .commit()
            .map_err(|e| OutcomeError::Transaction(e.to_string()))?;
        Ok(())
    }

    /// Return finding `kind`s that have been dismissed more than `threshold` times.
    ///
    /// Why: the suppression-list pipeline feeds these kinds back into the prompt
    /// as "what NOT to flag" guardrails (issue #1421); the prompt injection is a
    /// follow-up (PR F) — this method exposes the aggregated data.
    /// What: scans the full `OUTCOMES` table, groups records by `kind`, sums
    /// `dismissed_count` per kind, and returns kinds whose sum is **at least**
    /// `threshold` (`dismissed_count >= threshold`).  A kind dismissed exactly
    /// `threshold` times IS returned — the threshold is inclusive (N or more).
    /// Test: `record_and_dismissed_patterns_threshold`, `record_below_threshold`.
    pub fn dismissed_patterns(&self, threshold: u32) -> Result<Vec<String>, OutcomeError> {
        let read = self
            .db
            .begin_read()
            .map_err(|e| OutcomeError::Transaction(e.to_string()))?;
        let table = read
            .open_table(OUTCOMES)
            .map_err(|e| OutcomeError::Transaction(e.to_string()))?;

        let mut kind_counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        for item in table
            .iter()
            .map_err(|e| OutcomeError::Transaction(e.to_string()))?
        {
            let (_, v) = item.map_err(|e| OutcomeError::Transaction(e.to_string()))?;
            let record: OutcomeRecord =
                serde_json::from_str(v.value()).map_err(|e| OutcomeError::Serde(e.to_string()))?;
            if record.dismissed_count > 0 {
                *kind_counts.entry(record.kind).or_default() += record.dismissed_count;
            }
        }

        let mut result: Vec<String> = kind_counts
            .into_iter()
            .filter(|(_, count)| *count >= threshold)
            .map(|(kind, _)| kind)
            .collect();
        result.sort();
        Ok(result)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::github::outcomes::FindingOutcome;

    fn temp_store() -> (OutcomeStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("outcomes.redb");
        let store = OutcomeStore::open(&path).expect("open store");
        (store, dir)
    }

    fn make_outcome(hash: &str, kind: &str, outcome: Outcome) -> FindingOutcome {
        FindingOutcome {
            finding_hash: hash.to_string(),
            kind: kind.to_string(),
            outcome,
            timestamp: "2026-06-23T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn open_creates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("outcomes.redb");
        let _store = OutcomeStore::open(&path).expect("open");
        assert!(path.exists(), "redb file must be created");
    }

    #[test]
    fn record_and_dismissed_patterns_threshold() {
        let (store, _d) = temp_store();

        for i in 0..6u32 {
            let o = make_outcome(&format!("hash{i}"), "nit", Outcome::Dismissed);
            store.record(&o).expect("record");
        }
        for i in 0..3u32 {
            let o = make_outcome(&format!("sec{i}"), "security", Outcome::Dismissed);
            store.record(&o).expect("record");
        }

        let patterns = store.dismissed_patterns(5).expect("query");
        assert!(
            patterns.contains(&"nit".to_string()),
            "nit should exceed threshold 5"
        );
        assert!(
            !patterns.contains(&"security".to_string()),
            "security should be below threshold"
        );
    }

    #[test]
    fn record_below_threshold_not_returned() {
        let (store, _d) = temp_store();
        let o = make_outcome("hash1", "style", Outcome::Dismissed);
        store.record(&o).expect("record");
        let patterns = store.dismissed_patterns(5).expect("query");
        assert!(patterns.is_empty(), "one dismissal is below threshold 5");
    }

    #[test]
    fn non_dismissed_outcomes_not_counted() {
        let (store, _d) = temp_store();
        for i in 0..10u32 {
            let o = make_outcome(&format!("h{i}"), "perf", Outcome::Accepted);
            store.record(&o).expect("record");
        }
        let patterns = store.dismissed_patterns(1).expect("query");
        assert!(
            patterns.is_empty(),
            "accepted outcomes must not count as dismissed"
        );
    }

    #[test]
    fn multiple_kinds_independent() {
        let (store, _d) = temp_store();
        for i in 0..6u32 {
            let o = make_outcome(&format!("s{i}"), "security", Outcome::Dismissed);
            store.record(&o).expect("record");
        }
        for i in 0..2u32 {
            let o = make_outcome(&format!("n{i}"), "nit", Outcome::Dismissed);
            store.record(&o).expect("record");
        }
        let patterns = store.dismissed_patterns(5).expect("query");
        assert!(patterns.contains(&"security".to_string()));
        assert!(!patterns.contains(&"nit".to_string()));
    }

    #[test]
    fn record_same_hash_twice_increments_count() {
        let (store, _d) = temp_store();
        let o = make_outcome("dup_hash", "security", Outcome::Dismissed);
        store.record(&o).expect("first record");
        store.record(&o).expect("second record");
        let patterns = store.dismissed_patterns(1).expect("query");
        assert!(
            patterns.contains(&"security".to_string()),
            "2 > 1 threshold"
        );
    }

    #[test]
    fn dismissed_patterns_at_exact_threshold_included() {
        let (store, _d) = temp_store();
        for i in 0..5u32 {
            let o = make_outcome(&format!("h{i}"), "style", Outcome::Dismissed);
            store.record(&o).expect("record");
        }
        let patterns = store.dismissed_patterns(5).expect("query");
        assert!(
            patterns.contains(&"style".to_string()),
            "count == threshold (5) IS returned — threshold is inclusive"
        );
    }
}
