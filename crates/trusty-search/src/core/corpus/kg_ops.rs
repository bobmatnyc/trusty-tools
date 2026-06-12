//! [`CorpusStore`] knowledge-graph and community persistence.
//!
//! Why: split from the monolithic `store_impl` to keep each file under 500
//! lines. This file owns KG node/edge persistence and the migration-tolerance
//! community tables — nothing else.
//! What: `impl CorpusStore` block covering `save_kg_graph`, `load_kg_graph`,
//! `kg_node_count`, `save_communities`, `load_communities`, and
//! `symbol_community`.
//! Test: covered by the `tests` submodule (e.g. `save_load_kg_roundtrip`).

use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata};

use super::store_impl::CorpusStore;
use super::tables::{
    KG_COMMUNITIES_TABLE, KG_EDGES_REV_TABLE, KG_EDGES_TABLE, KG_NODES_TABLE,
    KG_SYMBOL_COMMUNITY_TABLE,
};
use super::types::{load_adjacency, PersistedKgNode};

impl CorpusStore {
    /// Replace the persisted KG node set + forward/reverse adjacency lists in
    /// one atomic transaction (issue #41 phase 2).
    ///
    /// Why: persisting the symbol graph alongside the chunk corpus lets
    /// warm-boot skip the full `build_from_chunks` rebuild. Doing the whole
    /// write under one transaction guarantees readers never observe a
    /// half-rewritten graph.
    /// What: clears the three KG tables then re-inserts the supplied nodes and
    /// forward/reverse adjacencies. Each value is `serde_json`-encoded. An
    /// `(adj_fwd, adj_rev)` row whose vector is empty is skipped to keep the
    /// stored graph minimal.
    /// Test: `save_load_kg_roundtrip` round-trips a synthetic graph through
    /// `save_kg_graph` + `load_kg_graph` and asserts equality.
    pub fn save_kg_graph(
        &self,
        nodes: &[(String, PersistedKgNode)],
        adj_fwd: &[(String, Vec<(String, String)>)],
        adj_rev: &[(String, Vec<(String, String)>)],
    ) -> Result<()> {
        let txn = self.db.begin_write().context("begin kg graph upsert txn")?;
        {
            let mut nodes_tbl = txn.open_table(KG_NODES_TABLE)?;
            // Drain stale rows first so a shrinking graph doesn't leave orphans.
            nodes_tbl.retain(|_, _| false).context("clear kg_nodes")?;
            for (symbol, node) in nodes {
                let bytes = serde_json::to_vec(node)
                    .with_context(|| format!("serialize kg node {symbol}"))?;
                nodes_tbl
                    .insert(symbol.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert kg node {symbol}"))?;
            }

            let mut fwd_tbl = txn.open_table(KG_EDGES_TABLE)?;
            fwd_tbl.retain(|_, _| false).context("clear kg_edges")?;
            for (src, targets) in adj_fwd {
                if targets.is_empty() {
                    continue;
                }
                let bytes = serde_json::to_vec(targets)
                    .with_context(|| format!("serialize kg fwd adjacency for {src}"))?;
                fwd_tbl
                    .insert(src.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert kg fwd adjacency for {src}"))?;
            }

            let mut rev_tbl = txn.open_table(KG_EDGES_REV_TABLE)?;
            rev_tbl.retain(|_, _| false).context("clear kg_edges_rev")?;
            for (tgt, sources) in adj_rev {
                if sources.is_empty() {
                    continue;
                }
                let bytes = serde_json::to_vec(sources)
                    .with_context(|| format!("serialize kg rev adjacency for {tgt}"))?;
                rev_tbl
                    .insert(tgt.as_str(), bytes.as_slice())
                    .with_context(|| format!("insert kg rev adjacency for {tgt}"))?;
            }
        }
        txn.commit().context("commit kg graph upsert txn")?;
        Ok(())
    }

    /// Load the persisted symbol graph (issue #41 phase 2).
    ///
    /// Why: warm-boot wants to bring the KG back online without paying the
    /// `build_from_chunks` cost. Returning the raw node + adjacency lists lets
    /// the caller (`SymbolGraph::load_from_corpus`) rebuild the in-memory
    /// `petgraph` without re-touching the chunk corpus.
    /// What: returns `(nodes, adj_fwd, adj_rev)` where each list is the
    /// deserialized contents of the three KG tables. An empty (or fresh)
    /// database yields three empty vectors. Corrupt rows are skipped with a
    /// `warn` rather than failing the whole load.
    /// Test: `save_load_kg_roundtrip`.
    #[allow(clippy::type_complexity)]
    pub fn load_kg_graph(
        &self,
    ) -> Result<(
        Vec<(String, PersistedKgNode)>,
        Vec<(String, Vec<(String, String)>)>,
        Vec<(String, Vec<(String, String)>)>,
    )> {
        let txn = self.db.begin_read().context("begin kg graph read txn")?;

        let mut nodes: Vec<(String, PersistedKgNode)> = Vec::new();
        {
            let nodes_tbl = txn.open_table(KG_NODES_TABLE)?;
            for entry in nodes_tbl.iter().context("iterate kg_nodes table")? {
                let (key, value) = entry.context("read kg_nodes row")?;
                let symbol = key.value().to_string();
                match serde_json::from_slice::<PersistedKgNode>(value.value()) {
                    Ok(node) => nodes.push((symbol, node)),
                    Err(e) => {
                        tracing::warn!("kg: skipping corrupt kg_nodes row '{symbol}' ({e})")
                    }
                }
            }
        }

        let adj_fwd = load_adjacency(&txn, KG_EDGES_TABLE, "kg_edges")?;
        let adj_rev = load_adjacency(&txn, KG_EDGES_REV_TABLE, "kg_edges_rev")?;
        Ok((nodes, adj_fwd, adj_rev))
    }

    /// Number of persisted KG nodes currently stored.
    ///
    /// Why: warm-boot uses this as a cheap "is the persisted graph populated?"
    /// probe before deciding whether to fall back to `build_from_chunks`.
    /// What: returns the row count of `KG_NODES_TABLE`.
    /// Test: covered by `save_load_kg_roundtrip` (asserts count after save).
    pub fn kg_node_count(&self) -> Result<usize> {
        let txn = self.db.begin_read().context("begin kg count txn")?;
        let table = txn.open_table(KG_NODES_TABLE)?;
        Ok(table.len().context("count kg_nodes")? as usize)
    }

    /// Replace the persisted community records + symbol→community map
    /// (migration tolerance, not called by the active search path as of
    /// v0.10.0).
    ///
    /// Why: retained so old tooling that still calls this (e.g. test helpers,
    /// migration utilities) compiles. The Louvain pipeline was removed in
    /// v0.10.0 (issue #152); this method is no longer called by the daemon.
    /// What: clears the two migration-tolerance community tables then re-inserts
    /// the supplied records and per-symbol mappings in one atomic transaction.
    /// Test: `save_load_communities_roundtrip` round-trips a synthetic partition.
    pub fn save_communities(
        &self,
        records: &[(u64, Vec<u8>)],
        symbol_to_community: &[(String, u64)],
    ) -> Result<()> {
        let txn = self
            .db
            .begin_write()
            .context("begin communities upsert txn")?;
        {
            let mut comm_tbl = txn.open_table(KG_COMMUNITIES_TABLE)?;
            comm_tbl
                .retain(|_, _| false)
                .context("clear kg_communities")?;
            for (id, bytes) in records {
                comm_tbl
                    .insert(id, bytes.as_slice())
                    .with_context(|| format!("insert community {id}"))?;
            }
            let mut sym_tbl = txn.open_table(KG_SYMBOL_COMMUNITY_TABLE)?;
            sym_tbl
                .retain(|_, _| false)
                .context("clear kg_symbol_community")?;
            for (sym, id) in symbol_to_community {
                sym_tbl
                    .insert(sym.as_str(), id)
                    .with_context(|| format!("insert symbol→community for {sym}"))?;
            }
        }
        txn.commit().context("commit communities upsert txn")?;
        Ok(())
    }

    /// Load persisted community records (migration tolerance, not called by
    /// the active search path as of v0.10.0).
    ///
    /// Why: retained for parity with `save_communities` so old code that calls
    /// both still compiles. The `/communities` HTTP endpoint was removed in
    /// v0.10.0 (issue #152).
    /// What: returns `Vec<(community_id, serialized_record_bytes)>` from the
    /// migration-tolerance `kg_communities` redb table.
    /// Test: `save_load_communities_roundtrip`.
    pub fn load_communities(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let txn = self.db.begin_read().context("begin communities read txn")?;
        let table = txn.open_table(KG_COMMUNITIES_TABLE)?;
        let mut out: Vec<(u64, Vec<u8>)> = Vec::new();
        for entry in table.iter().context("iterate kg_communities table")? {
            let (key, value) = entry.context("read kg_communities row")?;
            out.push((key.value(), value.value().to_vec()));
        }
        Ok(out)
    }

    /// Look up the community id for a single symbol (migration tolerance, not
    /// called by the active search path as of v0.10.0).
    ///
    /// Why: retained for parity with `save_communities` / `load_communities`
    /// so any surviving callers compile. Community id lookups were removed from
    /// the search materialisation path in v0.10.0 (issue #152).
    /// What: returns `Ok(Some(id))` when the symbol has an entry in the legacy
    /// `kg_symbol_community` table; `Ok(None)` otherwise.
    /// Test: `save_load_communities_roundtrip` asserts point reads.
    pub fn symbol_community(&self, symbol: &str) -> Result<Option<u64>> {
        let txn = self
            .db
            .begin_read()
            .context("begin symbol_community read txn")?;
        let table = txn.open_table(KG_SYMBOL_COMMUNITY_TABLE)?;
        Ok(table
            .get(symbol)
            .context("get symbol_community row")?
            .map(|v| v.value()))
    }
}
