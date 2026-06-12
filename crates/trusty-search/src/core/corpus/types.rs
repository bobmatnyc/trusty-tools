//! Persisted KG node type and the adjacency-table iterator helper.
//!
//! Why: separating these small, data-only items from the large `CorpusStore`
//! impl keeps `store_impl.rs` focused on transaction logic.
//! What: exports [`PersistedKgNode`] (the on-disk KG node payload) and
//! [`load_adjacency`] (shared read helper for the two adjacency tables).
//! Test: both are covered transitively by `save_load_kg_roundtrip` in `tests`.

use anyhow::{Context, Result};
use redb::{ReadableTable, TableDefinition};

/// Compact on-disk representation of a [`crate::core::symbol_graph::SymbolNode`]
/// (issue #41 phase 2).
///
/// Why: the runtime `SymbolNode` carries the symbol name three times (as the
/// `petgraph` node weight, the `by_symbol` map key, and inside the node
/// itself). Storing only `chunk_id + file` (with the symbol implied by the
/// row key) keeps the on-disk size lean and avoids a String redundancy.
/// What: serde-derived JSON payload stored under `KG_NODES_TABLE[symbol]`.
/// Test: covered by `save_load_kg_roundtrip` in this module and by the
/// `SymbolGraph` round-trip test in `core::symbol_graph::tests`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PersistedKgNode {
    pub chunk_id: String,
    pub file: String,
}

/// Iterate one of the KG adjacency tables and deserialize each row.
///
/// Why: `KG_EDGES_TABLE` and `KG_EDGES_REV_TABLE` have identical shapes
/// (`symbol → Vec<(edge_kind, peer_symbol)>`); centralising the read avoids
/// duplicating the corrupt-row tolerance and `serde_json` decode boilerplate.
/// What: walks the table on the supplied read transaction and returns a
/// `Vec<(key, adjacency)>`. Corrupt rows are logged at `warn` and skipped.
/// Test: covered transitively by `save_load_kg_roundtrip`.
#[allow(clippy::type_complexity)]
pub(super) fn load_adjacency(
    txn: &redb::ReadTransaction,
    table_def: TableDefinition<'_, &str, &[u8]>,
    label: &str,
) -> Result<Vec<(String, Vec<(String, String)>)>> {
    let table = txn.open_table(table_def)?;
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for entry in table
        .iter()
        .with_context(|| format!("iterate {label} table"))?
    {
        let (key, value) = entry.with_context(|| format!("read {label} row"))?;
        let sym = key.value().to_string();
        match serde_json::from_slice::<Vec<(String, String)>>(value.value()) {
            Ok(adj) => out.push((sym, adj)),
            Err(e) => tracing::warn!("kg: skipping corrupt {label} row '{sym}' ({e})"),
        }
    }
    Ok(out)
}
