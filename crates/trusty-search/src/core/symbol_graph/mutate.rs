//! Incremental mutation methods for `SymbolGraph`.
//!
//! Why: separating `update_file` and `remove_file` from the build and query
//! methods keeps each file focused and under the 500-line cap.
//! What: the two `impl SymbolGraph` methods that accept a corpus snapshot
//! and rebuild the graph in response to a per-file change or deletion.
//! Test: covered by `test_update_file_drops_old_edges_and_wires_new` and
//! `test_remove_file_drops_file_symbols` in `tests.rs`.

use crate::core::entity::RawEntity;

use super::types::{ChunkTuple, SymbolGraph};

impl SymbolGraph {
    /// Replace one file's portion of the graph with a freshly-rebuilt subset
    /// from `new_chunks` (issue #41 phase 2).
    ///
    /// Why: a per-file index update (`POST /indexes/:id/index-file`) shouldn't
    /// trigger a full `build_from_chunks` over the entire corpus. By taking
    /// the existing corpus snapshot, replacing this file's chunks with the
    /// new ones, and rebuilding only the resulting tuples, we keep
    /// incremental edits O(corpus) instead of O(corpus²) over many
    /// successive saves and avoid losing Phase B/C edges on the file just
    /// touched. Because `petgraph::DiGraph` does not stably remove nodes
    /// (`remove_node` is a swap-remove that invalidates trailing indices),
    /// an in-place patch is impractical — we instead rebuild the whole graph
    /// from a corpus snapshot the caller supplies. The caller already holds
    /// the corpus map (`CodeIndexer::chunks`), so the snapshot is cheap.
    /// What: keeps every chunk tuple whose `file` differs from `file_path`,
    /// appends the rebuilt tuples for the new chunks, and runs
    /// `build_from_chunks_with_entities` on the result. The caller is
    /// responsible for persisting via [`Self::save_to_corpus`] afterwards.
    /// Test: covered by `test_update_file_drops_old_edges_and_wires_new`.
    pub fn update_file(
        &mut self,
        existing: &[ChunkTuple],
        existing_entities: &[(String, Vec<RawEntity>)],
        file_path: &str,
        new_chunks: &[ChunkTuple],
        new_entities: &[RawEntity],
    ) {
        let mut merged: Vec<ChunkTuple> = existing
            .iter()
            .filter(|t| t.1 != file_path)
            .cloned()
            .collect();
        merged.extend(new_chunks.iter().cloned());

        let mut merged_ents: Vec<(String, Vec<RawEntity>)> = existing_entities
            .iter()
            .filter(|(f, _)| f != file_path)
            .cloned()
            .collect();
        if !new_entities.is_empty() {
            merged_ents.push((file_path.to_string(), new_entities.to_vec()));
        }

        *self = Self::build_from_chunks_with_entities(&merged, &merged_ents);
    }

    /// Remove every node / edge attributed to `file_path` (issue #41 phase 2).
    ///
    /// Why: a file deletion (`POST /indexes/:id/remove-file` or a
    /// `FileWatcher` rename event) must purge that file's symbols from the
    /// graph so subsequent KG expansions don't surface stale chunks. Like
    /// `update_file`, the lack of stable petgraph node removal makes a
    /// rebuild-from-snapshot the simplest correct implementation.
    /// What: filters the supplied corpus snapshot to exclude tuples whose
    /// `file` matches `file_path`, then runs
    /// `build_from_chunks_with_entities` on the survivors. Caller is
    /// responsible for persisting via [`Self::save_to_corpus`] afterwards.
    /// Test: covered by `test_remove_file_drops_file_symbols`.
    pub fn remove_file(
        &mut self,
        existing: &[ChunkTuple],
        existing_entities: &[(String, Vec<RawEntity>)],
        file_path: &str,
    ) {
        let kept: Vec<ChunkTuple> = existing
            .iter()
            .filter(|t| t.1 != file_path)
            .cloned()
            .collect();
        let kept_ents: Vec<(String, Vec<RawEntity>)> = existing_entities
            .iter()
            .filter(|(f, _)| f != file_path)
            .cloned()
            .collect();
        *self = Self::build_from_chunks_with_entities(&kept, &kept_ents);
    }
}
