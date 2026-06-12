//! Result-materialisation pass for [`CodeIndexer::search`].
//!
//! Why: extracted from `search/mod.rs` (issue #607) to keep the parent file
//! under the 500-SLOC hard cap. Materialisation is orthogonal to the lane
//! orchestration in `search/mod.rs` — it converts fused `(id, score)` pairs
//! into fully-populated `CodeChunk`s.
//! What: `materialize_search_results` — the final pass that batch-reads chunk
//! text from the durable redb corpus and joins `(id, score)` pairs against the
//! per-lane hit sets to produce `match_reason`.
//! Test: covered by every `test_search_*` integration test in `indexer::tests`.

use std::collections::HashSet;

use crate::core::git::normalize_path;

use super::super::helpers::compute_match_reason;
use super::super::{build_compact_snippet, raw_to_code_chunk, CodeChunk, CodeIndexer, SearchQuery};

impl CodeIndexer {
    /// Materialize the top-k `(id, score)` pairs into `CodeChunk`s with the
    /// correct `match_reason` derived from the source lanes.
    ///
    /// Why: isolates the final per-result loop (lookup table joins, snippet
    /// construction, RawChunk → CodeChunk) so `search` stays focused on
    /// orchestration. Reading chunk text from redb at materialisation time
    /// serves bytes from the OS page cache rather than the heap.
    /// What: builds lookup sets for HNSW and BM25 hit IDs, then for each of
    /// the top-k `(id, score)` pairs picks a `match_reason` and emits a
    /// `CodeChunk` via `raw_to_code_chunk`.
    /// Test: covered by every search integration test.
    pub(super) async fn materialize_search_results(
        &self,
        all: Vec<(String, f32)>,
        hnsw_results: &[(String, f32)],
        bm25_results: &[(String, f32)],
        kg_ids: &HashSet<String>,
        branch_files: Option<&HashSet<String>>,
        query: &SearchQuery,
    ) -> Vec<CodeChunk> {
        let in_hnsw: HashSet<&String> = hnsw_results.iter().map(|(id, _)| id).collect();
        let in_bm25: HashSet<&String> = bm25_results.iter().map(|(id, _)| id).collect();

        let top_k: Vec<(String, f32)> = all.into_iter().take(query.top_k).collect();
        let top_k_ids: Vec<String> = top_k.iter().map(|(id, _)| id.clone()).collect();
        let chunks = self.fetch_chunks_for_ids(&top_k_ids).await;
        let mut out = Vec::with_capacity(top_k.len());
        for (id, score) in top_k {
            let Some(raw) = chunks.get(&id) else {
                tracing::trace!("fused id {id} not in corpus — likely race; skipping");
                continue;
            };
            let match_reason = compute_match_reason(
                in_hnsw.contains(&id),
                in_bm25.contains(&id),
                kg_ids.contains(&id),
            );
            let snippet = if query.compact {
                Some(build_compact_snippet(&raw.content))
            } else {
                None
            };
            let mut chunk = raw_to_code_chunk(raw, score, match_reason, snippet, &self.root_path);
            if let Some(set) = branch_files {
                chunk.on_branch = set.contains(normalize_path(&raw.file));
            }
            out.push(chunk);
        }
        out
    }
}
