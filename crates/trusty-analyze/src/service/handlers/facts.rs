//! Route handlers for the FactStore CRUD endpoints.
//!
//! Why: Extracted from `service/mod.rs` to isolate the knowledge-triple
//! management surface (list, upsert, delete facts) into a focused module.
//!
//! What: Three async handlers over `GET /facts`, `POST /facts`, and
//! `DELETE /facts/{id}`, all off-loading blocking redb I/O to
//! `tokio::task::spawn_blocking` so the async runtime stays responsive.
//!
//! Test: `upsert_then_list_facts_round_trip` in `service/tests.rs`.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    response::Json,
};
use serde::Deserialize;

use crate::core::facts::new_fact;
use crate::service::events::{AnalyzerAppState, AnalyzerEvent, ApiError};

#[derive(Deserialize)]
pub struct FactQueryParams {
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
}

pub async fn list_facts(
    State(state): State<Arc<AnalyzerAppState>>,
    Query(p): Query<FactQueryParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Why: `FactStore::query` opens a synchronous redb read transaction. Even
    // though reads use `begin_read()`, redb serialises read-transaction
    // *acquisition* against any in-flight write commit; calling it directly
    // on the tokio runtime worker thread stalled the executor whenever an
    // `upsert_fact` was mid-commit, producing the ~900ms p99 spike seen in
    // issue #67 while p50 stayed at 0.25ms.
    // What: move the blocking redb call onto the blocking pool via
    // `spawn_blocking` so the async worker stays responsive and concurrent
    // requests don't pile up behind a single slow read.
    // Test: covered by the existing `upsert_then_list_facts_round_trip` (the
    // round-trip still works); the latency improvement is observable under
    // concurrent load (not asserted in unit tests).
    let facts = state.facts.clone();
    let hits = tokio::task::spawn_blocking(move || {
        facts.query(
            p.subject.as_deref(),
            p.predicate.as_deref(),
            p.object.as_deref(),
        )
    })
    .await
    .map_err(|e| ApiError::internal(format!("query facts task panicked: {e}")))?
    .map_err(|e| ApiError::internal(format!("query facts: {e:#}")))?;
    let count = hits.len();
    Ok(Json(serde_json::json!({ "facts": hits, "count": count })))
}

#[derive(Deserialize)]
pub struct UpsertFactRequest {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub index_id: String,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    #[serde(default)]
    pub provenance: Vec<String>,
}

fn default_confidence() -> f32 {
    1.0
}

pub async fn upsert_fact(
    State(state): State<Arc<AnalyzerAppState>>,
    Json(req): Json<UpsertFactRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let subject = req.subject.clone();
    let predicate = req.predicate.clone();
    let mut fact = new_fact(req.subject, req.predicate, req.object, req.index_id);
    fact.confidence = req.confidence.clamp(0.0, 1.0);
    fact.provenance = req.provenance;
    let id = fact.id;
    // Why: redb write transactions block the calling thread for the entire
    // commit fsync. Holding the tokio worker hostage starves every other
    // task on that worker (the same root cause that produced the #67 p99
    // spike for `list_facts`). Pushing the write to the blocking pool keeps
    // the async runtime responsive.
    // What: clone the Arc-backed store, run the upsert under `spawn_blocking`,
    // and re-raise both join errors and store errors as 500s.
    // Test: covered by `upsert_then_list_facts_round_trip`.
    let facts = state.facts.clone();
    tokio::task::spawn_blocking(move || facts.upsert(fact))
        .await
        .map_err(|e| ApiError::internal(format!("upsert fact task panicked: {e}")))?
        .map_err(|e| ApiError::internal(format!("upsert fact: {e:#}")))?;
    state.emit(AnalyzerEvent::FactUpserted { subject, predicate });
    Ok(Json(serde_json::json!({ "id": id, "upserted": true })))
}

pub async fn delete_fact(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Why: same blocking-redb concern as `upsert_fact` — `Database::delete`
    // opens a write transaction and fsyncs on commit. Running it directly
    // on the async runtime worker risked starving other handlers.
    // What: dispatch to the blocking pool via `spawn_blocking`.
    // Test: covered transitively by the facts integration tests.
    let facts = state.facts.clone();
    let removed = tokio::task::spawn_blocking(move || facts.delete(id))
        .await
        .map_err(|e| ApiError::internal(format!("delete fact task panicked: {e}")))?
        .map_err(|e| ApiError::internal(format!("delete fact: {e:#}")))?;
    if removed {
        state.emit(AnalyzerEvent::FactDeleted { id: id.to_string() });
    }
    Ok(Json(serde_json::json!({ "id": id, "removed": removed })))
}
