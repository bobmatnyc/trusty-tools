//! Make a staging corpus match the live one for a set of keys (#12064).
//!
//! Why: an incremental reindex copies one snapshot of the live corpus into
//! staging. A write that commits to the live corpus after that snapshot and
//! before the swap is missing from staging, and a delete in that span is
//! still there; the promotion would drop the one and resurrect the other.
//! What: [`CorpusStore::resync_selection_from`] and
//! [`CorpusStore::resync_chunk_ids_from`] delete this store's rows for the
//! keys and copy the source's current rows for them, in ONE write
//! transaction each, so the keys end with exactly the source's rows.
//! Test: `resync_replaces_changed_rows_and_drops_deleted_ones` below;
//! the window by `a_write_and_a_delete_in_the_carryover_window_survive_the_promotion`.

use anyhow::{Context, Result};
use redb::{ReadTransaction, ReadableDatabase, TableDefinition, WriteTransaction};

use super::store_impl::CorpusStore;
use super::tables::{CHUNKS_TABLE, ENTITIES_TABLE, FILE_HASHES_TABLE};
use crate::core::path_selection::PathSelection;

type Table = TableDefinition<'static, &'static str, &'static [u8]>;

impl CorpusStore {
    /// Replace this store's chunk, entity and hash rows under `sel` with
    /// `source`'s. Returns the number of rows copied from `source`.
    pub(crate) fn resync_selection_from(
        &self,
        source: &CorpusStore,
        sel: &PathSelection,
    ) -> Result<usize> {
        let old = self.rows_under(sel)?;
        let new = source.rows_under(sel)?;
        let ids = |rows: Vec<(String, String)>| rows.into_iter().map(|(id, _)| id).collect();
        self.replace_rows(
            source,
            [
                (CHUNKS_TABLE, ids(old.chunks), ids(new.chunks)),
                (ENTITIES_TABLE, old.entity_files, new.entity_files),
                (
                    FILE_HASHES_TABLE,
                    self.hash_keys_under(sel)?,
                    source.hash_keys_under(sel)?,
                ),
            ],
        )
    }

    /// Replace this store's rows for chunk `ids` with `source`'s; an id the
    /// source no longer holds is deleted.
    pub(crate) fn resync_chunk_ids_from(
        &self,
        source: &CorpusStore,
        ids: &[String],
    ) -> Result<usize> {
        self.replace_rows(source, [(CHUNKS_TABLE, ids.to_vec(), ids.to_vec())])
    }

    /// Per table: remove the first key list, then copy the second from
    /// `source` where it holds the key. One write transaction for all.
    fn replace_rows<const N: usize>(
        &self,
        source: &CorpusStore,
        tables: [(Table, Vec<String>, Vec<String>); N],
    ) -> Result<usize> {
        let src = source
            .db
            .begin_read()
            .context("begin resync source read txn")?;
        let txn = self.db.begin_write().context("begin resync write txn")?;
        let mut copied = 0;
        for (table, remove, copy) in &tables {
            copied += replace_keys(&txn, &src, *table, remove, copy)?;
        }
        txn.commit().context("commit resync txn")?;
        Ok(copied)
    }

    /// The content-hash keys `sel` names, read by key range.
    fn hash_keys_under(&self, sel: &PathSelection) -> Result<Vec<String>> {
        let txn = self.db.begin_read().context("begin hash range read txn")?;
        let table = match txn.open_table(FILE_HASHES_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(anyhow::anyhow!("open file_hashes: {e}")),
        };
        let mut keys = Vec::new();
        for start in sel.scan_starts() {
            for entry in table.range::<&str>(start..).context("range hashes")? {
                let (key, _) = entry.context("read hash row")?;
                if !key.value().starts_with(start) {
                    break;
                }
                if sel.selects_file(key.value()) {
                    keys.push(key.value().to_string());
                }
            }
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }
}

/// Remove `remove` from `table` in `txn`, then copy `copy` from `src`.
fn replace_keys(
    txn: &WriteTransaction,
    src: &ReadTransaction,
    table: Table,
    remove: &[String],
    copy: &[String],
) -> Result<usize> {
    let mut dst = txn.open_table(table)?;
    for key in remove {
        dst.remove(key.as_str())
            .with_context(|| format!("resync: remove row '{key}'"))?;
    }
    let source = match src.open_table(table) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(anyhow::anyhow!("resync: open source table: {e}")),
    };
    let mut copied = 0;
    for key in copy {
        if let Some(value) = source
            .get(key.as_str())
            .with_context(|| format!("resync: read row '{key}'"))?
        {
            dst.insert(key.as_str(), value.value())
                .with_context(|| format!("resync: copy row '{key}'"))?;
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunker::{ChunkType, RawChunk};
    use std::collections::BTreeSet;

    fn chunk(file: &str, id: &str, content: &str) -> RawChunk {
        RawChunk {
            id: id.into(),
            file: file.into(),
            start_line: 1,
            end_line: 1,
            content: content.into(),
            function_name: None,
            language: None,
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        }
    }

    fn ids(store: &CorpusStore) -> BTreeSet<String> {
        store.list_chunk_ids().expect("ids").into_iter().collect()
    }

    /// Why: a rewritten file's old ids, a deleted file's rows and their hashes
    /// must all leave staging, and a sibling must stay untouched.
    /// What: staging holds the snapshot; live then rewrites `a.rs`, deletes
    /// `b.rs` and removes chunk `c.rs:1:1`; after the resync staging matches
    /// live for those keys and keeps `d.rs`.
    /// Test: this function IS the test.
    #[test]
    fn resync_replaces_changed_rows_and_drops_deleted_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let live = CorpusStore::open(&dir.path().join("live.redb")).expect("live");
        let staging = CorpusStore::open(&dir.path().join("staging.redb")).expect("staging");
        for store in [&live, &staging] {
            store
                .upsert_chunks(&[
                    chunk("a.rs", "a.rs:1:1", "old"),
                    chunk("b.rs", "b.rs:1:1", "b"),
                    chunk("c.rs", "c.rs:1:1", "c"),
                    chunk("d.rs", "d.rs:1:1", "d"),
                ])
                .expect("seed");
            store
                .upsert_file_hashes(&[("a.rs", "h-old"), ("b.rs", "h-b")])
                .expect("hashes");
        }
        live.delete_chunks(&["a.rs:1:1".into(), "b.rs:1:1".into(), "c.rs:1:1".into()])
            .expect("delete");
        live.upsert_chunks(&[chunk("a.rs", "a.rs:2:3", "new")])
            .expect("rewrite");
        live.delete_file_hash_entries(&["b.rs".into()])
            .expect("hash");
        live.upsert_file_hashes(&[("a.rs", "h-new")]).expect("hash");

        let files = PathSelection::Files(BTreeSet::from(["a.rs".into(), "b.rs".into()]));
        staging
            .resync_selection_from(&live, &files)
            .expect("resync files");
        staging
            .resync_chunk_ids_from(&live, &["c.rs:1:1".into()])
            .expect("resync ids");

        assert_eq!(ids(&staging), ids(&live));
        assert!(ids(&staging).contains("d.rs:1:1"), "a sibling stays");
        assert_eq!(
            staging.file_hash("a.rs").expect("hash").as_deref(),
            Some("h-new")
        );
        assert_eq!(staging.file_hash("b.rs").expect("hash"), None);
    }
}
