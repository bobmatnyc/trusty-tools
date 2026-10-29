//! The exact-match corpus lane (#7675): every chunk carrying a literal.
//!
//! Why: split out of `exact.rs` to keep that file under the 500-SLOC cap; a
//! child module, so it shares the parent's private ranking helpers.
//! What: [`CodeIndexer::exact_match_lane`] and its BM25 postings prefilter.
//! Test: `crates/trusty-search/src/core/indexer/tests/exact_match_floor.rs`.

use std::sync::atomic::Ordering;

use regex::Regex;

use super::{
    as_hit_input, filename_match_tier, rank_hits, ExactHit, ExactLaneOutcome, ExactLiteral,
    LiteralShape, FILENAME_CONTENT_HIT_CAP, FILENAME_HIT_CAP,
};
use crate::core::indexer::{CodeIndexer, RawChunk, SearchMode};

/// Which chunks the lane has to look at.
enum Candidates {
    /// The BM25 postings intersection — a superset of the verbatim matches.
    Postings(Vec<String>),
    /// Every chunk in the map, because no usable candidate set was derivable.
    AllChunks,
}

impl CodeIndexer {
    /// Chunk ids whose BM25 postings carry every token of `lit`.
    ///
    /// Why: matching the literal against every chunk's content is `O(corpus)`
    /// in bytes and was the lane's whole cost — on a 100k-chunk index that is
    /// tens of milliseconds against a documented sub-10ms p50 target. A chunk
    /// containing a literal verbatim necessarily contains all of its tokens, so
    /// the postings intersection is a sound candidate set to verify.
    /// What: tokenizes the literal with the same tokenizer the corpus was
    /// indexed with, intersects the postings, and returns the ids beside the
    /// BM25 live-document count the caller checks completeness against.
    /// `None` whenever the prefilter cannot be trusted — a literal that
    /// tokenizes to nothing, or an empty/evicted BM25 index — and the caller
    /// then scans. A filename literal uses it for its content hits only; its
    /// path hits are matched against every chunk's path (#7675).
    ///
    /// Read OUTSIDE the `chunks` read lock deliberately: nesting the two read
    /// guards would order this lane's locks against the ingest path's.
    /// Test: `the_postings_candidate_path_and_the_full_scan_agree`.
    async fn exact_postings_candidates(&self, lit: &ExactLiteral) -> Option<(Vec<String>, usize)> {
        let terms = trusty_common::bm25::tokenize(&lit.text);
        if terms.is_empty() {
            return None;
        }
        // #10193: the query's short rehydrate wait; a cold index scans instead.
        if !self.await_corpus_for_query().await {
            return None;
        }
        let bm25 = self.bm25.read().await;
        if bm25.is_empty() {
            return None;
        }
        let ids = bm25.docs_containing_all(&terms)?;
        let live = bm25.len();
        Some((ids.into_iter().map(str::to_string).collect(), live))
    }

    /// Corpus lane: every chunk carrying `lit`, best-first, capped at `want`.
    ///
    /// Why: BM25 cannot be relied on to RANK a literal — it splits compound
    /// identifiers into common words and scores a chunk that merely shares those
    /// words above the one chunk that carries the identifier whole. Verifying
    /// the literal directly is the only lane that cannot miss. See #7675.
    /// What: takes candidates from the BM25 postings when they are complete
    /// (`bm25.len() >= chunks.len()`, so no chunk was dropped by the corpus
    /// cap) and falls back to every chunk otherwise, reporting that fallback as
    /// `full_scan`. Mirrors `grep_fallback_search`'s rehydrate-race handling,
    /// honours the same path/repo `filter` every other lane takes, and reports
    /// an exhausted rehydrate as `degraded` rather than as an empty lane.
    ///
    /// It also applies `mode`'s own file-type and docstring rules HERE rather
    /// than leaving them to `apply_archive_downrank`, which runs after
    /// truncation: a promoted hit the mode filter then deletes costs a `top_k`
    /// slot and returns nothing in its place, which is how this lane first
    /// emptied `test_code_mode_source_outranks_changelog_pre_truncation`.
    ///
    /// `tie_scores` (#7775) looks a candidate's fused lane score up by chunk
    /// id. It orders same-tier [`LiteralShape::Filename`] hits — and nothing
    /// else, see [`rank_hits`] — so the `FILENAME_HIT_CAP` truncation below
    /// keeps the best-ranked members of a tie rather than the
    /// alphabetically-first ones. `None` (every caller that has no fused list
    /// yet) leaves every hit at `0.0`, which is the pre-#7775 chunk-id order.
    ///
    /// A [`LiteralShape::Filename`] literal returns its path hits (every chunk,
    /// capped by `FILENAME_HIT_CAP`) followed by its content hits (postings
    /// candidates, capped by `FILENAME_CONTENT_HIT_CAP`); `full_scan` then
    /// describes the content pass (#7675).
    /// Test: `conceptual_ranking_is_unchanged_by_the_floor`,
    /// `a_filename_querys_content_hits_are_capped_apart_from_its_path_hits`,
    /// `a_filename_query_with_no_content_hits_returns_only_its_path_hits`,
    /// `exact_match_lane_degrades_observably_on_exhausted_retries`,
    /// `the_postings_candidate_path_and_the_full_scan_agree`,
    /// `test_code_mode_source_outranks_changelog_pre_truncation`,
    /// `tied_filename_hits_are_ordered_by_lane_score_not_chunk_id`.
    pub(crate) async fn exact_match_lane(
        &self,
        lit: &ExactLiteral,
        re: &Regex,
        want: usize,
        mode: SearchMode,
        filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
        tie_scores: Option<&(dyn Fn(&str) -> f32 + Send + Sync)>,
    ) -> ExactLaneOutcome {
        if want == 0 {
            return ExactLaneOutcome::default();
        }
        let tie_score = |id: &str| tie_scores.map_or(0.0, |f| f(id));
        let mode_admits = |raw: &RawChunk| {
            if matches!(mode, SearchMode::Code)
                && matches!(raw.chunk_type, crate::core::chunker::ChunkType::Docstring)
            {
                return false;
            }
            super::super::docs_penalty::is_allowed_for_mode(&raw.file, mode)
        };
        let admitted = |raw: &RawChunk| filter.is_none_or(|f| f(&raw.id)) && mode_admits(raw);
        let postings = self.exact_postings_candidates(lit).await;
        for _ in 0..super::super::lanes::REHYDRATE_RACE_RETRIES {
            // #10193: the query's short rehydrate wait, shared with the other lanes.
            if !self.await_corpus_for_query().await {
                break;
            }
            let chunks = self.chunks.read().await;
            if chunks.is_empty() && self.chunks_evicted.load(Ordering::Relaxed) {
                continue;
            }
            // A corpus-capped BM25 index holds fewer documents than the chunk
            // map, so its postings can miss a literal outright. Completeness is
            // the precondition for using them at all.
            let candidates = match &postings {
                Some((ids, live)) if *live >= chunks.len() => Candidates::Postings(ids.clone()),
                _ => Candidates::AllChunks,
            };
            let content_pool = |keep: &dyn Fn(&RawChunk) -> bool| -> Vec<ExactHit> {
                let inputs: Box<dyn Iterator<Item = &RawChunk>> = match &candidates {
                    Candidates::Postings(ids) => {
                        Box::new(ids.iter().filter_map(|id| chunks.get(id.as_str())))
                    }
                    Candidates::AllChunks => Box::new(chunks.values()),
                };
                rank_hits(
                    inputs
                        .filter(|raw| admitted(raw) && keep(raw))
                        .map(|raw| as_hit_input(raw, tie_score(&raw.id))),
                    lit,
                    re,
                )
            };
            let hits = if matches!(lit.shape, LiteralShape::Filename) {
                // #7675 round 3: path hits get the small, dedicated
                // `FILENAME_HIT_CAP` on top of (never instead of) `want` — see
                // that constant's doc for why `want` alone does not bound an
                // ambiguous basename's flood. Path matching reads every chunk.
                let is_path = |raw: &RawChunk| filename_match_tier(&raw.file, &lit.text).is_some();
                let mut path_hits = rank_hits(
                    chunks
                        .values()
                        .filter(|raw| admitted(raw) && is_path(raw))
                        .map(|raw| as_hit_input(raw, tie_score(&raw.id))),
                    lit,
                    re,
                );
                path_hits.truncate(want.min(FILENAME_HIT_CAP));
                // #7675: content hits (owner ruling 2026-10-26) take their own
                // cap and only the slots the path hits left, so they never
                // change how many path hits survive.
                let mut content_hits = content_pool(&|raw| !is_path(raw));
                content_hits.truncate(
                    want.saturating_sub(path_hits.len())
                        .min(FILENAME_CONTENT_HIT_CAP),
                );
                path_hits.append(&mut content_hits);
                path_hits
            } else {
                let mut hits = content_pool(&|_| true);
                hits.truncate(want);
                hits
            };
            return ExactLaneOutcome {
                hits,
                degraded: false,
                full_scan: matches!(candidates, Candidates::AllChunks),
            };
        }
        // The chunk map never rehydrated within the bounded waits. #7675: the
        // sibling lanes (`bm25_search`, `grep_fallback_search`) flip the sticky
        // per-index flag and the shared gauge here, because an empty lane is
        // otherwise bit-for-bit indistinguishable from "the literal is absent".
        // This lane makes exactly the same claim and owes the same signal.
        self.lane_degraded.store(true, Ordering::Relaxed);
        metrics::gauge!("trusty_bm25_lane_degraded", "index" => self.index_id.clone()).set(1.0);
        metrics::counter!("trusty_exact_match_lane_degraded_total", "index" => self.index_id.clone())
            .increment(1);
        tracing::warn!(
            "index '{}': chunk rehydrate still not ready within the query's rehydrate wait — \
             returning an empty exact-match lane for this query (degraded, not a literal that \
             is absent; issues #7675, #10193)",
            self.index_id
        );
        ExactLaneOutcome {
            hits: Vec::new(),
            degraded: true,
            full_scan: false,
        }
    }
}
