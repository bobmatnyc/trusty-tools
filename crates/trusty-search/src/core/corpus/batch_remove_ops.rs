//! [`CorpusStore`] range reads and one-transaction deletes for a batch removal
//! by path prefix or file list (#10110).
//!
//! Why: the single-file removal found a file's chunks by scanning the whole
//! in-memory map, which first rehydrated an evicted corpus. Chunk, entity and
//! hash rows are all keyed by a string that begins with the file path, so a
//! redb range from the selection's key answers the same question in
//! O(matched rows) with no rehydrate.
//! What: [`CorpusStore::rows_under`] lists the selected chunk ids (with their
//! files) and entity rows; [`CorpusStore::delete_chunk_and_entity_rows`]
//! deletes one slice of them in ONE write transaction;
//! [`CorpusStore::delete_file_hashes_under`] deletes the selected hash rows.
//! Test: `rows_under_reads_a_range_and_spares_a_sibling` below; the callers by
//! `indexer::tests::batch_remove_10110`.

use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTable};

use super::store_impl::CorpusStore;
use super::tables::{CHUNKS_TABLE, ENTITIES_TABLE, FILE_HASHES_TABLE};
use crate::core::path_selection::PathSelection;

/// The durable rows one selection names (#10110).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SelectedRows {
    /// `(chunk id, file)`, in key order, so one file's chunks are adjacent.
    pub(crate) chunks: Vec<(String, String)>,
    /// Files holding an entity row.
    pub(crate) entity_files: Vec<String>,
}

/// The one field of a chunk row a selection needs.
#[derive(serde::Deserialize)]
struct FileField<'a> {
    #[serde(borrow)]
    file: std::borrow::Cow<'a, str>,
}

impl CorpusStore {
    /// The chunk and entity rows `sel` names, read by key range (#10110).
    ///
    /// What: one read transaction. Each scan start opens a range and stops at
    /// the first key that no longer begins with it; a chunk row is kept when
    /// its decoded `file` is selected, an entity row when its key is. A chunk
    /// row whose `file` cannot be decoded is an error: a removal must not
    /// skip a row it cannot read and then report success.
    pub(crate) fn rows_under(&self, sel: &PathSelection) -> Result<SelectedRows> {
        let txn = self
            .db
            .begin_read()
            .context("begin batch-remove read txn")?;
        let chunks = txn.open_table(CHUNKS_TABLE)?;
        let entities = txn.open_table(ENTITIES_TABLE)?;
        let mut rows = SelectedRows::default();
        for start in sel.scan_starts() {
            for entry in chunks.range::<&str>(start..).context("range chunks")? {
                let (key, value) = entry.context("read chunk row")?;
                if !key.value().starts_with(start) {
                    break;
                }
                let row: FileField<'_> = serde_json::from_slice(value.value())
                    .with_context(|| format!("decode chunk row '{}'", key.value()))?;
                if sel.selects_file(&row.file) {
                    rows.chunks
                        .push((key.value().to_string(), row.file.into_owned()));
                }
            }
            for entry in entities.range::<&str>(start..).context("range entities")? {
                let (key, _) = entry.context("read entity row")?;
                if !key.value().starts_with(start) {
                    break;
                }
                if sel.selects_file(key.value()) {
                    rows.entity_files.push(key.value().to_string());
                }
            }
        }
        // Overlapping scan starts (`a` and `a/b`) can read a row twice.
        rows.chunks.sort();
        rows.chunks.dedup();
        rows.entity_files.sort();
        rows.entity_files.dedup();
        Ok(rows)
    }

    /// Delete chunk `ids` and the entity rows of `entity_files` in ONE write
    /// transaction (#10110 F3: the single-file path deletes entities in a
    /// second, warn-only transaction, so a restart could resurrect them).
    /// Unknown keys are a no-op.
    pub(crate) fn delete_chunk_and_entity_rows(
        &self,
        ids: &[String],
        entity_files: &[String],
    ) -> Result<()> {
        if ids.is_empty() && entity_files.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write().context("begin batch-remove txn")?;
        {
            let mut chunks = txn.open_table(CHUNKS_TABLE)?;
            for id in ids {
                chunks
                    .remove(id.as_str())
                    .with_context(|| format!("delete chunk {id}"))?;
            }
            let mut entities = txn.open_table(ENTITIES_TABLE)?;
            for file in entity_files {
                entities
                    .remove(file.as_str())
                    .with_context(|| format!("delete entities for {file}"))?;
            }
        }
        txn.commit().context("commit batch-remove txn")?;
        Ok(())
    }

    /// Delete every content-hash row `sel` names, in one write transaction.
    ///
    /// Why (#10110 fix-bar 2): a retry after a failed hash delete matches no
    /// chunk, so a delete keyed by the removed files would write nothing and
    /// leave the stale hashes; the selection is the key, not the chunks.
    /// What: ranges `FILE_HASHES_TABLE` from each scan start; aborts without
    /// a commit when nothing matched. Returns the rows deleted.
    pub(crate) fn delete_file_hashes_under(&self, sel: &PathSelection) -> Result<usize> {
        let txn = self.db.begin_write().context("begin hash delete txn")?;
        let mut doomed: Vec<String> = Vec::new();
        {
            let mut tbl = txn.open_table(FILE_HASHES_TABLE)?;
            for start in sel.scan_starts() {
                for entry in tbl.range::<&str>(start..).context("range hashes")? {
                    let (key, _) = entry.context("read hash row")?;
                    if !key.value().starts_with(start) {
                        break;
                    }
                    if sel.selects_file(key.value()) {
                        doomed.push(key.value().to_string());
                    }
                }
            }
            for key in &doomed {
                tbl.remove(key.as_str())
                    .with_context(|| format!("delete file hash entry for {key}"))?;
            }
        }
        if doomed.is_empty() {
            txn.abort().context("abort empty hash delete txn")?;
            return Ok(0);
        }
        txn.commit().context("commit hash delete txn")?;
        Ok(doomed.len())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// A prefix range reads its own rows, skips the sibling `src/foobar/`, and
    /// the slice delete takes the chunk and entity rows in one go.
    #[test]
    fn rows_under_reads_a_range_and_spares_a_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CorpusStore::open(&dir.path().join("i.redb")).expect("open");
        let chunk = |id: &str, file: &str| crate::core::chunker::RawChunk {
            id: id.to_string(),
            file: file.to_string(),
            start_line: 1,
            end_line: 2,
            content: "fn x() {}".to_string(),
            function_name: None,
            language: Some("rust".to_string()),
            chunk_type: crate::core::chunker::ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        };
        store
            .upsert_chunks(&[
                chunk("src/foo/a.rs:1:2", "src/foo/a.rs"),
                chunk("src/foobar/b.rs:1:2", "src/foobar/b.rs"),
            ])
            .expect("upsert");
        store
            .upsert_file_hashes(&[("src/foo/a.rs", "aa"), ("src/foobar/b.rs", "bb")])
            .expect("hashes");
        let sel = PathSelection::Prefix(BTreeSet::from(["src/foo".to_string()]));
        let rows = store.rows_under(&sel).expect("rows");
        assert_eq!(
            rows.chunks,
            vec![("src/foo/a.rs:1:2".to_string(), "src/foo/a.rs".to_string())]
        );
        let ids: Vec<String> = rows.chunks.iter().map(|(id, _)| id.clone()).collect();
        store
            .delete_chunk_and_entity_rows(&ids, &rows.entity_files)
            .expect("delete");
        assert!(store.rows_under(&sel).expect("rows").chunks.is_empty());
        assert_eq!(store.delete_file_hashes_under(&sel).expect("hashes"), 1);
        assert_eq!(store.delete_file_hashes_under(&sel).expect("again"), 0);
        let left = store.load_file_hashes().expect("load");
        assert_eq!(left.len(), 1, "the sibling's hash stays: {left:?}");
    }
}
