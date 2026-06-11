//! Contributed-graph overlay storage (ADR-0009, issue #819).
//!
//! Why: external extractors (T-SQL/C# cross-tier scanners, future
//! endpoint/queue producers) contribute relationship graphs that the
//! chunk-derived pipeline cannot see. Those contributions must be durable
//! (survive daemon restarts) and must survive reindex (which rewrites only
//! the derived `kg_*` tables). Storing them in a separate redb table keyed
//! by producer gives replace-per-producer semantics for free: re-ingesting
//! replaces that producer's whole prior contribution, so deletions in the
//! scanned codebase never leave stale edges behind.
//!
//! What: a child module of `corpus` (so it can use the private `db` handle)
//! defining the `kg_contrib` table — `producer → serialized ContribGraph` —
//! plus the wire-shape types and the `CorpusStore` save/load/delete methods.
//! One row per producer; saving is a single atomic replace.
//!
//! Test: `tests` below — save/load round-trip, replace-per-producer,
//! missing-table tolerance (pre-upgrade DBs), delete.

use anyhow::Context;
use redb::{ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use super::CorpusStore;

/// `kg_contrib` table: producer id → serialized [`ContribGraph`] (JSON).
///
/// Why a single blob per producer rather than per-node/per-edge rows:
/// the ingest contract is replace-per-producer (ADR-0009 + #819 discussion),
/// so the natural unit of storage is the producer's entire contribution.
/// Replace = one insert; delete = one remove; load = iterate producers.
const KG_CONTRIB_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("kg_contrib");

/// One contributed node: extractor-minted canonical identity.
///
/// `id` examples: `hotstats_live.dbo.tbl_requests` (table),
/// `dbo.usp_GetProperty` (proc), `OrderService.Save` (host-language method).
/// `kind` examples: `table`, `view`, `proc`, `function`, `csharp_method`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContribNode {
    pub id: String,
    pub kind: String,
}

/// One contributed edge. `kind` is the coarse vocabulary key (`reads`,
/// `writes`, `references`, `calls_function`, `accesses_resource`) mapped to a
/// first-class `EdgeKind` at merge time; `tag` is the `custom:<relation>`
/// escape-hatch key used as the fallback when `kind` is absent/unknown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContribEdge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    /// Source files that asserted this relation (extractor provenance).
    #[serde(default)]
    pub provenance: Vec<String>,
    /// Linked-server metadata for cross-server T-SQL references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_server: Option<String>,
}

/// A producer's complete contribution for one index.
///
/// Envelope metadata (`producer_version`, `git_sha`) enables cheap staleness
/// checks ("overlay built from SHA X, repo is at Y") without timestamps —
/// the reference emitter is byte-deterministic and a timestamp would break
/// that property (#819 discussion).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContribGraph {
    pub producer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub nodes: Vec<ContribNode>,
    #[serde(default)]
    pub edges: Vec<ContribEdge>,
}

impl CorpusStore {
    /// Replace `graph.producer`'s contribution with `graph` (ADR-0009).
    ///
    /// Why: replace-per-producer is the deletion-correct ingest semantics —
    /// extractors emit their complete graph per run, so the previous run's
    /// rows must not linger (a proc deleted from the codebase would otherwise
    /// keep its edges forever).
    /// What: serializes the whole [`ContribGraph`] and inserts it under the
    /// producer key in one write txn (atomic replace).
    /// Test: `contrib_replace_per_producer_drops_old_rows`.
    pub fn save_contrib_graph(&self, graph: &ContribGraph) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(graph).context("serialize contrib graph")?;
        let txn = self.db.begin_write().context("begin contrib write txn")?;
        {
            let mut table = txn
                .open_table(KG_CONTRIB_TABLE)
                .context("open kg_contrib table")?;
            table
                .insert(graph.producer.as_str(), bytes.as_slice())
                .context("insert contrib graph")?;
        }
        txn.commit().context("commit contrib write txn")?;
        Ok(())
    }

    /// Load every producer's contribution, sorted by producer id.
    ///
    /// Why: graph (re)builds merge all contributions into the in-RAM
    /// petgraph; ordering by producer keeps merge results deterministic.
    /// What: reads every row of `kg_contrib`. A database created before this
    /// table existed returns an empty vec (table-missing is not an error).
    /// Test: `contrib_round_trip`, `contrib_missing_table_is_empty`.
    pub fn load_contrib_graphs(&self) -> anyhow::Result<Vec<ContribGraph>> {
        let txn = self.db.begin_read().context("begin contrib read txn")?;
        let table = match txn.open_table(KG_CONTRIB_TABLE) {
            Ok(t) => t,
            // Pre-upgrade DB: the table has never been written. Not an error.
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e).context("open kg_contrib table"),
        };
        let mut out: Vec<ContribGraph> = Vec::new();
        for entry in table.iter().context("iterate kg_contrib")? {
            let (_, value) = entry.context("read kg_contrib row")?;
            let graph: ContribGraph =
                serde_json::from_slice(value.value()).context("deserialize contrib graph")?;
            out.push(graph);
        }
        out.sort_by(|a, b| a.producer.cmp(&b.producer));
        Ok(out)
    }

    /// Remove one producer's contribution entirely.
    ///
    /// Why: index housekeeping and explicit producer retraction.
    /// What: deletes the producer's row; returns whether a row existed.
    /// Test: `contrib_delete_removes_producer`.
    pub fn delete_contrib_graph(&self, producer: &str) -> anyhow::Result<bool> {
        let txn = self.db.begin_write().context("begin contrib delete txn")?;
        let existed;
        {
            let mut table = txn
                .open_table(KG_CONTRIB_TABLE)
                .context("open kg_contrib table")?;
            existed = table
                .remove(producer)
                .context("remove contrib graph")?
                .is_some();
        }
        txn.commit().context("commit contrib delete txn")?;
        Ok(existed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, CorpusStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CorpusStore::open(&dir.path().join("corpus.redb")).expect("open corpus");
        (dir, store)
    }

    fn sample(producer: &str, edge_to: &str) -> ContribGraph {
        ContribGraph {
            producer: producer.to_string(),
            producer_version: Some("0.1.0".into()),
            git_sha: Some("abc123".into()),
            nodes: vec![
                ContribNode {
                    id: "dbo.usp_x".into(),
                    kind: "proc".into(),
                },
                ContribNode {
                    id: edge_to.into(),
                    kind: "table".into(),
                },
            ],
            edges: vec![ContribEdge {
                from: "dbo.usp_x".into(),
                to: edge_to.into(),
                kind: Some("writes".into()),
                tag: Some("custom:writes_table".into()),
                provenance: vec!["a.sql".into()],
                linked_server: None,
            }],
        }
    }

    #[test]
    fn contrib_round_trip() {
        let (_dir, store) = store();
        let g = sample("navigatsql", "dbo.orders");
        store.save_contrib_graph(&g).expect("save");
        let loaded = store.load_contrib_graphs().expect("load");
        assert_eq!(loaded, vec![g]);
    }

    #[test]
    fn contrib_missing_table_is_empty() {
        let (_dir, store) = store();
        // No contrib ever saved: the table does not exist yet.
        assert!(store.load_contrib_graphs().expect("load").is_empty());
    }

    #[test]
    fn contrib_replace_per_producer_drops_old_rows() {
        let (_dir, store) = store();
        store
            .save_contrib_graph(&sample("navigatsql", "dbo.orders"))
            .expect("save v1");
        // Second ingest from the same producer: completely replaces the first.
        let v2 = sample("navigatsql", "dbo.customers");
        store.save_contrib_graph(&v2).expect("save v2");
        let loaded = store.load_contrib_graphs().expect("load");
        assert_eq!(loaded, vec![v2]);
        assert!(!loaded[0].edges.iter().any(|e| e.to == "dbo.orders"));
    }

    #[test]
    fn contrib_multi_producer_sorted_and_isolated() {
        let (_dir, store) = store();
        store
            .save_contrib_graph(&sample("zeta", "dbo.t2"))
            .expect("save zeta");
        store
            .save_contrib_graph(&sample("alpha", "dbo.t1"))
            .expect("save alpha");
        let loaded = store.load_contrib_graphs().expect("load");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].producer, "alpha");
        assert_eq!(loaded[1].producer, "zeta");
    }

    #[test]
    fn contrib_delete_removes_producer() {
        let (_dir, store) = store();
        store
            .save_contrib_graph(&sample("navigatsql", "dbo.orders"))
            .expect("save");
        assert!(store.delete_contrib_graph("navigatsql").expect("delete"));
        assert!(!store.delete_contrib_graph("navigatsql").expect("re-delete"));
        assert!(store.load_contrib_graphs().expect("load").is_empty());
    }

    #[test]
    fn contrib_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("corpus.redb");
        {
            let store = CorpusStore::open(&path).expect("open");
            store
                .save_contrib_graph(&sample("navigatsql", "dbo.orders"))
                .expect("save");
        }
        // Simulated daemon restart.
        let store = CorpusStore::open(&path).expect("reopen");
        let loaded = store.load_contrib_graphs().expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].producer, "navigatsql");
    }
}
