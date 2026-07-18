//! HTTP handlers for `tm sessions sync-assets` (issue #2444).
//!
//! Why: `#2002`'s agent/skill/output-style deployment is one-shot at session
//! launch — a long-lived managed session never re-syncs when the bundled or
//! catalog source changes underneath it. These two routes give the operator (or
//! the PM, via the CLI) a way to force a re-deploy against a LIVE session's
//! workspace without a full relaunch: one per-session, one fleet-wide for
//! `--all`. They live in their own file so `mod.rs` stays under the 500-SLOC
//! production cap, mirroring the `prune.rs`/`reactivate.rs` split precedent.
//! What: [`sync_session_assets_route`] (POST …/managed/{id}/sync-assets) and
//! [`sync_all_session_assets_route`] (POST …/managed/sync-assets) delegate to
//! [`crate::core::session_launch::sync_session_assets`] — the SAME roster+style
//! deployers `prepare_session` calls at launch — via [`sync_one`], run on the
//! blocking pool since the deploy does synchronous filesystem I/O.
//! Test: `sync_route_redeploys_stale_agent`, `sync_route_missing_session_404`,
//! `sync_all_route_skips_provisioning_and_decommissioned` in `super::tests`.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use serde::Serialize;

use super::summary::parse_id;
use crate::core::paths::FrameworkPaths;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedSessionState, SessionRecord};

/// Build the sync-assets sub-router (both the per-session and fleet-wide
/// routes), merged into the main daemon router by `api.rs`.
///
/// Why: keeps both `/api/v1/sessions/managed/{id}/sync-assets` and the
/// literal `/api/v1/sessions/managed/sync-assets` fleet route registered
/// together with their handlers, mirroring the `provision_status::router()`
/// merge precedent — `api.rs` gains one `.merge(...)` call instead of two
/// inline `.route(...)` registrations.
/// What: a bare `Router<Arc<DaemonState>>` with both POST routes; `api.rs`
/// merges it into the shared router (state is injected once, at that merge
/// point).
/// Test: covered end-to-end via `tests/session_manager_mvp.rs`'s router-level
/// sync-assets tests.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new()
        .route(
            "/api/v1/sessions/managed/{id}/sync-assets",
            post(sync_session_assets_route),
        )
        .route(
            "/api/v1/sessions/managed/sync-assets",
            post(sync_all_session_assets_route),
        )
}

/// Response body for both sync-assets routes' per-session result.
///
/// Why: the operator needs to see exactly what changed (or that nothing did,
/// because everything was already current) rather than a bare success flag.
/// What: mirrors [`crate::core::session_launch::SyncAssetsReport`]'s fields
/// plus the session `id` (needed on the fleet-wide response to attribute each
/// result to its session).
/// Test: `sync_route_redeploys_stale_agent`.
#[derive(Debug, Serialize)]
pub struct SyncAssetsResponse {
    /// Managed session id (UUID string).
    pub id: String,
    /// Agent filenames (re)written this run.
    pub agents_deployed: Vec<String>,
    /// Agent filenames left as-is (user-modified / unchanged).
    pub agents_skipped: Vec<String>,
    /// Skill stems (re)written this run.
    pub skills_deployed: Vec<String>,
    /// Skill stems left as-is.
    pub skills_skipped: Vec<String>,
    /// Whether the project-tier output styles were refreshed successfully.
    pub output_style_synced: bool,
}

/// Response body for POST /api/v1/sessions/managed/sync-assets (`--all`).
///
/// Why: a fleet-wide run must report per-session outcomes rather than a single
/// aggregate — some sessions may be skipped (no live workspace to sync) and
/// some may individually fail without aborting the rest of the fleet.
/// What: `synced` (one [`SyncAssetsResponse`] per session actually re-deployed),
/// `skipped` (session ids not in a syncable state — `provisioning`/
/// `decommissioned`), and `errors` (`"<id>: <message>"` for any session whose
/// redeploy failed; every other session still ran).
/// Test: `sync_all_route_skips_provisioning_and_decommissioned`.
#[derive(Debug, Serialize)]
pub struct SyncAllAssetsResponse {
    /// Sessions successfully re-synced.
    pub synced: Vec<SyncAssetsResponse>,
    /// Session ids skipped because their state has no live workspace to sync
    /// (`provisioning` — nothing deployed yet; `decommissioned` — workspace
    /// gone).
    pub skipped: Vec<String>,
    /// `"<id>: <message>"` for any session whose redeploy failed. A failure
    /// here never aborts the rest of the fleet run.
    pub errors: Vec<String>,
}

/// Whether a session's state has a live workspace worth re-syncing.
///
/// Why: shared by both routes below so the per-session and fleet-wide paths
/// can never disagree on which sessions `sync-assets` applies to — the same
/// state set `summary.rs`'s private `probe_staleness_for` gates the
/// `stale_assets` marker on (a `Provisioning` session has not deployed yet,
/// and a `Decommissioned` one has no workspace left).
/// What: `true` for `Active`, `Stopped`, and `Errored`.
/// Test: `sync_all_route_skips_provisioning_and_decommissioned`.
fn syncable(state: &ManagedSessionState) -> bool {
    matches!(
        state,
        ManagedSessionState::Active | ManagedSessionState::Stopped | ManagedSessionState::Errored
    )
}

/// Run [`crate::core::session_launch::sync_session_assets`] for one session on
/// the blocking pool.
///
/// Why: the redeploy does synchronous filesystem I/O (directory walks, file
/// reads/writes for the agent/skill/output-style deployers) — running it
/// directly on the async executor thread would block it. Resolving the
/// workspace-scoped [`FrameworkPaths`] here (rather than in the caller) keeps
/// both route handlers below identical one-liners.
/// What: resolves `record`'s workdir via
/// [`crate::core::session_assets::session_workdir`], builds
/// `FrameworkPaths::for_managed_workspace(workdir)`, and delegates.
/// Test: `sync_route_redeploys_stale_agent`.
async fn sync_one(record: SessionRecord) -> Result<SyncAssetsResponse, String> {
    tokio::task::spawn_blocking(move || {
        let workdir = crate::core::session_assets::session_workdir(&record).to_path_buf();
        let fw = FrameworkPaths::for_managed_workspace(&workdir);
        crate::core::session_launch::sync_session_assets(&fw, &workdir)
            .map(|report| SyncAssetsResponse {
                id: record.id.to_string(),
                agents_deployed: report.agents_deployed,
                agents_skipped: report.agents_skipped,
                skills_deployed: report.skills_deployed,
                skills_skipped: report.skills_skipped,
                output_style_synced: report.output_style_synced,
            })
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("sync-assets task panicked: {e}")))
}

/// POST /api/v1/sessions/managed/{id}/sync-assets — re-deploy one session's
/// assets against the current catalog (issue #2444).
///
/// Why: the concrete fix for a single stale session flagged by `stale_assets`
/// on `tm sessions ls` — re-run the SAME deployers launch uses, without a full
/// relaunch or touching CLAUDE.md/hooks/MCP config.
/// What: looks up the session (404 if unknown), then delegates to [`sync_one`].
/// Applies regardless of the session's `stale_assets` state — running it on an
/// already-current workspace is a safe, idempotent no-op (every deployer here
/// is the SAME manifest-checksum-driven machinery `prepare_session` uses).
/// Test: `sync_route_redeploys_stale_agent`, `sync_route_missing_session_404`.
pub async fn sync_session_assets_route(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    let record = match mgr.get(&id).await {
        Ok(r) => r,
        Err(_) => {
            return (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response();
        }
    };
    match sync_one(record).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// POST /api/v1/sessions/managed/sync-assets — re-deploy EVERY syncable
/// session's assets (`tm sessions sync-assets --all`, issue #2444).
///
/// Why: an operator refreshing a catalog/bundled skill or agent (a `tm
/// install`, or a fresh `tm` binary) wants to push it to every live session in
/// one call rather than iterating ids by hand.
/// What: lists every managed session, skips one whose state has no live
/// workspace ([`syncable`] — `provisioning`/`decommissioned`), and runs
/// [`sync_one`] for the rest SEQUENTIALLY (this is an infrequent,
/// operator-triggered admin action across an independent per-session
/// filesystem tree each — concurrency would only add complexity, not
/// meaningfully reduce latency for a typical fleet size). A single session's
/// redeploy failure is recorded in `errors` and does NOT abort the rest of the
/// fleet run.
/// Test: `sync_all_route_skips_provisioning_and_decommissioned`.
pub async fn sync_all_session_assets_route(
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    let mgr = state.session_manager().await;
    let records = mgr.list().await;

    let mut synced = Vec::new();
    let mut skipped = Vec::new();
    let mut errors = Vec::new();
    for record in records {
        if !syncable(&record.state) {
            skipped.push(record.id.to_string());
            continue;
        }
        let id = record.id.to_string();
        match sync_one(record).await {
            Ok(resp) => synced.push(resp),
            Err(e) => errors.push(format!("{id}: {e}")),
        }
    }

    Json(SyncAllAssetsResponse {
        synced,
        skipped,
        errors,
    })
}
