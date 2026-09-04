//! `POST /indexes/:id/quantize` — the scalar-precision backfill (issue #6822).
//!
//! Why: #6822 flips the `TRUSTY_VECTOR_QUANT` default to `f16`, and that default
//! reaches an index only at CREATION time — every index already on disk keeps
//! its `f32` vectors. `reindex --force` does not help: the store object is built
//! once at warm-boot and a reindex upserts into that same handle, so a forced
//! reindex re-embeds at the OLD precision. This route is the explicit, one-shot
//! conversion an operator runs once per existing index.
//!
//! Why NOT a `reindex --quant` flag (issue #402): a reindex resolves a root and
//! walks a tree, which is the machinery that let #402 hijack another index's
//! corpus and prune it. This route accepts no root, performs no walk and never
//! re-registers a handle — it is addressed by index id through
//! `state.registry.get` alone, and the only thing it rewrites is that index's
//! own vector arena. Two further guards are inherited rather than reinvented:
//! the `ReindexStatus::Running` refusal below (the exact check
//! `service::shutdown_flush` uses to stop a partial in-memory graph being
//! published, #1717 / #3970), and `UsearchStore::save`'s own #1711
//! empty-over-populated and #1717 shrink guards on the durable write.
//!
//! What: [`quantize_handler`] renders one [`crate::core::store::RequantizeReport`]
//! — the same record for a dry run and an applied run, so the confirmation an
//! operator reads is the report of the work.
//! Test: `super::tests_quantize_6822`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::core::registry::IndexId;
use crate::core::store_config::VectorQuant;
use crate::service::reindex::ReindexStatus;

use super::state::SearchAppState;

/// Body of `POST /indexes/{id}/quantize`.
///
/// Why: both fields default so a bare `POST` with no body means "convert to the
/// current default, for real" — the common operator action — while `dry_run`
/// keeps the report-only path one field away.
/// What: `quant` accepts the same spellings as `TRUSTY_VECTOR_QUANT`
/// (`f32`/`none`, `f16`, `i8`); absent means [`VectorQuant::default`].
/// Test: `super::tests_quantize_6822::quantize_defaults_to_the_env_default`.
#[derive(Deserialize, Default)]
pub struct QuantizeRequest {
    #[serde(default)]
    pub quant: Option<String>,
    /// When `true`, report what would change and write nothing.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

pub(super) async fn quantize_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    body: Option<Json<QuantizeRequest>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    quantize_report(&state, &id, body.map(|Json(req)| req))
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

/// The body `POST /indexes/{id}/quantize` serves, without the transport.
///
/// Why split from the axum handler: mirrors `reindex_report` so the route can be
/// exercised directly from tests (and, later, over the socket transport) without
/// standing up a server.
/// What: resolves the index, refuses while a reindex is running, parses the
/// target precision, then delegates to
/// `CodeIndexer::requantize_vectors`. A `None` report means the index has no
/// vector store to convert (BM25-only / `skip_vector`), which is a 409 rather
/// than a success — reporting "done" for an index that was never converted is
/// the silent no-op #6822 exists to remove.
/// Test: `super::tests_quantize_6822`.
pub(crate) async fn quantize_report(
    state: &Arc<SearchAppState>,
    id: &str,
    body: Option<QuantizeRequest>,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    let handle = match state.registry.get(&index_id) {
        Some(h) => h,
        None => {
            let (status, body) =
                super::degraded::residency_miss_response(&state.cold_store, &index_id);
            return Err((status, body.0));
        }
    };

    let req = body.unwrap_or_default();
    let dry_run = req.dry_run.unwrap_or(false);
    let target = match req.quant.as_deref() {
        Some(raw) => VectorQuant::parse_operator_value(raw).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "error": format!(
                        "unrecognised quant {raw:?} — expected one of f32|none, f16, i8"
                    ),
                    "index_id": index_id.0,
                }),
            )
        })?,
        None => VectorQuant::default(),
    };

    // #6822: refuse while a reindex is in flight. Same `reindex_progress` map
    // and same `ReindexStatus::Running` check `shutdown_flush` uses, for the
    // same reason: a reindex redirects the store's snapshot path to a staging
    // file (#3970) and holds a possibly-partial in-memory graph (#1717), so a
    // conversion published now could land on the staging file or publish a
    // partial arena. A dry run reads nothing durable and is allowed through.
    if !dry_run
        && state
            .reindex_progress
            .get(&index_id)
            .is_some_and(|p| p.status.load() == ReindexStatus::Running)
    {
        return Err((
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": "a reindex is in progress for this index — retry the quantize backfill \
                          once it completes (issues #1717, #3970)",
                "index_id": index_id.0,
            }),
        ));
    }

    let indexer = handle.indexer.read().await;
    let report = indexer
        .requantize_vectors(target, dry_run)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({
                    "error": format!("quantize failed: {e}"),
                    "index_id": index_id.0,
                }),
            )
        })?;
    let Some(report) = report else {
        return Err((
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": "this index has no vector store to convert (lexical-only, skip_vector, \
                          or write-quarantined)",
                "index_id": index_id.0,
                "skip_vector": handle.skip_vector,
                "lexical_only": handle.lexical_only,
            }),
        ));
    };

    // Same derivation `index_status_report` uses: prefer the durable corpus
    // count, fall back to the in-memory map, and report `null` rather than a
    // partial-looking number when the corpus failed to open (#4333).
    let chunk_count: Option<usize> = if indexer.corpus_open_failed {
        None
    } else {
        Some(
            indexer
                .corpus_arc()
                .and_then(|c| c.chunk_count().ok())
                .unwrap_or_else(|| indexer.chunk_count()),
        )
    };
    Ok(serde_json::json!({
        "index_id": index_id.0,
        "root_path": handle.root_path,
        "chunk_count": chunk_count,
        "report": report,
    }))
}
