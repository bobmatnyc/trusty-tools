//! `MemoryService` recall: per-palace and cross-palace, ranked (#8246).
//!
//! Why: the chat tools and chat context injection ran their own recall and
//! skipped the stale-snapshot demotion every MCP recall applies (#8246 review).
//! Routing them through these methods gives one ranked recall per shape. Split
//! out of `core.rs`, which sat at the 500-SLOC cap.
//! What: [`MemoryService::recall_ranked`] (vector + BM25 over a widened
//! window, demoted, cut to `top_k`), [`MemoryService::recall`] (its JSON rows),
//! and [`MemoryService::recall_all`] (the cross-palace fan-out, demoted, with
//! the ADR-0071 coverage report). No user-scope rulings leg here: the
//! per-palace surfaces return a bare JSON array, which has nowhere to report
//! a failed rulings palace.
//! Test: `demotion_applies_on_every_recall_surface`
//! (`tests/recall_temporal_rank.rs`); `recall_entry_json_hoists_drawer_fields`;
//! `recall_all_never_opens_an_empty_palace` (#9141).

use serde_json::{json, Value};
use trusty_common::memory_core::retrieval::{
    recall_across_palaces_with_default_embedder, recall_deep_with_default_embedder,
    recall_with_default_embedder, RecallResult,
};

use super::core::MemoryService;
use super::helpers::recall_entry_json;
use super::recall_stream::{recall_all_scoped, RecallAllScope};
use super::types::{ServiceError, ServiceResult};
use crate::tools::recall_rank::{
    demote_stale_snapshots, demote_stale_snapshots_across, ranking_window,
};

impl MemoryService {
    /// Per-palace recall, ranked: the hits every non-MCP recall surface shows.
    ///
    /// Why: the chat `recall_memories` tool and the chat context injection
    /// called the retrieval layer directly and so skipped demotion (#8246).
    /// What: opens the palace, runs the shallow or deep vector recall and the
    /// optional BM25 lane over a [`ranking_window`] of candidates, RRF-fuses
    /// them, demotes stale snapshots, and cuts to `top_k`.
    /// Test: `demotion_applies_on_every_recall_surface`.
    pub async fn recall_ranked(
        &self,
        id: &str,
        query: &str,
        top_k: usize,
        deep: bool,
    ) -> ServiceResult<Vec<RecallResult>> {
        let handle = self.open_handle(id)?;
        // #8246: fetch past `top_k` so demotion can lift a hit over the cut.
        let window = ranking_window(top_k, None);
        let mut results = if deep {
            recall_deep_with_default_embedder(&handle, query, window).await
        } else {
            recall_with_default_embedder(&handle, query, window).await
        }
        .map_err(|e| ServiceError::internal(format!("recall: {e:#}")))?;
        // #5036: the lexical lane, keyed on the RESOLVED palace id, never the
        // caller's slug — `open_handle` follows aliases. `fuse_bm25_into_recall`
        // only BOOSTS drawers the vector lane already returned, so it has no
        // scaling constant that can degenerate on an empty surviving set.
        if let Some(hits) =
            crate::tools::bm25::bm25_search_optional(&self.state, handle.id.as_str(), query, window)
                .await
        {
            crate::tools::bm25::fuse_bm25_into_recall(&mut results, &hits, window);
        }
        // #8246: same stale-snapshot demotion as the MCP recall handlers.
        demote_stale_snapshots(&mut results, chrono::Utc::now());
        results.truncate(top_k);
        Ok(results)
    }

    /// Per-palace recall (semantic search), optionally with deep retrieval.
    ///
    /// Why: one JSON row shape for callers that want flattened drawers.
    /// What: [`Self::recall_ranked`], each hit as a `recall_entry_json` row
    /// (the issue #69 shape).
    /// Test: `recall_entry_json_hoists_drawer_fields`.
    pub async fn recall(
        &self,
        id: &str,
        query: &str,
        top_k: usize,
        deep: bool,
    ) -> ServiceResult<Value> {
        let results = self.recall_ranked(id, query, top_k, deep).await?;
        let payload: Vec<Value> = results.into_iter().map(recall_entry_json).collect();
        Ok(json!(payload))
    }

    /// Cross-palace recall over the default (resident) scope.
    ///
    /// Why (#9299, ADR-0071 D1): the default no longer opens palaces; this
    /// keeps the old signature for in-process callers.
    /// What: [`Self::recall_all_scoped`] with [`RecallAllScope::Resident`].
    /// Test: `default_scope_opens_no_palace_and_keeps_the_resident_set`.
    pub async fn recall_all(&self, query: &str, top_k: usize, deep: bool) -> Value {
        self.recall_all_scoped(query, top_k, deep, RecallAllScope::Resident)
            .await
    }

    /// Cross-palace recall over `scope`, with the coverage report.
    ///
    /// Why: shared by the chat `memory_recall_all` tool and in-process callers;
    /// one fan-out-merge path avoids drift. #9299 (ADR-0071 D4): the response
    /// says how much of the estate it searched, so it moved from a bare array
    /// to an object.
    /// What: runs `recall_all_scoped` over a [`ranking_window`] of candidates,
    /// demotes stale snapshots (#8246), cuts to `top_k`, and returns
    /// `{"results": [...]}` plus the D4 coverage fields. Each hit carries its
    /// palace id. A listing or search error returns `{"error": ...}`.
    /// Test: `demotion_applies_on_every_recall_surface`;
    /// `open_failure_is_reported_on_every_surface`;
    /// `recall_all_returns_open_palaces_to_baseline`.
    pub async fn recall_all_scoped(
        &self,
        query: &str,
        top_k: usize,
        deep: bool,
        scope: RecallAllScope,
    ) -> Value {
        let window = ranking_window(top_k, None);
        let outcome = recall_all_scoped(
            &self.state,
            scope,
            "recall_all",
            window,
            |handles| async move {
                recall_across_palaces_with_default_embedder(&handles, query, window, deep).await
            },
        )
        .await;
        match outcome {
            Ok(outcome) => {
                let mut results = outcome.results;
                // #8246: same demotion as `memory_recall_all`, then the cut.
                demote_stale_snapshots_across(&mut results, chrono::Utc::now());
                results.truncate(top_k);
                let rows: Vec<Value> = results
                    .into_iter()
                    .map(|r| {
                        json!({
                            "palace_id": r.palace_id,
                            "drawer_id": r.result.drawer.id.to_string(),
                            "content": r.result.drawer.content(),
                            "importance": r.result.drawer.importance,
                            "tags": r.result.drawer.tags,
                            "score": r.result.score,
                            "layer": r.result.layer,
                        })
                    })
                    .collect();
                let mut out = serde_json::Map::new();
                out.insert("results".into(), json!(rows));
                outcome.coverage.insert_into(scope, &mut out);
                Value::Object(out)
            }
            Err(e) => json!({ "error": format!("recall_across_palaces: {e:#}") }),
        }
    }
}
