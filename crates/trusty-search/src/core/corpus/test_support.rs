//! Corpus fault injectors for the fail-closed regression tests (#5357, #5505).
//!
//! Why: the #2178 root-move trust gate reads the corpus's last-indexed root,
//! and #5357 is about what that gate does when the READ fails. redb gives a
//! test no way to fail a read from outside — the database file lock means a
//! second handle cannot reach the open `Database` behind the store's back — so
//! the fault has to be planted through the store's own handle. Asserting the
//! gate on a hand-rolled `Err` value instead would prove the decision function
//! and leave the wiring from `IndexHandle::read_indexed_root` untested, which
//! is exactly where the `unwrap_or(None)` lived.
//!
//! The same reasoning covers the #5505 contributed-overlay arms: the merge's
//! only reachable failure is a stored row that will not deserialize, and only
//! the store's own handle can plant one.
//!
//! What: free functions taking `&CorpusStore`, reaching its `pub(super)` `db`
//! field. They live here rather than as methods so the production type carries
//! no test-only surface. The whole module is `#[cfg(test)]`, so none of it
//! exists in a shipped binary.
//!
//! Test: `service::reindex::root_hijack_tests`, `service::server::tests_5357`,
//! `core::symbol_graph::contrib_tests`, and
//! `service::server::tests_contrib_graph` are the only callers.

use anyhow::{Context, Result};

use super::contrib::KG_CONTRIB_TABLE;
use super::tables::KG_NODES_TABLE;
use super::CorpusStore;
use crate::core::migration::{META_KEY_INDEXED_ROOT, META_TABLE};

/// Make every `_meta` read fail with a real redb error.
///
/// What: drops `_meta` and recreates a table of the same NAME with an
/// incompatible value type, so every later `open_table(META_TABLE)` returns
/// `TableError::TableTypeMismatch` — an on-disk schema fault, the shape a
/// damaged or half-migrated corpus produces.
/// Test: `reindex_refuses_when_the_corpus_indexed_root_read_fails`.
pub(crate) fn break_meta_table(store: &CorpusStore) -> Result<()> {
    const DECOY_META_TABLE: redb::TableDefinition<&str, u64> = redb::TableDefinition::new("_meta");
    let txn = store.db.begin_write().context("begin _meta break txn")?;
    txn.delete_table(META_TABLE)
        .map_err(|e| anyhow::anyhow!("drop _meta table: {e}"))?;
    {
        let mut decoy = txn
            .open_table(DECOY_META_TABLE)
            .map_err(|e| anyhow::anyhow!("create decoy _meta table: {e}"))?;
        decoy
            .insert(META_KEY_INDEXED_ROOT, 0u64)
            .context("seed decoy _meta row")?;
    }
    txn.commit().context("commit _meta break txn")?;
    Ok(())
}

/// Corrupt the stored `indexed_root` VALUE while leaving the table's schema
/// intact.
///
/// Why: distinct from [`break_meta_table`], and the narrower of the two. The
/// table opens, the key is present, and only the bytes are damaged — the arm
/// `read_indexed_root_sync` used to answer `Ok(None)` for, which reads back as
/// "this index has no prior root" and skips the gate outright.
/// What: overwrites `_meta[indexed_root]` with a byte sequence that is not
/// valid UTF-8.
/// Test: `reindex_refuses_when_the_indexed_root_value_is_corrupt`.
pub(crate) fn corrupt_indexed_root_value(store: &CorpusStore) -> Result<()> {
    let txn = store
        .db
        .begin_write()
        .context("begin indexed_root corrupt txn")?;
    {
        let mut table = txn
            .open_table(META_TABLE)
            .map_err(|e| anyhow::anyhow!("open _meta table: {e}"))?;
        // Lone continuation bytes — invalid as UTF-8 under any decoder.
        table
            .insert(META_KEY_INDEXED_ROOT, [0xffu8, 0xfe, 0x80].as_slice())
            .context("insert corrupt indexed_root bytes")?;
    }
    txn.commit().context("commit indexed_root corrupt txn")?;
    Ok(())
}

/// Make every derived-KG write fail with a real redb error (#5505).
///
/// What: the [`break_meta_table`] trick applied to `kg_nodes` — the table is
/// recreated with an incompatible value type, so `save_kg_graph`'s
/// `open_table` returns `TableError::TableTypeMismatch` and
/// `SymbolGraph::save_to_corpus` fails while every OTHER table still works.
/// Test: `contrib_persist_failure_still_merges`.
pub(crate) fn break_kg_nodes_table(store: &CorpusStore) -> Result<()> {
    const DECOY_KG_NODES_TABLE: redb::TableDefinition<&str, u64> =
        redb::TableDefinition::new("kg_nodes");
    let txn = store.db.begin_write().context("begin kg_nodes break txn")?;
    txn.delete_table(KG_NODES_TABLE)
        .map_err(|e| anyhow::anyhow!("drop kg_nodes table: {e}"))?;
    {
        let mut decoy = txn
            .open_table(DECOY_KG_NODES_TABLE)
            .map_err(|e| anyhow::anyhow!("create decoy kg_nodes table: {e}"))?;
        decoy.insert("decoy", 0u64).context("seed decoy kg row")?;
    }
    txn.commit().context("commit kg_nodes break txn")?;
    Ok(())
}

/// Plant a `kg_contrib` row whose value is not a serializable `ContribGraph`,
/// so `load_contrib_graphs` fails for the whole index (#5505).
///
/// Why: the contributed-overlay merge only has one realistic failure mode a
/// test can reach — a stored row that will not deserialize. Writing it through
/// the store's own `db` handle is the only way in: redb's file lock means a
/// second `Database::open` on the same path cannot reach the live corpus.
/// What: inserts non-JSON bytes under `producer`. The row is left in place, so
/// a later `save_contrib_graph` for a DIFFERENT producer still succeeds while
/// every load keeps failing — which is exactly the #5505 shape (the ingest is
/// durable, the merge is not).
/// Test: `ingest_reports_503_when_the_contributed_overlay_cannot_be_merged`.
pub(crate) fn corrupt_contrib_row(store: &CorpusStore, producer: &str) -> Result<()> {
    let txn = store
        .db
        .begin_write()
        .context("begin contrib corrupt txn")?;
    {
        let mut table = txn
            .open_table(KG_CONTRIB_TABLE)
            .map_err(|e| anyhow::anyhow!("open kg_contrib table: {e}"))?;
        table
            .insert(producer, b"{ this is not a contrib graph".as_slice())
            .context("insert corrupt contrib row")?;
    }
    txn.commit().context("commit contrib corrupt txn")?;
    Ok(())
}
