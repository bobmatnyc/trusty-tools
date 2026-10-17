//! The `meta` block of a per-index search response.
//!
//! Why: #9970 adds `meta.embedder`, and `search.rs` had no room left under the
//! 500-line cap; the block is also the part of `search_report` that grows with
//! each new caller-facing signal.
//! What: [`SearchMeta`] carries the values `search_report` computed and
//! renders them, plus `embedder` from the installed `SwitchableEmbedder`.
//! Test: `search_handler_meta_includes_stale_index_root_field` and the other
//! `meta` tests; `search_carries_the_note_while_the_bootstrap_runs` in
//! `tests_9970_embedder_note.rs`.

use serde_json::Value;

use super::state::SearchAppState;
use crate::core::bm25::Bm25Truncation;
use crate::core::indexer::{ExactMatchReport, SearchDrops};
use crate::service::embedder_supervisor::embedder_view::EmbedderView;

/// Everything `search_report` knows about one query beyond its rows.
pub(super) struct SearchMeta {
    pub last_indexed: Option<String>,
    pub results_may_be_stale: bool,
    pub caps: Vec<&'static str>,
    pub stale_index_root: bool,
    pub dropped: SearchDrops,
    pub bm25_lane_degraded: bool,
    pub bm25: Option<Bm25Truncation>,
    pub vector_unavailable: bool,
    pub embedder_error: Option<String>,
    pub vector_disabled_by_config: bool,
    pub exact_match: ExactMatchReport,
}

/// The `embedder` object (#9970): active and intended backend, bootstrap
/// step and agent note. `None` before the daemon installs its switchable
/// embedder (tests, and the init window before it).
/// Test: `search_carries_the_note_while_the_bootstrap_runs`.
pub(super) fn embedder_entry(state: &SearchAppState) -> Option<Value> {
    let switchable = state.current_switchable_embedder()?;
    serde_json::to_value(EmbedderView::of(&switchable)).ok()
}

impl SearchMeta {
    /// Render the `meta` object, `embedder` included when one is installed.
    pub(super) fn into_json(self, state: &SearchAppState) -> Value {
        let Self {
            last_indexed,
            results_may_be_stale,
            caps,
            stale_index_root,
            dropped,
            bm25_lane_degraded,
            bm25,
            vector_unavailable,
            embedder_error,
            vector_disabled_by_config,
            exact_match,
        } = self;
        let mut meta = serde_json::json!({
            "last_indexed": last_indexed,
            "results_may_be_stale": results_may_be_stale,
            // Issue #109 Phase 1: surface which lanes contributed to this
            // result set. Lets clients display "lexical-only" badges or
            // retry once the semantic lane is ready.
            "search_capabilities": caps,
            // Issue #541: machine-readable signal that results were dropped
            // because the index root is stale. Clients (Claude Code, UI) can
            // show a remediation banner without log scraping. `false` is the
            // normal case (no drops); `true` means the operator should run
            // `trusty-search index <path>` to re-register with a fresh root.
            "stale_index_root": stale_index_root,
            // #2203: how many candidates this query retrieved and then
            // discarded, per drop site. Without it a short `results` array was
            // indistinguishable from a small match set — `top_k: 10` returning
            // 3 rows read the same whether 3 matched or 7 were deleted on the
            // way out, which is how #2203 presented as "search is broken" on an
            // intact corpus. `unresolved_corpus` is the fault case (the corpus
            // could not be read); the rest are the requested `mode` /
            // `exclude_archived` / root scope doing their job.
            "dropped": dropped,
            "dropped_total": dropped.total(),
            // Issue #3683 code-critic review finding 3 (HIGH): `true` means
            // this query's lexical lane (BM25 and/or grep-fallback) degraded
            // to empty/partial because the detached corpus rehydrate hadn't
            // converged within its bounded wait — NOT a genuine "no match"
            // result. Clients should treat results under a `true` flag as
            // provisional and may retry shortly; the background rehydrate
            // keeps running regardless and the next query is warm.
            "bm25_lane_degraded": bm25_lane_degraded,
            // #9235: `true` when the BM25 corpus cap kept resident chunks out
            // of the lexical lane; those chunks can only match semantically.
            "bm25_truncated": bm25.map(|b| b.truncated),
            "bm25_docs_dropped": bm25.map(|b| b.docs_dropped),
            "bm25_corpus_cap": bm25.map(|b| b.corpus_cap),
            "bm25_truncation_unavailable_reason":
                bm25.is_none().then_some(Bm25Truncation::UNAVAILABLE_REASON),
            // #5068: the counterpart flag to `bm25_lane_degraded` for the OTHER
            // lane. `true` means this hybrid query ran with no vector
            // contribution at all — the results are lexical, however
            // conceptual the query was. A pinned `stage: semantic` gets a 503
            // instead (see `degraded::vector_lane_unavailable`); this flag is
            // for the unpinned caller, who legitimately gets whatever lanes are
            // ready but must be able to tell which ones those were without
            // diffing `search_capabilities` against a schema it does not have.
            // #8348: also `true` when the query embed failed and the lane was skipped.
            "vector_unavailable": vector_unavailable,
            "embedder_error": embedder_error,
            // #5068: separates "off for this index" from "not built yet" — the
            // same split `vector_unavailable`'s 503 body carries, so a caller
            // handles one contract, not two.
            "vector_disabled_by_config": vector_disabled_by_config,
            // #7675: `true` when a literal the query named was found verbatim in
            // the corpus and every chunk carrying it was floored above every
            // chunk that was not — so the top hit is an exact occurrence, not a
            // semantic guess. `exact_match_literal` is the text that was
            // matched, and is present even when the floor found nothing, so a
            // caller can tell "read as a literal query, no occurrence" from
            // "read as a conceptual query" (where it is `null`).
            "exact_match_floor": exact_match.applied,
            "exact_match_literal": exact_match.literal,
            // #7675: the fail-open guard. `exact_match_degraded` is `true` when
            // the lane could not read the corpus at all, so an absent floor
            // means "the lane did not run", not "the literal is not there" —
            // the same distinction `bm25_lane_degraded` draws for its lane.
            // `exact_match_full_scan` says the BM25 postings prefilter was
            // unavailable and every chunk's content was matched instead.
            "exact_match_degraded": exact_match.degraded,
            "exact_match_full_scan": exact_match.full_scan,
        });
        // #9970: the agent note while the python embedder is not serving.
        if let (Some(entry), Some(map)) = (embedder_entry(state), meta.as_object_mut()) {
            map.insert("embedder".into(), entry);
        }
        meta
    }
}
