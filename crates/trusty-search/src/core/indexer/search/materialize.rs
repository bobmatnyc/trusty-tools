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

use crate::core::chunker::ChunkType;
use crate::core::git::normalize_path;

use super::super::archive::{self, MarkerCache};
use super::super::docs_penalty;
use super::super::helpers::compute_match_reason;
use super::super::{
    build_compact_snippet, raw_to_code_chunk, CodeChunk, CodeIndexer, SearchMode, SearchQuery,
};
use super::drops::SearchDrops;

/// The per-row filters `materialize_search_results` applies while it fills the
/// page (#9404).
#[derive(Debug, Clone, Copy)]
pub(super) struct PageFilters {
    /// Drop rows whose file `docs_penalty::is_allowed_for_mode` rejects for
    /// this mode. `None` keeps every file type.
    pub(super) file_mode: Option<SearchMode>,
    /// Withhold docstring rows, with the all-docstring fallback (`Code` mode).
    pub(super) drop_docstrings: bool,
    /// Drop rows with a strong archive signal (`exclude_archived`).
    pub(super) drop_archived: bool,
}

impl CodeIndexer {
    /// Materialize the top-k `(id, score)` pairs into `CodeChunk`s with the
    /// correct `match_reason` derived from the source lanes.
    ///
    /// Why: isolates the final per-result loop (lookup table joins, snippet
    /// construction, RawChunk → CodeChunk) so `search` stays focused on
    /// orchestration. Reading chunk text from redb at materialisation time
    /// serves bytes from the OS page cache rather than the heap. #9404: the
    /// mode's file-type filter and `Code` mode's docstring filter run here,
    /// while the page is filled, because filtering after the `top_k` cut
    /// emptied a page whose every slot held a filtered row while matching code
    /// sat deeper in `all`. The `exclude_archived` filter runs here for the
    /// same reason.
    /// What: walks `all` in rank order, fetching rows one deficit-sized batch
    /// at a time, until `top_k` slots are used. An emitted chunk or an id with
    /// no row (counted in `unresolved_corpus`) uses a slot. A row whose
    /// resolved file `filters.file_mode` rejects is skipped, counted in
    /// `mode_filtered`, and frees its slot; it is never returned, because the
    /// mode names the file types the caller asked for. With
    /// `filters.drop_archived`, a row `archive::classify` labels with a strong
    /// signal (any reason but `stale:`) is skipped the same way, counted in
    /// `archived`, and never returned. With
    /// `filters.drop_docstrings`, a docstring row is skipped the same way and
    /// counted in `docstring_filtered`. If that leaves the page empty, the
    /// skipped docstrings (up to `top_k`, in rank order) are returned instead
    /// and `docstring_filtered` reads `0`, so a docstring-only match is never
    /// an empty success. Survivors keep their fused order, and the page never
    /// exceeds `top_k`.
    ///
    /// Errors when the durable corpus read fails (#5917): every id would land
    /// in the drop block, so the result set would be empty for a reason the
    /// tally cannot express.
    /// Test: `bugdebt_query_backfills_code_past_a_docstring_filled_top_k`,
    /// `docstring_only_matches_return_the_docstrings_not_an_empty_page`,
    /// `bugdebt_query_backfills_code_past_doc_files_filling_top_k`,
    /// `doc_file_only_matches_return_an_empty_page_with_every_row_counted`,
    /// `exclude_archived_backfills_code_past_archived_chunks_filling_top_k`,
    /// `archived_only_matches_return_an_empty_page_with_every_row_counted`;
    /// `search_handler_meta_reports_rows_dropped_when_the_corpus_has_no_matching_row`
    /// in `service::server::tests_search` pins the unresolved count.
    // #9404: the page filters are the eighth argument.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn materialize_search_results(
        &self,
        all: Vec<(String, f32)>,
        hnsw_results: &[(String, f32)],
        bm25_results: &[(String, f32)],
        kg_ids: &HashSet<String>,
        branch_files: Option<&HashSet<String>>,
        query: &SearchQuery,
        filters: PageFilters,
    ) -> anyhow::Result<(Vec<CodeChunk>, SearchDrops)> {
        let in_hnsw: HashSet<&String> = hnsw_results.iter().map(|(id, _)| id).collect();
        let in_bm25: HashSet<&String> = bm25_results.iter().map(|(id, _)| id).collect();

        let mut out = Vec::with_capacity(query.top_k);
        let mut dropped = SearchDrops::default();
        // #9404: the docstrings the filter skipped, kept for the
        // all-docstring fallback below.
        let mut withheld_docstrings: Vec<CodeChunk> = Vec::new();
        let mut slots_used = 0_usize;
        let mut remaining = all.into_iter();
        // #7434: one root table per query, not per hit.
        let roots = self.chunk_roots();
        let mut markers = MarkerCache::new();
        while slots_used < query.top_k {
            let batch: Vec<(String, f32)> =
                remaining.by_ref().take(query.top_k - slots_used).collect();
            if batch.is_empty() {
                break;
            }
            let batch_ids: Vec<String> = batch.iter().map(|(id, _)| id.clone()).collect();
            let chunks = self.fetch_chunks_for_ids(&batch_ids).await?;
            for (id, score) in batch {
                let Some(raw) = chunks.get(&id) else {
                    // The id ranked into the top-k but the corpus held no row
                    // for it — the chunk was removed while the query ran. Since
                    // #5917 a FAILED durable read no longer reaches here: it
                    // errors one frame up instead of falling back to an
                    // in-memory map that is empty after idle eviction, which
                    // used to land every id in this arm and answer with an
                    // empty result set.
                    dropped.unresolved_corpus += 1;
                    slots_used += 1;
                    tracing::debug!(
                        index_id = %self.index_id,
                        chunk_id = %id,
                        "search: no corpus row for fused id — dropping it from the results"
                    );
                    continue;
                };
                // #9404: checked against the resolved path, as the retain it
                // replaces was, and before the docstring check, so a rejected
                // docstring never becomes a fallback row.
                let file = roots.resolve_absolute(&raw.file);
                let file = file.to_string_lossy();
                if let Some(mode) = filters.file_mode {
                    if !docs_penalty::is_allowed_for_mode(&file, mode) {
                        dropped.mode_filtered += 1;
                        continue;
                    }
                }
                // #9404: the same inputs `apply_archive_downrank` classifies
                // (resolved path, chunk text). `stale:` is a soft signal and
                // stays. Checked before the docstring filter, so an archived
                // docstring never becomes a fallback row.
                if filters.drop_archived {
                    let (_, reason) =
                        archive::classify(&self.root_path, &file, &raw.content, &mut markers);
                    if reason.is_some_and(|r| !r.starts_with("stale:")) {
                        dropped.archived += 1;
                        continue;
                    }
                }
                let is_withheld_docstring =
                    filters.drop_docstrings && matches!(raw.chunk_type, ChunkType::Docstring);
                if is_withheld_docstring {
                    dropped.docstring_filtered += 1;
                    if withheld_docstrings.len() >= query.top_k {
                        continue;
                    }
                }
                let match_reason = compute_match_reason(
                    in_hnsw.contains(&id),
                    in_bm25.contains(&id),
                    kg_ids.contains(&id),
                )
                .as_str();
                let snippet = if query.compact {
                    Some(build_compact_snippet(&raw.content))
                } else {
                    None
                };
                let mut chunk = raw_to_code_chunk(raw, score, match_reason, snippet, &roots);
                if let Some(set) = branch_files {
                    chunk.on_branch = set.contains(normalize_path(&raw.file));
                }
                if is_withheld_docstring {
                    withheld_docstrings.push(chunk);
                } else {
                    slots_used += 1;
                    out.push(chunk);
                }
            }
        }
        // #9404: every resolved candidate was a docstring. Returning nothing
        // would report a matching index as an empty success, so return them.
        if out.is_empty() && !withheld_docstrings.is_empty() {
            out = withheld_docstrings;
            dropped.docstring_filtered = 0;
        }
        let unresolved = dropped.unresolved_corpus;
        if unresolved > 0 {
            // #2203: a warn plus a Prometheus counter, matching how the
            // out-of-root drop reports itself in `service::server::search`.
            // Every caller of `search` gets this signal; only the HTTP handler
            // gets the per-query count in `meta.dropped`.
            metrics::counter!(
                "trusty_search_dropped_unresolved_corpus_total",
                "index_id" => self.index_id.clone(),
            )
            .increment(unresolved as u64);
            tracing::warn!(
                index_id = %self.index_id,
                dropped = unresolved,
                returned = out.len(),
                "search: dropped {unresolved} of {} ranked result(s) — the corpus \
                 returned no row for them, so they were removed while the query \
                 ran. It is never normal (a failed read errors instead since \
                 #5917).",
                unresolved + out.len(),
            );
        }
        Ok((out, dropped))
    }
}
