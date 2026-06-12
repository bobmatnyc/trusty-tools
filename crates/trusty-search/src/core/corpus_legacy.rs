//! Legacy community-persistence methods (migration tolerance, issue #152).
//!
//! Why: the Louvain community pipeline was removed in v0.10.0 (issue #152),
//! but the `kg_communities` / `kg_symbol_community` tables and their
//! accessors are retained so old tooling, migration utilities, and the
//! redb-migrate copier keep compiling and old databases open without schema
//! errors. Parked in a child module of `corpus` (private-field access via
//! the parent-module privacy rule) to keep `corpus.rs` under its line-cap
//! budget — these methods are frozen, not evolving.
//!
//! What: `save_communities`, `load_communities`, `symbol_community` —
//! verbatim moves from `corpus.rs`, one `impl CorpusStore` block.
//!
//! Test: `save_load_communities_roundtrip` in `corpus::tests`.

use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTable};

use super::{CorpusStore, KG_COMMUNITIES_TABLE, KG_SYMBOL_COMMUNITY_TABLE};

impl CorpusStore {
    /// Replace the persisted community records + symbol→community map (migration
    /// tolerance, not called by the active search path as of v0.10.0).
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
