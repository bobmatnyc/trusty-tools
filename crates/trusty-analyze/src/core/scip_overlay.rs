//! Durable per-index SCIP graph overlay store, redb-backed.
//!
//! Why: `POST /indexes/{id}/scip` used to write into an in-process
//! `HashMap<String, KgGraph>`, so a daemon restart discarded an ingest that
//! had already answered HTTP 200 — and `GET /indexes/{id}/graph` then served
//! a tree-sitter-only graph no caller could tell apart from one where the
//! overlay had been applied. SCIP payloads are uploaded by the operator and
//! cannot be re-derived from the corpus, so losing one loses real work. See
//! #5049.
//!
//! What: a redb table from `index_id (&str)` → JSON-encoded
//! [`ScipOverlayRecord`]. `get` returning `Option` is the empty-vs-absent
//! distinction the daemon exposes over HTTP: `None` means nobody has ingested
//! SCIP for this index, `Some(record)` whose graph has zero nodes means
//! somebody ingested a SCIP index that contained no symbols.
//!
//! Unlike [`crate::core::FactStore`], an unreadable database here is NOT
//! recreated empty. Facts are re-derivable from source; an uploaded SCIP
//! index is not, and silently replacing it with an empty store is the same
//! silent data loss this module exists to fix.
//!
//! Test: `overlay_round_trips_across_reopen`, `absent_overlay_reads_as_none`,
//! `empty_overlay_is_distinguishable_from_absent`, `put_overwrites_prior_overlay`.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::types::KgGraph;

const SCIP_OVERLAY_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("scip_overlays");

/// One index's ingested SCIP overlay plus the time it was ingested.
///
/// Why: callers need more than the graph itself to trust a `/graph` response —
/// "when did this overlay land" is what tells an operator whether the ingest
/// they just ran is the one being served.
/// What: the owning index id, the extracted `KgGraph`, and a Unix-seconds
/// ingest timestamp. Serialized as JSON into redb.
/// Test: `overlay_round_trips_across_reopen`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScipOverlayRecord {
    pub index_id: String,
    pub graph: KgGraph,
    /// Unix seconds at which `put` last wrote this overlay.
    pub ingested_at: u64,
}

/// Derive the overlay database path that pairs with a given facts-store path.
///
/// Why: the daemon already resolves exactly one data location — `--facts-path`
/// (default `~/.trusty-tools/analyze/facts.redb`). Deriving the overlay path
/// from it keeps a single knob governing the whole data directory instead of
/// adding a second flag operators can set inconsistently.
/// What: replaces the file name with `scip_overlays.redb`, leaving the parent
/// directory untouched.
/// Test: `overlay_path_is_a_sibling_of_the_facts_store`.
pub fn overlay_path_beside_facts(facts_path: &Path) -> std::path::PathBuf {
    facts_path.with_file_name("scip_overlays.redb")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// redb-backed store for per-index SCIP overlays. Cheap to clone —
/// `Arc<Database>`.
#[derive(Clone)]
pub struct ScipOverlayStore {
    db: Arc<Database>,
}

impl ScipOverlayStore {
    /// Open (creating if absent) the overlay redb at `path`.
    ///
    /// Why: the daemon must fail to start rather than serve a store it could
    /// not read — an unreadable overlay database means every subsequent
    /// `/graph` response would silently drop overlays that are still on disk,
    /// which is exactly the failure mode #5049 is about.
    /// What: `Database::create` plus a write txn that materialises the table so
    /// reads against a brand-new file succeed. Any open error propagates.
    /// Test: `overlay_round_trips_across_reopen`, `corrupt_db_fails_open_loudly`.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::create(path)
            .with_context(|| format!("open SCIP overlay redb at {}", path.display()))?;
        let txn = db.begin_write().context("begin overlay init txn")?;
        {
            let _t = txn
                .open_table(SCIP_OVERLAY_TABLE)
                .context("open scip_overlays table for init")?;
        }
        txn.commit().context("commit overlay init txn")?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Store `graph` as the overlay for `index_id`, replacing any prior one.
    ///
    /// Why: a re-ingest supersedes the previous SCIP index rather than merging
    /// with it — SCIP output is a whole-repository snapshot, so unioning two
    /// generations would resurrect symbols the newer index deliberately dropped.
    /// What: writes a fresh [`ScipOverlayRecord`] with `ingested_at = now` and
    /// commits. Returns the record so the handler can report it without a
    /// second read.
    /// Test: `put_overwrites_prior_overlay`.
    pub fn put(&self, index_id: &str, graph: KgGraph) -> Result<ScipOverlayRecord> {
        let record = ScipOverlayRecord {
            index_id: index_id.to_string(),
            graph,
            ingested_at: now_secs(),
        };
        let bytes = serde_json::to_vec(&record).context("encode SCIP overlay")?;
        let txn = self.db.begin_write().context("begin overlay put txn")?;
        {
            let mut table = txn
                .open_table(SCIP_OVERLAY_TABLE)
                .context("open scip_overlays table for put")?;
            table
                .insert(index_id, bytes.as_slice())
                .context("insert SCIP overlay")?;
        }
        txn.commit().context("commit overlay put txn")?;
        Ok(record)
    }

    /// Read the overlay for `index_id`.
    ///
    /// Why: `None` versus `Some(record)` is the distinction the whole fix turns
    /// on. A caller that gets an empty `/graph` needs to know whether no SCIP
    /// data exists for the index at all, or whether an ingested SCIP index
    /// simply carried no symbols.
    /// What: `Ok(None)` when the key is absent; `Ok(Some(record))` otherwise. A
    /// decode failure is an error, never a silent `None` — a row that exists but
    /// cannot be parsed is data loss, not absence.
    /// Test: `absent_overlay_reads_as_none`,
    /// `empty_overlay_is_distinguishable_from_absent`.
    pub fn get(&self, index_id: &str) -> Result<Option<ScipOverlayRecord>> {
        let txn = self.db.begin_read().context("begin overlay read txn")?;
        let table = txn
            .open_table(SCIP_OVERLAY_TABLE)
            .context("open scip_overlays table for read")?;
        let Some(raw) = table.get(index_id).context("read SCIP overlay row")? else {
            return Ok(None);
        };
        let record: ScipOverlayRecord = serde_json::from_slice(raw.value())
            .with_context(|| format!("decode SCIP overlay for index {index_id}"))?;
        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{KgNode, KgNodeKind};
    use tempfile::TempDir;

    fn node(id: &str) -> KgNode {
        KgNode {
            id: id.into(),
            kind: KgNodeKind::Function,
            name: id.into(),
            qualified_name: id.into(),
            language: "rust".into(),
            file: "src/lib.rs".into(),
            start_line: 1,
            end_line: 2,
            doc_comment: None,
            is_public: true,
            extra: serde_json::Value::Null,
        }
    }

    /// Why: #5049 — the whole defect is that an ingest does not survive the
    /// process that accepted it. Reopening the same file is the in-process
    /// stand-in for a daemon restart.
    /// What: writes an overlay, drops the store, reopens the same path, and
    /// asserts the graph is still there.
    /// Test: this test.
    #[test]
    fn overlay_round_trips_across_reopen() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("scip_overlays.redb");

        let mut g = KgGraph::new();
        g.nodes.push(node("hello"));
        {
            let store = ScipOverlayStore::open(&path).unwrap();
            store.put("myidx", g).unwrap();
        }

        let reopened = ScipOverlayStore::open(&path).unwrap();
        let rec = reopened
            .get("myidx")
            .unwrap()
            .expect("overlay must survive reopen");
        assert_eq!(rec.index_id, "myidx");
        assert_eq!(rec.graph.node_count(), 1);
        assert_eq!(rec.graph.nodes[0].name, "hello");
    }

    #[test]
    fn overlay_path_is_a_sibling_of_the_facts_store() {
        let p = overlay_path_beside_facts(Path::new("/var/lib/analyze/facts.redb"));
        assert_eq!(p, Path::new("/var/lib/analyze/scip_overlays.redb"));
    }

    #[test]
    fn absent_overlay_reads_as_none() {
        let tmp = TempDir::new().unwrap();
        let store = ScipOverlayStore::open(&tmp.path().join("o.redb")).unwrap();
        assert!(store.get("never-ingested").unwrap().is_none());
    }

    /// Why: persistence alone does not answer "did anyone ingest SCIP here?".
    /// An ingested-but-symbol-free SCIP index and an index nobody ingested both
    /// contribute zero nodes to `/graph`; only the `Option` tells them apart.
    /// What: ingests an empty `KgGraph`, then asserts `get` is `Some` with zero
    /// nodes while an untouched index is `None`.
    /// Test: this test.
    #[test]
    fn empty_overlay_is_distinguishable_from_absent() {
        let tmp = TempDir::new().unwrap();
        let store = ScipOverlayStore::open(&tmp.path().join("o.redb")).unwrap();
        store.put("empty-idx", KgGraph::new()).unwrap();

        let rec = store.get("empty-idx").unwrap().expect("ingested");
        assert_eq!(rec.graph.node_count(), 0);
        assert!(store.get("other-idx").unwrap().is_none());
    }

    #[test]
    fn put_overwrites_prior_overlay() {
        let tmp = TempDir::new().unwrap();
        let store = ScipOverlayStore::open(&tmp.path().join("o.redb")).unwrap();

        let mut first = KgGraph::new();
        first.nodes.push(node("old"));
        store.put("idx", first).unwrap();

        let mut second = KgGraph::new();
        second.nodes.push(node("new"));
        store.put("idx", second).unwrap();

        let rec = store.get("idx").unwrap().unwrap();
        assert_eq!(rec.graph.node_count(), 1);
        assert_eq!(rec.graph.nodes[0].name, "new");
    }

    /// Why: an unreadable overlay database must stop the daemon, not be
    /// silently replaced with an empty one — a fresh empty store would drop
    /// overlays that are still on disk, reproducing #5049 by another route.
    /// This is the deliberate difference from `FactStore::open`, which does
    /// recreate because facts are re-derivable.
    /// What: writes garbage to the path and asserts `open` returns `Err`.
    /// Test: this test.
    #[test]
    fn corrupt_db_fails_open_loudly() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("o.redb");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&[0xABu8; 4096]))
            .unwrap();
        assert!(
            ScipOverlayStore::open(&path).is_err(),
            "an unreadable overlay db must not open as an empty store"
        );
    }
}
