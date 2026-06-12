//! Hit/miss tracking for recall optimization.
//!
//! Why: Without feedback, importance scores are static. RecallLog closes the
//! loop — frequently recalled drawers are demonstrably more useful.
//! What: redb-backed event log with hit_count, miss_rate, top_drawers queries.
//! Issue #57 migrates the storage layer from rusqlite + r2d2 to redb so the
//! analytics sidecar drops the heavy SQLite dependency chain and lines up with
//! the rest of the Memory Palace (`kg_redb.rs`, payload_store). The public
//! `RecallLog` API is unchanged — callers that previously pointed at
//! `<data_dir>/recall.db` keep working; the file on disk becomes
//! `<data_dir>/recall.redb`. The one-shot SQLite → redb migration path was
//! removed in issue #989 (all palaces confirmed migrated).
//! NLP: Query normalization via stop-word removal + FNV-1a hash. Zero inference.
//! Test: record then hit_count, miss_rate 1.0 when all miss, top_drawers ordering,
//! plus reopen round-trip (events survive across reopens).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use redb::{Database, ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

use crate::memory_core::store::kg_store::RECALL_LOG;

// ── Query normalization (NLP — no inference) ─────────────────────────────────

/// English stop words removed during query normalization.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "in", "on", "at", "to", "of", "for", "with",
    "by", "from", "and", "or", "but", "not", "it", "this", "that", "be", "as", "do", "did", "has",
    "have", "had",
];

/// Normalize a query: lowercase, strip punctuation, collapse whitespace,
/// remove stop words.
///
/// Why: Two semantically equivalent queries should hash to the same bucket.
/// What: Pure string transformation — no embeddings, no inference.
/// Test: "The quick Brown Fox!" → "quick brown fox"
pub fn normalize_query(text: &str) -> String {
    let lower = text.to_lowercase();
    let no_punct: String = lower
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let words: Vec<&str> = no_punct
        .split_whitespace()
        .filter(|w| !STOP_WORDS.contains(w))
        .collect();
    words.join(" ")
}

/// FNV-1a 64-bit hash — deterministic, zero dependencies.
///
/// Why: We need a stable u64 key for query grouping without an extra crate.
/// What: Standard FNV-1a algorithm over UTF-8 bytes.
/// Test: same input yields same hash; different inputs differ.
pub fn fnv1a_hash(text: &str) -> u64 {
    const OFFSET: u64 = 14695981039346656037;
    const PRIME: u64 = 1099511628211;
    let mut hash = OFFSET;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Hash a query after normalization.
///
/// Why: Group semantically equivalent queries under one stable key.
/// What: normalize_query then fnv1a_hash.
/// Test: "The cat" and "a cat" both hash to fnv1a_hash("cat").
pub fn query_hash(text: &str) -> u64 {
    fnv1a_hash(&normalize_query(text))
}

// ── Types ────────────────────────────────────────────────────────────────────

/// One recall event row, postcard-encoded as the value in the RECALL_LOG table.
///
/// Why: Persisting the structured event (rather than ad-hoc columns) lets us
/// add fields without a schema migration — postcard reads ignore missing
/// trailing fields. The serde derives are required for postcard encoding.
/// What: Same fields the SQLite implementation carried, with `serde::Serialize`
/// / `Deserialize` added.
/// Test: `record_then_hit_count`, `roundtrip_persists_across_reopen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallEvent {
    pub palace_id: String,
    pub query_hash: u64,
    pub layer: u8,
    /// None = miss (query returned 0 results)
    pub drawer_id: Option<Uuid>,
    pub score: f32,
    pub occurred_at: DateTime<Utc>,
}

// ── RecallLog ────────────────────────────────────────────────────────────────

/// redb-backed hit/miss event log.
///
/// Why: Provides durable analytics for the Memory Palace without the rusqlite
/// dependency chain. The public surface (`record`, `hit_count`, `miss_rate`,
/// `top_drawers`, `missed_queries`) is preserved as-is so retrieval.rs and the
/// CLI/MCP plumbing keep working unchanged.
/// What: Owns an `Arc<redb::Database>` over a single `recall.redb` file. Each
/// `record` writes one row into the RECALL_LOG table; the read methods range-
/// scan the whole table (the log is bounded by retention windows / palace
/// sizes; if it ever grows enough to matter we'll add secondary indexes).
/// Test: see the `tests` module below — round-trip persistence, hit/miss/
/// drawer/missed-query queries.
pub struct RecallLog {
    db: Arc<Database>,
    path: PathBuf,
    /// Monotonic event-id source — guarantees unique keys even when multiple
    /// `record` calls land inside the same millisecond.
    next_id: AtomicU64,
}

impl RecallLog {
    /// Open (or create) a recall log at `path`.
    ///
    /// Why: Sharing the palace directory keeps everything in one place. The
    /// legacy SQLite path (`recall.db`) is silently rewritten to `recall.redb`
    /// so retrieval.rs's existing call site (`<data_dir>/recall.db`) keeps
    /// working without churn. The one-shot SQLite → redb migration was removed
    /// in issue #989 (all palaces confirmed migrated).
    /// What: Resolves the redb path, creates parent dirs, opens the redb
    /// database, touches the RECALL_LOG table so range scans on a fresh file
    /// succeed, then seeds the `next_id` counter from the highest existing key
    /// so monotonicity holds across reopens.
    /// Test: `record_then_hit_count`, `roundtrip_persists_across_reopen`.
    pub fn open(path: &Path) -> Result<Self> {
        let redb_path = resolve_redb_path(path);

        if let Some(parent) = redb_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create recall log parent dir {}",
                    parent.display()
                )
            })?;
        }

        // Note: the one-shot SQLite → redb migration (formerly gated behind
        // `sqlite-kg`) was removed in issue #989 — all palaces are confirmed
        // migrated. No migration step runs here.

        let db = super::store::open_or_recreate(&redb_path).with_context(|| {
            format!("failed to open redb recall log at {}", redb_path.display())
        })?;

        // Touch the table and discover the highest existing event id in one
        // write transaction so subsequent reads on a brand-new file succeed
        // and so we can seed the in-process monotonic counter.
        let mut max_seen: u64 = 0;
        {
            let wtx = db
                .begin_write()
                .context("failed to begin write txn for recall log init")?;
            {
                let table = wtx
                    .open_table(RECALL_LOG)
                    .context("failed to open RECALL_LOG table")?;
                // redb's BTreeMap-backed tables sort numerically for u64 keys,
                // so the last entry has the max id.
                if let Some(entry) = table
                    .last()
                    .context("failed to read last key from RECALL_LOG")?
                {
                    max_seen = entry.0.value();
                }
            }
            wtx.commit().context("failed to commit recall log init")?;
        }

        Ok(Self {
            db: Arc::new(db),
            path: redb_path,
            next_id: AtomicU64::new(max_seen),
        })
    }

    /// Allocate the next monotonic event id.
    ///
    /// Why: Multiple recall events can land in the same millisecond. Using the
    /// wall-clock alone would collide and overwrite previously persisted rows;
    /// combining the wall-clock with an in-process counter keeps keys unique
    /// while still preserving sort-by-insertion-order for range scans.
    /// What: Returns `max(now_ms, last_id + 1)`. The counter persists across
    /// inserts via `next_id` and is seeded from the highest existing key on
    /// open.
    /// Test: covered indirectly by `record_then_hit_count` (multiple records
    /// in the same ms) and `roundtrip_persists_across_reopen`.
    fn alloc_id(&self) -> u64 {
        let now_ms = Utc::now().timestamp_millis().max(0) as u64;
        loop {
            let current = self.next_id.load(Ordering::Acquire);
            let candidate = now_ms.max(current + 1);
            if self
                .next_id
                .compare_exchange(current, candidate, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return candidate;
            }
        }
    }

    /// Record a recall event.
    ///
    /// Why: Persist hit/miss feedback to drive importance updates and gap
    /// detection. `tokio::task::spawn_blocking` preserves the original async
    /// signature so retrieval.rs and friends are unaffected by the redb
    /// migration.
    /// What: Allocates a unique event id, postcard-encodes the event, and
    /// writes one row into the RECALL_LOG table under a single write txn.
    /// Test: `record_then_hit_count`, `roundtrip_persists_across_reopen`.
    pub async fn record(&self, event: RecallEvent) -> Result<()> {
        let id = self.alloc_id();
        let bytes =
            postcard::to_allocvec(&event).context("failed to postcard-encode RecallEvent")?;
        let db = self.db.clone();
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let wtx = db
                .begin_write()
                .with_context(|| format!("begin_write recall log {}", path.display()))?;
            {
                let mut table = wtx
                    .open_table(RECALL_LOG)
                    .context("open RECALL_LOG table")?;
                table
                    .insert(id, bytes.as_slice())
                    .context("insert RecallEvent row")?;
            }
            wtx.commit().context("commit RecallEvent write")?;
            Ok(())
        })
        .await
        .context("record task join error")??;
        Ok(())
    }

    /// Snapshot every event currently in the log.
    ///
    /// Why: All read methods are full scans — bounded by retention/window — so
    /// we share a single snapshot helper rather than duplicate the txn / decode
    /// dance per method.
    /// What: Opens a read transaction, decodes each row into `RecallEvent`,
    /// returns them in key order (insertion order).
    /// Test: covered by every public read method's tests.
    // Retained as the shared full-scan helper for the read methods; not all
    // build configurations exercise a caller, so silence the dead-code lint
    // rather than delete the documented shared infrastructure.
    #[allow(dead_code)]
    fn snapshot(&self) -> Result<Vec<RecallEvent>> {
        let db = self.db.clone();
        let path = self.path.clone();
        let rtx = db
            .begin_read()
            .with_context(|| format!("begin_read recall log {}", path.display()))?;
        let table = rtx
            .open_table(RECALL_LOG)
            .context("open RECALL_LOG table (read)")?;
        let mut out = Vec::new();
        for entry in table.iter().context("iter RECALL_LOG")? {
            let (_k, v) = entry.context("decode RECALL_LOG row")?;
            let ev: RecallEvent =
                postcard::from_bytes(v.value()).context("postcard decode RecallEvent")?;
            out.push(ev);
        }
        Ok(out)
    }

    /// Total hit count for a specific drawer.
    ///
    /// Why: Frequently recalled drawers should bubble up in importance.
    /// What: Snapshot then count events whose `drawer_id == Some(drawer_id)`.
    /// Test: record twice for same drawer → hit_count == 2.
    pub async fn hit_count(&self, drawer_id: Uuid) -> Result<u64> {
        let events = self.snapshot_async().await?;
        let mut count: u64 = 0;
        for ev in events {
            if ev.drawer_id == Some(drawer_id) {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Fraction of distinct queries in last `window_days` that returned 0 results.
    ///
    /// Why: High miss rate signals knowledge gaps in the palace.
    /// What: distinct miss queries / distinct total queries within window.
    /// Test: only-miss events → 1.0; only-hit events → 0.0.
    pub async fn miss_rate(&self, palace_id: &str, window_days: u32) -> Result<f32> {
        let events = self.snapshot_async().await?;
        let since = Utc::now() - chrono::Duration::days(window_days as i64);
        use std::collections::HashSet;
        let mut total: HashSet<u64> = HashSet::new();
        let mut misses: HashSet<u64> = HashSet::new();
        for ev in events {
            if ev.palace_id != palace_id || ev.occurred_at < since {
                continue;
            }
            total.insert(ev.query_hash);
            if ev.drawer_id.is_none() {
                misses.insert(ev.query_hash);
            }
        }
        if total.is_empty() {
            return Ok(0.0);
        }
        Ok(misses.len() as f32 / total.len() as f32)
    }

    /// Top drawers by hit count.
    ///
    /// Why: Identify the most-valuable drawers to promote in L1.
    /// What: Group events by drawer_id (palace-filtered), sort by hit count
    /// descending, return top `limit`.
    /// Test: drawer with 3 hits ranks above drawer with 1 hit.
    pub async fn top_drawers(&self, palace_id: &str, limit: usize) -> Result<Vec<(Uuid, u64)>> {
        let events = self.snapshot_async().await?;
        use std::collections::HashMap;
        let mut counts: HashMap<Uuid, u64> = HashMap::new();
        for ev in events {
            if ev.palace_id != palace_id {
                continue;
            }
            if let Some(id) = ev.drawer_id {
                *counts.entry(id).or_insert(0) += 1;
            }
        }
        let mut out: Vec<(Uuid, u64)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(limit);
        Ok(out)
    }

    /// Most-missed query hashes (queries that returned 0 results).
    ///
    /// Why: Surfaces knowledge gaps so users can fill them.
    /// What: Group events where `drawer_id is None` by `query_hash`
    /// (palace-filtered), sort by miss count descending, return top `limit`.
    /// Test: query missed 3 times ranks above query missed 1 time.
    pub async fn missed_queries(&self, palace_id: &str, limit: usize) -> Result<Vec<(u64, u64)>> {
        let events = self.snapshot_async().await?;
        use std::collections::HashMap;
        let mut counts: HashMap<u64, u64> = HashMap::new();
        for ev in events {
            if ev.palace_id != palace_id || ev.drawer_id.is_some() {
                continue;
            }
            *counts.entry(ev.query_hash).or_insert(0) += 1;
        }
        let mut out: Vec<(u64, u64)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(limit);
        Ok(out)
    }

    /// Async-friendly snapshot helper.
    ///
    /// Why: Keep blocking redb work off the async runtime; the read methods
    /// remain `async fn` so retrieval.rs callers are unchanged.
    /// What: Wraps `snapshot()` in `tokio::task::spawn_blocking`.
    /// Test: indirectly covered by every public read-method test.
    async fn snapshot_async(&self) -> Result<Vec<RecallEvent>> {
        let db = self.db.clone();
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<RecallEvent>> {
            let rtx = db
                .begin_read()
                .with_context(|| format!("begin_read recall log {}", path.display()))?;
            let table = rtx
                .open_table(RECALL_LOG)
                .context("open RECALL_LOG table (read)")?;
            let mut out = Vec::new();
            for entry in table.iter().context("iter RECALL_LOG")? {
                let (_k, v) = entry.context("decode RECALL_LOG row")?;
                let ev: RecallEvent =
                    postcard::from_bytes(v.value()).context("postcard decode RecallEvent")?;
                out.push(ev);
            }
            Ok(out)
        })
        .await
        .context("snapshot task join error")?
    }
}

/// Internal: callers historically passed `<data_root>/recall.db` for the
/// SQLite sidecar. Now that the store is redb-backed, accept that same path
/// and silently rewrite it to `recall.redb` so existing call sites continue
/// to work. Paths with any other extension (or no extension) are kept as-is.
///
/// Why: keeps retrieval.rs's `<data_dir>/recall.db` join unchanged.
/// What: rewrites `.db` → `.redb`, leaves everything else alone.
/// Test: `callers_passing_recall_db_get_redb_sibling`.
fn resolve_redb_path(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e == "db") {
        path.with_extension("redb")
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn normalize_removes_stop_words() {
        assert_eq!(normalize_query("The quick Brown Fox!"), "quick brown fox");
    }

    #[test]
    fn normalize_strips_punctuation() {
        assert_eq!(normalize_query("  Rust  is  fast!  "), "rust fast");
    }

    #[test]
    fn fnv1a_is_deterministic() {
        assert_eq!(fnv1a_hash("hello"), fnv1a_hash("hello"));
        assert_ne!(fnv1a_hash("hello"), fnv1a_hash("world"));
    }

    #[tokio::test]
    async fn record_then_hit_count() {
        let dir = tempdir().unwrap();
        let log = RecallLog::open(&dir.path().join("recall.db")).unwrap();
        let id = Uuid::new_v4();
        log.record(RecallEvent {
            palace_id: "test".into(),
            query_hash: query_hash("vector store"),
            layer: 2,
            drawer_id: Some(id),
            score: 0.9,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
        log.record(RecallEvent {
            palace_id: "test".into(),
            query_hash: query_hash("vector store"),
            layer: 2,
            drawer_id: Some(id),
            score: 0.85,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
        assert_eq!(log.hit_count(id).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn miss_rate_all_miss() {
        let dir = tempdir().unwrap();
        let log = RecallLog::open(&dir.path().join("recall.db")).unwrap();
        log.record(RecallEvent {
            palace_id: "test".into(),
            query_hash: query_hash("unknown topic"),
            layer: 3,
            drawer_id: None,
            score: 0.0,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
        let rate = log.miss_rate("test", 7).await.unwrap();
        assert!((rate - 1.0).abs() < 1e-4, "expected 1.0 got {rate}");
    }

    #[tokio::test]
    async fn miss_rate_all_hit() {
        let dir = tempdir().unwrap();
        let log = RecallLog::open(&dir.path().join("recall.db")).unwrap();
        log.record(RecallEvent {
            palace_id: "test".into(),
            query_hash: query_hash("rust"),
            layer: 2,
            drawer_id: Some(Uuid::new_v4()),
            score: 0.9,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
        let rate = log.miss_rate("test", 7).await.unwrap();
        assert!((rate - 0.0).abs() < 1e-4, "expected 0.0 got {rate}");
    }

    #[tokio::test]
    async fn top_drawers_sorted_by_hits() {
        let dir = tempdir().unwrap();
        let log = RecallLog::open(&dir.path().join("recall.db")).unwrap();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        for _ in 0..3 {
            log.record(RecallEvent {
                palace_id: "test".into(),
                query_hash: 1,
                layer: 2,
                drawer_id: Some(id_a),
                score: 0.9,
                occurred_at: Utc::now(),
            })
            .await
            .unwrap();
        }
        log.record(RecallEvent {
            palace_id: "test".into(),
            query_hash: 2,
            layer: 2,
            drawer_id: Some(id_b),
            score: 0.8,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
        let top = log.top_drawers("test", 5).await.unwrap();
        assert_eq!(top[0].0, id_a);
        assert_eq!(top[0].1, 3);
        assert_eq!(top[1].0, id_b);
    }

    #[tokio::test]
    async fn missed_queries_returns_most_missed_first() {
        let dir = tempdir().unwrap();
        let log = RecallLog::open(&dir.path().join("recall.db")).unwrap();
        let h1 = query_hash("obscure topic");
        let h2 = query_hash("another gap");
        for _ in 0..3 {
            log.record(RecallEvent {
                palace_id: "test".into(),
                query_hash: h1,
                layer: 3,
                drawer_id: None,
                score: 0.0,
                occurred_at: Utc::now(),
            })
            .await
            .unwrap();
        }
        log.record(RecallEvent {
            palace_id: "test".into(),
            query_hash: h2,
            layer: 3,
            drawer_id: None,
            score: 0.0,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
        let missed = log.missed_queries("test", 5).await.unwrap();
        assert_eq!(missed[0].0, h1);
        assert_eq!(missed[0].1, 3);
    }

    #[tokio::test]
    async fn roundtrip_persists_across_reopen() {
        // Why: redb migration is only useful if events survive a reopen — the
        // SQLite implementation did, the new one must too.
        let dir = tempdir().unwrap();
        let path = dir.path().join("recall.db");
        let id = Uuid::new_v4();
        {
            let log = RecallLog::open(&path).unwrap();
            log.record(RecallEvent {
                palace_id: "test".into(),
                query_hash: 42,
                layer: 2,
                drawer_id: Some(id),
                score: 0.5,
                occurred_at: Utc::now(),
            })
            .await
            .unwrap();
        }
        // Reopen — the persisted event must still be there.
        let log2 = RecallLog::open(&path).unwrap();
        assert_eq!(log2.hit_count(id).await.unwrap(), 1);
        // And new inserts must not collide with the seeded id.
        log2.record(RecallEvent {
            palace_id: "test".into(),
            query_hash: 42,
            layer: 2,
            drawer_id: Some(id),
            score: 0.7,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
        assert_eq!(log2.hit_count(id).await.unwrap(), 2);
    }

    #[test]
    fn callers_passing_recall_db_get_redb_sibling() {
        // Existing callers pass `recall.db`; the resolver must redirect to
        // `recall.redb` so on-disk storage is actually redb.
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("recall.db");
        let _log = RecallLog::open(&legacy).unwrap();
        let redb_path = dir.path().join("recall.redb");
        assert!(
            redb_path.exists(),
            "expected redb sibling to be created at {}",
            redb_path.display()
        );
    }
}
