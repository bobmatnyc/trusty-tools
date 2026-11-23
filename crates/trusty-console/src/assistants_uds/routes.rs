//! `GET /api/console/assistants` and `GET /api/console/assistants/{id}/turns`
//! (#11747).
//!
//! Why: the Trusty Assistants page (#11749) needs the instance list and each
//! instance's history, both read-only facts on disk that `ta` owns.
//! What: two handlers. Both resolve the root through
//! [`AppState::assistants_root`], run their filesystem work on the blocking
//! pool, and answer JSON. They sit in the console router's core, so the
//! per-listener peer gate and the router-wide middleware cover them.
//! Test: `tests/assistants_routes.rs`.

use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use super::turns::{read_tail, turn_log_path};
use super::{InstanceId, assistants_root, list_instances};
use crate::server::AppState;

/// Turns returned when the request names no `limit`.
pub const DEFAULT_TURN_LIMIT: usize = 20;

/// Largest `limit` honoured; a larger one is clamped to it.
pub const MAX_TURN_LIMIT: usize = 200;

/// The query of `GET /api/console/assistants/{id}/turns`.
#[derive(Debug, Deserialize)]
pub struct TurnsQuery {
    /// How many of the newest turns to return; parsed by hand so a bad value
    /// is a JSON `400`, not axum's plain-text rejection.
    limit: Option<String>,
}

/// A JSON error body with `status`.
fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// Run `work` on the blocking pool; a panicked task is a `500`.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Response> + Send + 'static,
) -> Result<T, Response> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(e) => Err(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("assistants read task failed: {e}"),
        )),
    }
}

/// The listed instances under `root`, or a `500` naming the read failure.
fn instances_or_500(root: &std::path::Path) -> Result<Vec<InstanceId>, Response> {
    list_instances(root).map_err(|e| {
        warn!(root = %root.display(), error = %e, "assistants: cannot read the assistants root");
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot read the assistants root: {e}"),
        )
    })
}

/// `GET /api/console/assistants` — the instances `ta list` names.
///
/// What: `{"instances":[{"id":"…"},…]}`, sorted by id. A missing root is an
/// empty list; an unresolvable or unreadable root is a `500`.
/// Test: `list_names_only_instance_homes`, `list_on_a_missing_root_is_empty`
/// (`tests/assistants_routes.rs`).
pub async fn list_handler(State(state): State<AppState>) -> Response {
    let root = match state.assistants_root() {
        Ok(root) => root,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    match blocking(move || instances_or_500(&root)).await {
        Ok(ids) => {
            let instances: Vec<_> = ids.iter().map(|id| json!({ "id": id })).collect();
            Json(json!({ "instances": instances })).into_response()
        }
        Err(resp) => resp,
    }
}

/// `GET /api/console/assistants/{id}/turns?limit=N` — the newest turns.
///
/// What: the id is validated before any filesystem access (`400` naming the
/// rule). An id `ta list` does not name is `404` with `available`. Otherwise
/// `{"instance","order":"oldest_first","limit","total","skipped","turns"}`,
/// where `turns` holds the last `limit` good records in file order (newest
/// last), `total` counts every good record and `skipped` every bad line.
/// `limit` defaults to [`DEFAULT_TURN_LIMIT`], is clamped to
/// [`MAX_TURN_LIMIT`], and a non-integer or `0` is `400`.
/// Test: `turns_returns_the_last_n_oldest_first`, `turns_on_a_missing_log_is_empty`,
/// `turns_counts_a_malformed_line`, `turns_for_an_unknown_id_is_404_with_names`,
/// `turns_refuses_traversal_before_the_filesystem` (`tests/assistants_routes.rs`).
pub async fn turns_handler(
    State(state): State<AppState>,
    AxumPath(raw_id): AxumPath<String>,
    Query(query): Query<TurnsQuery>,
) -> Response {
    // #11747: validate before any filesystem access, so `..` or `/` never
    // reaches a path join.
    let id = match InstanceId::parse(&raw_id) {
        Ok(id) => id,
        Err(reason) => return error(StatusCode::BAD_REQUEST, reason),
    };
    let limit = match parse_limit(query.limit.as_deref()) {
        Ok(limit) => limit,
        Err(reason) => return error(StatusCode::BAD_REQUEST, reason),
    };
    let root = match state.assistants_root() {
        Ok(root) => root,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let work_id = id.clone();
    let read = blocking(move || {
        let available = instances_or_500(&root)?;
        if !available.contains(&work_id) {
            let body = json!({
                "error": format!("unknown assistant instance `{}`", work_id.as_str()),
                "id": work_id,
                "available": available,
            });
            return Err((StatusCode::NOT_FOUND, Json(body)).into_response());
        }
        let log = turn_log_path(&root.join(work_id.as_str()));
        read_tail(&log, limit).map_err(|e| {
            warn!(log = %log.display(), error = %e, "assistants: cannot read the turn log");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot read the turn log: {e}"),
            )
        })
    })
    .await;
    let tail = match read {
        Ok(tail) => tail,
        Err(resp) => return resp,
    };
    if tail.skipped > 0 {
        // #11747: fail-open check — the skip is logged and returned, never silent.
        warn!(
            instance = id.as_str(),
            skipped = tail.skipped,
            "assistants: skipped malformed turn-log lines"
        );
    }
    Json(json!({
        "instance": id,
        "order": "oldest_first",
        "limit": limit,
        "total": tail.total,
        "skipped": tail.skipped,
        "turns": tail.turns,
    }))
    .into_response()
}

/// The effective `limit`: default, clamp, or the reason it is refused.
/// Test: `tests::limit_defaults_clamps_and_refuses`.
pub(crate) fn parse_limit(raw: Option<&str>) -> Result<usize, String> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_TURN_LIMIT);
    };
    match raw.parse::<usize>() {
        Ok(0) => Err("limit must be at least 1".into()),
        Ok(n) => Ok(n.min(MAX_TURN_LIMIT)),
        Err(_) => Err(format!("limit must be a positive integer, got `{raw}`")),
    }
}

impl AppState {
    /// The assistants root this console reads: the override, else `ta`'s rule.
    ///
    /// # Errors
    ///
    /// When no override is set and no home directory is known.
    pub(crate) fn assistants_root(&self) -> Result<PathBuf, String> {
        match &self.assistants_root {
            Some(root) => Ok(root.as_ref().clone()),
            None => assistants_root(),
        }
    }

    /// Read assistant instances under `root` instead of the resolved one.
    ///
    /// Why: `$TA_ASSISTANTS_DIR` and `$HOME` are process-global, and the
    /// console's test binary runs many routers at once — the argument
    /// `AppState::with_analyze_socket` records.
    /// Test: `tests/assistants_routes.rs` sets it on every case.
    #[must_use]
    pub fn with_assistants_root(mut self, root: PathBuf) -> Self {
        self.assistants_root = Some(std::sync::Arc::new(root));
        self
    }
}
