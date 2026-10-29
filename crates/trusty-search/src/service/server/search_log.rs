//! The search log lines: per-stage timings, the fan-out line, and the log a
//! search cancelled at the deadline leaves behind (#10195).
//!
//! Why: the daemon log is the only place search latency can be attributed
//! today, and it carried one number per search. A search cancelled at the 30 s
//! deadline logged no index id, `search_all` logged at debug (below the
//! daemon's `info` filter), and `latency_ms` started after the cold load.
//! What: [`RequestTimings`] (the per-stage record in the log line and in
//! `meta.timings`), [`log_search`], [`log_fanout`], and [`CancelGuard`], which
//! logs `query_timeout` with the index, query and stage when the deadline
//! drops a search.
//! Test: `service::server::tests_10195`.

use std::time::Instant;

use serde::Serialize;

use crate::core::indexer::{query_timings, SearchTimings};
use crate::core::registry::IndexId;

/// The longest query prefix a log line carries, in bytes.
const LOGGED_QUERY_BYTES: usize = 80;

/// Milliseconds since `since`, saturating.
pub(super) fn ms_since(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// One per-index search's stage timings, as logged and as `meta.timings`.
///
/// Why (#10195): a slow search must say where its time went.
/// What: `resolve_ms` (index lookup, including a cold lazy load), `lock_ms`
/// (waiting for the index read lock), the indexer's [`SearchTimings`], and
/// `total_ms`, the full span from request start, equal to `latency_ms`.
/// Test: `search_reports_per_stage_timings_in_log_and_meta`.
#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct RequestTimings {
    pub resolve_ms: u64,
    pub lock_ms: u64,
    #[serde(flatten)]
    pub search: SearchTimings,
    pub total_ms: u64,
}

impl RequestTimings {
    /// Add these timings to the reply's `meta` block as `meta.timings`.
    pub(super) fn attach(&self, body: &mut serde_json::Value) {
        if let (Some(meta), Ok(t)) = (
            body.get_mut("meta").and_then(|m| m.as_object_mut()),
            serde_json::to_value(self),
        ) {
            meta.insert("timings".into(), t);
        }
    }
}

/// The `info` line every per-index search writes on success.
pub(super) fn log_search(
    index_id: &IndexId,
    intent: &str,
    results: usize,
    query: &str,
    t: &RequestTimings,
) {
    let s = &t.search;
    tracing::info!(
        index_id = %index_id,
        intent = %intent,
        latency_ms = t.total_ms,
        results,
        query = %crate::truncate_at_char_boundary(query, LOGGED_QUERY_BYTES),
        resolve_ms = t.resolve_ms,
        lock_ms = t.lock_ms,
        embed_ms = s.embed_ms,
        vector_ms = s.vector_ms,
        lexical_ms = s.lexical_ms,
        ripgrep_ms = s.ripgrep_ms,
        fusion_ms = s.fusion_ms,
        rehydrate_wait_ms = s.rehydrate_wait_ms,
        rehydrate_ran = s.rehydrate_ran,
        hnsw_promoted = s.hnsw_promoted,
        "search"
    );
}

/// The `info` line every `search_all` fan-out writes (#10195).
///
/// Why: the fan-out logged at debug, below the daemon's `info` filter, so
/// `search_all` calls never reached the log.
/// What: one line naming the target index ids, how many answered, the embed
/// and total time.
/// Test: `search_all_logs_one_info_line_with_its_target_indexes`.
pub(super) fn log_fanout(
    targets: &[IndexId],
    searched: usize,
    deadline_skipped: usize,
    embed_ms: u64,
    latency_ms: u64,
    query: &str,
) {
    let ids = targets.iter().map(|i| i.0.as_str()).collect::<Vec<_>>();
    tracing::info!(
        target_indexes = %ids.join(","),
        indexes_searched = searched,
        deadline_skipped,
        embed_ms,
        latency_ms,
        query = %crate::truncate_at_char_boundary(query, LOGGED_QUERY_BYTES),
        "search_all"
    );
}

/// Logs `query_timeout` when the interactive deadline drops a search.
///
/// Why (#10195): the deadline cancels the search future, so the search never
/// reaches its own log line, and the deadline wrapper knows neither the index
/// nor the stage. The guard lives inside the search and is dropped with it.
/// What: holds borrowed text and the request start; on drop, unless
/// [`Self::done`] ran, reads the query scope and logs at WARN with
/// `index_id`, `query`, `stage` and `elapsed_ms` when the deadline has
/// passed. An error returned before the deadline logs nothing here.
/// Test: `a_search_cancelled_at_the_deadline_logs_index_query_and_stage`.
pub(super) struct CancelGuard<'a> {
    index_id: &'a str,
    query: &'a str,
    started: Instant,
    armed: bool,
}

impl<'a> CancelGuard<'a> {
    pub(super) fn new(index_id: &'a str, query: &'a str, started: Instant) -> Self {
        Self {
            index_id,
            query,
            started,
            armed: true,
        }
    }

    /// The search answered; dropping the guard logs nothing.
    pub(super) fn done(mut self) {
        self.armed = false;
    }
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some((stage, true)) = query_timings::scope_state() {
            tracing::warn!(
                index_id = %self.index_id,
                query = %crate::truncate_at_char_boundary(self.query, LOGGED_QUERY_BYTES),
                stage = %stage.as_str(),
                elapsed_ms = ms_since(self.started),
                "query_timeout: search cancelled at the deadline"
            );
            // #10195: tell the deadline wrapper this cancellation is logged.
            query_timings::mark_timeout_logged();
        }
    }
}
