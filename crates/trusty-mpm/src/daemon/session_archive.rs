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
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;
use crate::project::ProjectRegistry;
use crate::session_manager::archive_move::{ArchiveMoveError, MANIFEST_FILE, default_archive_root};
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::session_archive::{
    ArchiveOptions, PreparedArchive, SessionArchiveError, SessionArchiveReport,
};
use crate::session_manager::{ManagedError, SessionManager};

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

/// How long a caller waits for the move before the daemon answers; under the
/// route's 90 s `LIFECYCLE_DEADLINE`, so the answer is this module's (naming
/// the entry), not the deadline layer's bare 504 (#10991).
const ARCHIVE_WAIT: Duration = Duration::from_secs(75);

/// The transport-neutral body of every session-archive entry point.
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] for an id that is not a UUID; otherwise see
/// [`to_daemon_error`] and [`session_archive_op_at`].
/// Test: `session_archive_unknown_session_is_not_found`,
/// `session_archive_rejects_a_bad_id`.
pub async fn session_archive_op(
    state: &Arc<DaemonState>,
    id: &str,
    opts: ArchiveOptions,
) -> Result<SessionArchiveReport, DaemonError> {
    let root = default_archive_root().map_err(|e| DaemonError::Internal(e.to_string()))?;
    session_archive_op_at(state, id, opts, &root, ARCHIVE_WAIT).await
}

/// [`session_archive_op`] with an explicit archive root and caller wait.
///
/// What: checks and plans in the request, then [`run_detached`].
/// Test: `session_archive_unknown_session_is_not_found`.
async fn session_archive_op_at(
    state: &Arc<DaemonState>,
    id: &str,
    opts: ArchiveOptions,
    root: &std::path::Path,
    wait: Duration,
) -> Result<SessionArchiveReport, DaemonError> {
    let id = id
        .parse::<uuid::Uuid>()
        .map(ManagedSessionId::from)
        .map_err(|_| DaemonError::InvalidRequest(format!("`{id}` is not a session id")))?;
    let mgr = state.session_manager().await;
    let registry = state.project_registry().await;
    let prepared = mgr
        .prepare_archive(&registry, &id, opts, root)
        .await
        .map_err(to_daemon_error)?;
    if opts.dry_run {
        return Ok(prepared.into_dry_run_report());
    }
    run_detached(mgr, registry, prepared, wait).await
}

/// Run a prepared archive's finish in its own task and wait `wait` for it.
///
/// Why: #10991 critic MEDIUM — a caller that times out (or disconnects) used
/// to drop the request future after the move but before the project stamp.
/// What: the finish (stop, move, stamp) runs in a spawned task, which
/// completes whatever the caller does. Past `wait`, answers
/// [`DaemonError::ServiceUnavailable`] naming the entry and its manifest.
/// Test: `a_caller_timeout_still_stamps_the_archived_project`.
async fn run_detached(
    mgr: Arc<SessionManager>,
    registry: Arc<ProjectRegistry>,
    prepared: PreparedArchive,
    wait: Duration,
) -> Result<SessionArchiveReport, DaemonError> {
    let entry = prepared.plan.entry.clone();
    let task = tokio::spawn(async move { mgr.finish_archive(&registry, prepared).await });
    match tokio::time::timeout(wait, task).await {
        Ok(Ok(done)) => done.map_err(to_daemon_error),
        Ok(Err(join)) => Err(DaemonError::Internal(format!(
            "archive task for {} failed: {join}",
            entry.display()
        ))),
        Err(_) => Err(DaemonError::ServiceUnavailable(format!(
            "archive to {} is still running in the daemon and will complete, \
             project stamp included; its manifest will be {}",
            entry.display(),
            entry.join(MANIFEST_FILE).display()
        ))),
    }
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

    /// #10991 critic MEDIUM: a caller whose wait runs out gets an answer
    /// naming the entry, and the move still finishes and stamps the project.
    /// A held registry lock pins the finish between move and stamp, so the
    /// timeout lands in exactly the window the critic named.
    #[tokio::test]
    async fn a_caller_timeout_still_stamps_the_archived_project() {
        let state = isolated_state().await;
        let scratch = crate::test_support::hermetic_temp_dir();
        let project_dir = scratch.path().join("widget");
        std::fs::create_dir_all(&project_dir).expect("mkdir");
        let archive_root = scratch.path().join("archive");
        let repo = "https://github.com/acme/widget";
        let registry = state.project_registry().await;
        let project: crate::project::Project = serde_json::from_value(serde_json::json!({
            "name": "widget", "repo_url": repo, "default_branch": "main",
        }))
        .expect("project");
        registry.register(project).await.expect("register");
        let id = ManagedSessionId::new();
        let mut record: crate::session_manager::SessionRecord =
            serde_json::from_value(serde_json::json!({
                "id": id.to_string(), "task": "t", "tmux_name": "tm-w",
                "cwd": project_dir, "state": "stopped",
                "created_at": "2026-11-11T00:00:00Z",
            }))
            .expect("record");
        record.repo_url = Some(repo.into());
        let mgr = state.session_manager().await;
        mgr.store.write().await.upsert(record).await.expect("seed");

        let prepared = mgr
            .prepare_archive(&registry, &id, ArchiveOptions::default(), &archive_root)
            .await
            .expect("prepare");
        // Holding the registry's write lock parks the finish at the stamp, so
        // the caller's wait runs out after the move and before the stamp.
        let guard = registry.hold_write_lock().await;
        let err = run_detached(
            Arc::clone(&mgr),
            Arc::clone(&registry),
            prepared,
            Duration::ZERO,
        )
        .await
        .expect_err("the stamp is blocked, so the wait runs out");
        let msg = err.to_string();
        assert!(matches!(err, DaemonError::ServiceUnavailable(_)), "{msg}");
        assert!(
            msg.contains(&*archive_root.to_string_lossy()),
            "names no entry: {msg}"
        );
        assert!(msg.contains(MANIFEST_FILE), "{msg}");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while project_dir.exists() {
            assert!(std::time::Instant::now() < deadline, "never moved");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(guard);
        while !registry.get("widget").await.expect("row").is_archived() {
            assert!(
                std::time::Instant::now() < deadline,
                "the move finished but the project was never stamped"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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
