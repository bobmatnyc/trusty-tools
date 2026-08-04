//! Query-time guard for indexes whose durable corpus failed to open (#4087).
//!
//! Why: a corpus-failed index stays fully REGISTERED — it is in the registry,
//! `list_indexes` shows it, and its handle answers searches. But the quarantine
//! invariant (`core::indexer::quarantine`) guarantees it holds no corpus, so
//! every query resolves zero chunks and the handler returned `HTTP 200` with
//! `results: []`. That is a total search outage for the index dressed as a
//! successful, empty answer: the caller cannot distinguish "nothing matched"
//! from "this index is broken". A live daemon was observed serving three
//! indexes this way. Silence is the worst available failure mode here, so this
//! module makes the failure loud (a 5xx the caller must handle) and
//! recoverable (the state clears the moment a corpus open succeeds).
//!
//! What: [`corpus_failure_response`] renders the 503 body for a single-index
//! query; [`is_corpus_failed`] is the cheap predicate the global fan-out uses
//! to exclude — and then COUNT — an unserviceable index rather than folding
//! its empty lane silently into the fused result set.
//!
//! Test: `service::server::tests_4087`.

use axum::http::StatusCode;
use axum::Json;

use crate::core::registry::IndexHandle;

/// `true` when this index's durable corpus failed to open, so any search
/// against it can only return an empty result set (#4087).
///
/// Why: the fan-out needs a predicate, not a response body, and must not pay
/// for JSON construction per index.
/// What: takes the indexer read lock briefly and reads the quarantine flag.
/// Test: `global_search_excludes_and_counts_corpus_failed_indexes`.
pub(super) async fn is_corpus_failed(handle: &IndexHandle) -> bool {
    handle.indexer.read().await.corpus_open_failed
}

/// Build the 503 response for a query against a corpus-failed index (#4087).
///
/// Why: returning `200 []` told the caller its query simply had no matches.
/// A 503 with a named error code forces the caller (trusty-review, the MCP
/// tools, a human) to treat it as the outage it is. 503 rather than 500
/// because the state is recoverable by construction: a successful
/// `CorpusStore::open` lifts it (`clear_corpus_open_failure`), and for the
/// transient failure kinds (#4333) that can happen on the very next restart
/// with no operator action at all.
/// What: `None` when the index is healthy — the caller proceeds normally.
/// `Some((503, body))` otherwise, carrying the #4333 classification so the
/// caller learns whether to retry or to escalate. The body deliberately
/// repeats the classified reason verbatim rather than paraphrasing it, so a
/// consumer never sees a rebuild instruction for a transient timeout.
/// Test: `search_against_corpus_failed_index_returns_503_not_empty_200`.
pub(super) async fn corpus_failure_response(
    index_id: &str,
    handle: &IndexHandle,
) -> Option<(StatusCode, Json<serde_json::Value>)> {
    let indexer = handle.indexer.read().await;
    if !indexer.corpus_open_failed {
        return None;
    }
    let kind = indexer.corpus_open_failure;
    drop(indexer);

    let (failure_kind, transient, reason) = match kind {
        Some(k) => (k.label(), k.is_transient(), k.stage_reason()),
        // Defensive: the flag is set in lockstep with the kind, so this arm
        // is unreachable in practice. Report the flag rather than inventing a
        // classification — an unknown cause must never be called corruption.
        None => (
            "unclassified",
            false,
            "durable corpus is unavailable and the cause was not classified",
        ),
    };

    Some((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "index_corpus_unavailable",
            "index_id": index_id,
            "failure_kind": failure_kind,
            "transient": transient,
            "message": format!(
                "index '{index_id}' cannot be searched: its durable corpus failed to open, \
                 so every query would return an empty result set indistinguishable from \
                 'no matches' (issue #4087). {reason}"
            ),
        })),
    ))
}
