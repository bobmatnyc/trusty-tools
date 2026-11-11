//! Daemon entry points for `tm session archive` (#10991).
//!
//! Why: the MCP tool, the socket method and the HTTP route run one body, so
//! the transports cannot disagree on what archive does or refuses.
//! What: [`session_archive_op`] maps
//! [`SessionManager::archive_session`](crate::session_manager::SessionManager::archive_session)
//! onto [`DaemonError`] with the archive root `~/.trusty-tools/archive`;
//! [`session_archive`] is the MCP wrapper, [`session_archive_route`] the HTTP
//! handler, [`register`] the socket method `mpm.managed.archive`.
//! Test: the `tests` module below; `dispatch_session_archive_tool` in
//! `crate::mcp::tests`; `every_route_names_a_served_method`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedError;
use crate::session_manager::archive_move::{ArchiveMoveError, default_archive_root};
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::session_archive::{
    ArchiveOptions, SessionArchiveError, SessionArchiveReport,
};

/// Map an archive failure to its daemon error class.
///
/// What: an unknown session is 404; a refusal the caller can act on (running,
/// Architect, failed stop, a source archive must not move, an existing entry)
/// is 422 with the full message; everything else is 500, still carrying the
/// message, which names any archive entry already written.
/// Test: `archive_error_classes`.
fn to_daemon_error(e: SessionArchiveError) -> DaemonError {
    match e {
        SessionArchiveError::Session(ManagedError::SessionNotFound(id)) => {
            DaemonError::NotFound(format!("session {id} not found"))
        }
        refused @ (SessionArchiveError::Protected(_)
        | SessionArchiveError::Running { .. }
        | SessionArchiveError::StopFailed { .. }
        | SessionArchiveError::Move(
            ArchiveMoveError::NotADirectory(_)
            | ArchiveMoveError::Refused { .. }
            | ArchiveMoveError::EntryExists(_),
        )) => DaemonError::Unprocessable(refused.to_string()),
        other => DaemonError::Internal(other.to_string()),
    }
}

/// The transport-neutral body of every session-archive entry point.
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] for an id that is not a UUID; otherwise see
/// [`to_daemon_error`].
/// Test: `session_archive_unknown_session_is_not_found`,
/// `session_archive_rejects_a_bad_id`.
pub async fn session_archive_op(
    state: &Arc<DaemonState>,
    id: &str,
    opts: ArchiveOptions,
) -> Result<SessionArchiveReport, DaemonError> {
    let id = id
        .parse::<uuid::Uuid>()
        .map(ManagedSessionId::from)
        .map_err(|_| DaemonError::InvalidRequest(format!("`{id}` is not a session id")))?;
    let root = default_archive_root().map_err(|e| DaemonError::Internal(e.to_string()))?;
    let mgr = state.session_manager().await;
    let registry = state.project_registry().await;
    mgr.archive_session(&registry, &id, opts, &root)
        .await
        .map_err(to_daemon_error)
}

/// `session_archive` MCP tool: [`session_archive_op`] as JSON.
///
/// Test: `session_archive_unknown_session_is_not_found`.
pub async fn session_archive(
    state: &Arc<DaemonState>,
    id: &str,
    opts: ArchiveOptions,
) -> Result<Value, String> {
    let report = session_archive_op(state, id, opts)
        .await
        .map_err(|e| format!("session_archive: {e}"))?;
    serde_json::to_value(report).map_err(|e| e.to_string())
}

/// `POST /api/v1/sessions/managed/{id}/archive` with body `{dry_run, force}`.
///
/// Test: `every_route_names_a_served_method` (socket parity).
pub async fn session_archive_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(opts): Json<ArchiveOptions>,
) -> Response {
    match session_archive_op(&state, &id, opts).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `mpm.managed.archive` parameters: the path id plus the body.
#[derive(Debug, Deserialize)]
struct ArchiveParams {
    id: String,
    #[serde(flatten)]
    opts: ArchiveOptions,
}

/// Mount `mpm.managed.archive` on `router`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    router.typed::<ArchiveParams, SessionArchiveReport, _, _>("mpm.managed.archive", move |p| {
        let s = Arc::clone(&held);
        async move {
            session_archive_op(&s, &p.id, p.opts)
                .await
                .map_err(Into::into)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn isolated_state() -> Arc<DaemonState> {
        let root = tempfile::tempdir().expect("tempdir").keep();
        Arc::new(DaemonState::with_root_isolated_managed(root).await)
    }

    #[tokio::test]
    async fn session_archive_unknown_session_is_not_found() {
        let state = isolated_state().await;
        let id = ManagedSessionId::new().to_string();
        let opts = ArchiveOptions {
            dry_run: true,
            force: false,
        };
        let err = session_archive_op(&state, &id, opts)
            .await
            .expect_err("unknown");
        assert!(matches!(err, DaemonError::NotFound(_)), "{err}");
        let mcp = session_archive(&state, &id, opts).await.expect_err("mcp");
        assert!(mcp.contains("not found"), "{mcp}");
    }

    #[tokio::test]
    async fn session_archive_rejects_a_bad_id() {
        let state = isolated_state().await;
        let err = session_archive_op(&state, "nope", ArchiveOptions::default())
            .await
            .expect_err("bad id");
        assert!(matches!(err, DaemonError::InvalidRequest(_)), "{err}");
    }

    #[test]
    fn archive_error_classes() {
        let running = SessionArchiveError::Running {
            path: "/w".into(),
            running: vec!["a".into()],
        };
        assert!(matches!(
            to_daemon_error(running),
            DaemonError::Unprocessable(m) if m.contains("--force")
        ));
        let moved = SessionArchiveError::Move(ArchiveMoveError::VerifyFailed {
            path: "/w".into(),
            reason: "x".into(),
        });
        assert!(matches!(to_daemon_error(moved), DaemonError::Internal(_)));
    }
}
