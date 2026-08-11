//! Loud-degradation responses: the one place a "this index cannot answer what
//! you asked" verdict is rendered (#4087, #5061, #5068).
//!
//! Why: a registered index can be unable to serve a request for three
//! structurally different reasons, and each one used to be reported as
//! something a caller could not act on — an empty `200`, a bare `503` with no
//! body, or BM25 results standing in for semantic ones. The common defect is
//! that the daemon answered as if it had succeeded. Every response here exists
//! to make one of those states distinguishable:
//!
//! | State | Verdict | Retryable |
//! |---|---|---|
//! | durable corpus failed to open (#4087) | `503 index_corpus_unavailable` | per #4333 classification |
//! | registered but not resident (#5061) | `503 index_not_resident` | yes — `search` reloads it |
//! | restore permanently failed (#5061) | `503 index_restore_failed` | no — operator action |
//! | not registered anywhere | `404 unknown index` | no |
//! | vector lane off by config (#5068) | `503 vector_unavailable` | no |
//! | vector lane not built yet (#5068) | `503 vector_unavailable` | yes |
//! | contribution stored, not merged (#5505) | `503 contrib_not_merged` | yes |
//!
//! Centralising them means `search`, `index_status`, `chunks`, and `grep` can
//! never drift into reporting the same daemon state three different ways —
//! which is exactly how #5061's "status says 404, search says 503" asymmetry
//! arose.
//!
//! What: [`corpus_failure_response`] renders the #4087 body; [`is_corpus_failed`]
//! is the cheap predicate the global fan-out uses to exclude — and then COUNT —
//! an unserviceable index; [`residency_miss_response`] renders the #5061
//! not-resident / restore-failed / unknown triple; [`vector_lane_unavailable`]
//! renders the #5068 semantic-against-a-vector-less-index verdict;
//! [`contrib_not_merged_response`] renders the #5505 stored-but-not-queryable
//! contributed-graph verdict.
//!
//! Test: `service::server::tests_4087` covers [`corpus_failure_response`];
//! `residency_miss_is_404_only_when_absent_everywhere`,
//! `cold_parked_status_503_body_names_search_as_the_restore_path`,
//! `restore_failed_status_503_body_is_not_retryable`, and
//! `search_residency_bodies_match_the_shared_contract` cover
//! [`residency_miss_response`];
//! `semantic_stage_on_skip_vector_index_is_503_not_degraded_200` and
//! `semantic_stage_before_embed_completes_is_503_retryable` cover
//! [`vector_lane_unavailable`].

use axum::http::StatusCode;
use axum::Json;

use crate::core::registry::{IndexHandle, IndexId, IndexStages, StageStatus};
use crate::service::lazy_loader::ColdIndexStore;

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

/// Resolve a hot-registry miss into the one verdict that describes it (#5061).
///
/// Why: `index_status`, `chunks`, and `grep` each grew their own version of
/// this three-way check, and two of them returned a bare `StatusCode` — an MCP
/// caller saw `returned 503 Service Unavailable: ` with empty text and had
/// nothing to branch on. Worse, the three disagreed with `search_handler`,
/// which reloads a cold-parked index transparently. A caller polling `status`
/// therefore saw a permanent-looking 503 for an index a plain `search` would
/// have restored on the spot. One builder means the four endpoints report the
/// same daemon state the same way, and the retryable case names the endpoint
/// that actually fixes it.
///
/// What: checks the failed set FIRST (a permanently-failed restore never clears
/// on its own, so "retry later" would be a lie for it), then the cold store,
/// then falls through to 404. `restore_via` on the not-resident arm is the
/// load-bearing field: `POST /indexes/{id}/search` is the only endpoint that
/// drives `get_or_load_index`, so it is how a caller un-sticks the 503 without
/// waiting for a sweep that may never come.
///
/// Callers MUST have already missed the hot registry — this never checks it.
/// Test: `residency_miss_is_404_only_when_absent_everywhere`,
/// `cold_parked_status_503_body_names_search_as_the_restore_path`.
pub(super) fn residency_miss_response(
    cold_store: &ColdIndexStore,
    index_id: &IndexId,
) -> (StatusCode, Json<serde_json::Value>) {
    let id = &index_id.0;
    // #5061: failed-before-cold, matching `search_handler`'s precedence — an id
    // can sit in BOTH sets (nothing clears `failed_entries`, and re-registering
    // re-adds to `entries`).
    if cold_store.is_failed(index_id) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "index_restore_failed",
                "index_id": id,
                "retryable": false,
                "message": format!(
                    "index '{id}' previously failed to restore (blocked volume or missing \
                     root_path) — restart the daemon or re-register to retry"
                ),
            })),
        );
    }
    if cold_store.contains(index_id) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "index_not_resident",
                "index_id": id,
                "retryable": true,
                // #5061: this endpoint cannot reload the index itself; `search`
                // can, so name it rather than leaving the caller to poll a 503
                // that nothing else will clear.
                "restore_via": format!("POST /indexes/{id}/search"),
                "message": format!(
                    "index '{id}' is registered and built but is not currently resident \
                     (cold-parked by TRUSTY_MAX_RESIDENT_INDEXES). It was NOT lost: a \
                     search against it reloads it, after which this endpoint answers \
                     normally."
                ),
            })),
        );
    }
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("unknown index: {id}"),
            "index_id": id,
        })),
    )
}

/// Verdict for a search that PINNED the semantic lane on an index whose vector
/// lane cannot answer it (#5068).
///
/// Why: `stage: semantic` against a `skip_vector` index returned `HTTP 200`
/// carrying BM25 rows. The caller asked for meaning-based retrieval, got
/// literal-text retrieval, and had no field to tell them apart — the only
/// signal was `search_capabilities` omitting `vector`, which reads as an
/// absence rather than an answer. This matters more since #5060 made worktree
/// indexes `skip_vector` by default, so the degraded path is now the common
/// one. `search_kg` already returns `503 kg_unavailable` for a `skip_kg` index;
/// this is that precedent applied to the lane next door.
///
/// What: `None` when the vector lane can serve the query — the caller proceeds.
/// `Some((503, body))` otherwise, with `reason` separating the two states that
/// look identical from outside: `skipped_by_config` is permanent for this
/// index's current configuration (`PATCH /indexes/{id}/config` to change it),
/// while `stage_not_ready` clears on its own once the embed pass finishes.
/// `retryable` carries the same split as a bool so a caller never has to parse
/// the reason string.
///
/// Only call this when the caller EXPLICITLY pinned the semantic stage. An
/// unpinned hybrid query legitimately degrades to whatever lanes are ready —
/// that case is reported additively via `meta.vector_unavailable`, not as an
/// error.
/// Test: `semantic_stage_on_skip_vector_index_is_503_not_degraded_200`,
/// `semantic_stage_before_embed_completes_is_503_retryable`.
pub(super) fn vector_lane_unavailable(
    index_id: &str,
    skip_vector: bool,
    stages: &IndexStages,
) -> Option<(StatusCode, Json<serde_json::Value>)> {
    let semantic_ready = stages.semantic.status == StageStatus::Ready;
    if semantic_ready && !skip_vector {
        return None;
    }
    let (reason, retryable, remedy) = if skip_vector {
        (
            "skipped_by_config",
            false,
            "this index was built with the vector component disabled — enable it with \
             PATCH /indexes/{id}/config {\"skip_vector\": false}, or use the lexical lane",
        )
    } else {
        (
            "stage_not_ready",
            true,
            "the embed pass for this index has not completed — retry once \
             GET /indexes/{id}/status reports \"vector\" in search_capabilities",
        )
    };
    Some((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "vector_unavailable",
            "reason": reason,
            "index_id": index_id,
            "retryable": retryable,
            "stages": stages,
            "message": format!(
                "index '{index_id}' cannot serve a semantic search: its vector lane is \
                 unavailable ({reason}). Returning lexical results here would answer a \
                 different question than the one asked (issue #5068). {remedy}"
            ),
        })),
    ))
}

/// Verdict for an ingest whose contribution is durable but not yet merged into
/// the serving graph (#5505).
///
/// Why: `POST /indexes/{id}/graph` answered `200` with `replaced: true` and
/// post-merge graph totals whenever the merge failed — totals that excluded the
/// contribution just ingested. The caller was told its ingest succeeded while
/// every query against it returned incomplete results. A `200` carrying a
/// `degraded` field would not fix that: existing callers branch on the status,
/// so the failure would stay invisible to exactly the clients the endpoint
/// misled.
///
/// What: `503`, matching the rest of this module's "registered, but cannot
/// serve what you asked" family. `persisted: true` is the load-bearing field —
/// the data is NOT lost, so a caller must not respond by re-deriving it.
///
/// `retryable` is EARNED, not assumed. One unreadable `kg_contrib` row fails
/// the whole load, so when the blocker is a DIFFERENT producer's row, retrying
/// this ingest fails identically forever — telling that caller to retry would
/// send it into an unbounded loop (#5505). So: retryable when no single row is
/// implicated (a table-level fault or a lost worker can clear on the next
/// attempt), or when the blocker is this producer's own row, which a re-send
/// replaces outright. Otherwise `false`, with `blocking_producer` naming the
/// row an operator must re-send or delete.
/// Test: `ingest_reports_503_when_the_contributed_overlay_cannot_be_merged`,
/// `ingest_503_is_not_retryable_when_another_producers_row_is_the_blocker`,
/// `ingest_route_answers_503_over_http_when_the_merge_fails`.
pub(super) fn contrib_not_merged_response(
    index_id: &str,
    producer: &str,
    replaced: bool,
    reason: &str,
    blocking_producer: Option<&str>,
) -> (StatusCode, Json<serde_json::Value>) {
    let retryable = blocking_producer.is_none_or(|blocker| blocker == producer);
    let remedy = match blocking_producer {
        Some(blocker) if blocker != producer => format!(
            "Retrying this ingest will NOT help: the blocker is producer '{blocker}'s stored \
             row, which this ingest never touches. Re-ingest '{blocker}' with a valid \
             document to replace that row."
        ),
        Some(_) => "Re-send this document: ingest is replace-per-producer, so it overwrites \
             the unreadable row."
            .to_string(),
        None => "No single stored row is implicated. Retry the ingest, or reindex the index."
            .to_string(),
    };
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "contrib_not_merged",
            "index_id": index_id,
            "producer": producer,
            "retryable": retryable,
            "persisted": true,
            "replaced": replaced,
            "reason": reason,
            "blocking_producer": blocking_producer,
            "message": format!(
                "producer '{producer}' contribution was stored durably in index \
                 '{index_id}' but could not be merged into the serving graph, so it is \
                 not queryable yet ({reason}). {remedy} The stored contribution is not \
                 lost (issue #5505)."
            ),
        })),
    )
}
