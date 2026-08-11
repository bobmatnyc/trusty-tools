//! `_meta` fault injectors for the #5357 fail-closed regression tests.
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
//! What: two free functions taking `&CorpusStore`, reaching its `pub(super)`
//! `db` field. They live here rather than as methods so the production type
//! carries no test-only surface. The whole module is `#[cfg(test)]`, so none of
//! it exists in a shipped binary.
//!
//! Test: `service::reindex::root_hijack_tests` and
//! `service::server::tests_5357` are the only callers.

use anyhow::{Context, Result};

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
