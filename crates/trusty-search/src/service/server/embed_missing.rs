//! `search.index.embed_missing`: embed only the chunks that have no vector
//! (#10551).
//!
//! Why: after an embedder fault the semantic stage is `Failed` and part of the
//! corpus has no vector; the only recovery was a full reindex. This is the
//! transport-free body of the socket method and MCP tool that resumes the
//! stage instead.
//! What: [`embed_missing_report`] looks the index up, runs
//! `service::embed_missing::resume_semantic` and renders its report or its
//! refusal as the shared `(status, body)` pair. Socket-only, no HTTP twin.
//! Test: `service::embed_missing::tests` drives the core;
//! `embed_missing_over_the_socket_matches_the_core_body` drives this wrapper.

use axum::http::StatusCode;
use serde::Deserialize;
use std::sync::Arc;

use super::state::SearchAppState;
use crate::core::registry::IndexId;
use crate::service::embed_missing::{resume_semantic, ResumeRefusal, ResumeReport};

/// The body of `search.index.embed_missing`.
#[derive(Debug, Default, Deserialize)]
pub struct EmbedMissingRequest {
    /// Report the gap and queue nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// Render a report as the 200 body.
fn report_body(index_id: &str, dry_run: bool, r: &ResumeReport) -> serde_json::Value {
    serde_json::json!({
        "index_id": index_id,
        "dry_run": dry_run,
        "action": r.action.label(),
        "chunks": r.chunks,
        "missing": r.missing,
        "vectors_rejected": r.rejected,
        "semantic": r.semantic,
    })
}

/// Render a refusal as `(status, body)`; the body carries `retryable`.
fn refusal_body(index_id: &str, refusal: ResumeRefusal) -> (StatusCode, serde_json::Value) {
    let (status, code, retryable, why) = match refusal {
        ResumeRefusal::NotApplicable(w) => (
            StatusCode::CONFLICT,
            "embed_missing_not_applicable",
            false,
            w,
        ),
        ResumeRefusal::EmbedderDown(w) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "embed_missing_embedder_unavailable",
            true,
            w,
        ),
        ResumeRefusal::Unreadable(w) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "embed_missing_unreadable",
            true,
            w,
        ),
        ResumeRefusal::Busy(w) => (StatusCode::CONFLICT, "embed_missing_busy", true, w),
    };
    (
        status,
        serde_json::json!({
            "error": code,
            "index_id": index_id,
            "retryable": retryable,
            "message": why,
        }),
    )
}

/// The body `search.index.embed_missing` serves (#10551).
///
/// Why: see the module doc.
/// What: `404` for an unknown index. `409 embed_missing_not_applicable`
/// (`retryable: false`) when the vector lane is off or no embedder is wired,
/// `409 embed_missing_busy` while a reindex holds the index, `503
/// embed_missing_embedder_unavailable` when the embedder fails its probe (the
/// stage is untouched) and `503 embed_missing_unreadable` when coverage cannot
/// be read; the 503s are `retryable: true`. Otherwise `200` with `action`
/// (`queued`, `would_queue`, `already_running`, `noop`, `settled`), `chunks`,
/// `missing`, `vectors_rejected` and the `semantic` status.
/// Test: `embed_missing_over_the_socket_matches_the_core_body`,
/// `embed_missing_on_an_unknown_index_is_a_404`.
pub(crate) async fn embed_missing_report(
    state: &Arc<SearchAppState>,
    id: &str,
    req: Option<EmbedMissingRequest>,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("unknown index '{id}'") }),
        )
    })?;
    let dry_run = req.unwrap_or_default().dry_run;
    match resume_semantic(&handle, dry_run).await {
        Ok(report) => Ok(report_body(id, dry_run, &report)),
        Err(refusal) => Err(refusal_body(id, refusal)),
    }
}
