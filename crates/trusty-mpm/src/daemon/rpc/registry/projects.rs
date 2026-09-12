//! The project-family RPC methods, and the transport-neutral bodies the four
//! legacy `/projects*` routes could not keep in `api.rs` (#6288 slice 5).
//!
//! Why those four bodies live HERE rather than beside their handlers: `api.rs`
//! sits on a frozen SLOC ratchet budget, so four extra `*_op` functions there
//! would push the file toward a cap it may not cross. The same reasoning put
//! slice 2's core bodies in [`crate::daemon::rpc::core_ops`]. The registry-B
//! bodies did NOT need to move — `managed_routes::project_registry_routes` and
//! `managed_routes::project_status` both had headroom, so their `*_op`
//! functions stayed next to the handlers that share them.
//!
//! What: [`register`] mounts nine methods — the four legacy `/projects*` verbs,
//! the four registry-B `/api/v1/projects*` verbs, and the `/status` rollup.
//! Nothing here names an HTTP type.
//!
//! Test: the `parity_projects_*` and `parity_project_status_*` cases in
//! `super::tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::core::project::ProjectInfo;
use crate::daemon::api::types::{
    DiscoverProjectsResponse, DiscoveredProjectInfo, ProjectsResponse,
};
use crate::daemon::error::DaemonError;
use crate::daemon::managed_routes::ProjectStatusResponse;
use crate::daemon::managed_routes::project_registry_routes as reg;
use crate::daemon::managed_routes::project_status;
use crate::daemon::state::DaemonState;
use crate::project::Project;

use super::NoParams;

/// A project name carried as an RPC parameter.
///
/// Why: `GET /api/v1/projects/{name}` puts the identity key in the path, which
/// a JSON-RPC call has nowhere to put — so it becomes a named field.
/// Test: `parity_projects_registry_get_agrees_across_transports`.
#[derive(Debug, Deserialize)]
pub struct NameParams {
    /// The registry key.
    pub name: String,
}

/// `mpm.projects.registry.patch` parameters: the path key plus the PATCH body.
///
/// Why flattened rather than nested: the body's double-`Option` fields carry
/// "absent means unchanged" semantics that a nesting level would not change but
/// would obscure. Flattening keeps the wire identical to the HTTP body with one
/// extra key.
/// Test: `parity_projects_registry_patch_agrees_across_transports`.
#[derive(Debug, Deserialize)]
pub struct PatchProjectParams {
    /// The registry key, from the HTTP route's path segment.
    pub name: String,
    /// The PATCH body, verbatim.
    #[serde(flatten)]
    pub body: reg::PatchProjectBody,
}

/// `mpm.projects.current` parameters.
///
/// Test: `parity_projects_current_agrees_across_transports`.
#[derive(Debug, Deserialize)]
pub struct PathParams {
    /// Directory whose registered project to resolve, or to announce.
    pub path: PathBuf,
}

/// `POST /projects`, `mpm.projects.register` — announce a working directory.
///
/// #7588: this is registry A — what `tm project init` calls. It used to register
/// the path and nothing else, while registry B (`tm projects register`) and the
/// `project_register` MCP tool both ran #7357's worktree backfill, so an
/// operator who reached for the singular verb got exit 0, no `worktrees.json`,
/// and every pre-existing worktree still invisible to `reconcile-worktrees`,
/// `prune-worktrees --merged-prs`, `tm doctor` and the Disk survey. It now runs
/// the SAME backfill, under the name that already owns the checkout when one
/// does — so whichever surface registers first, both end up attributing the
/// directory the same way rather than each claiming it under its own name.
///
/// NOT A GATE, exactly as #7357 established: the backfill reports failures and
/// never raises them. Registration is bookkeeping and must succeed whatever the
/// filesystem says. The report is dropped here rather than returned because
/// registry A's response type is the wire-stable `ProjectInfo` that
/// `tm project init` and the dashboard both read; the adoption counts reach the
/// log, and the records — which is what the operator was actually missing —
/// reach disk.
///
/// Test: `parity_projects_register_agrees_across_transports`,
/// `register_project_op_disables_git_auto_maintenance`,
/// `register_project_op_backfills_pre_existing_worktrees_7588`,
/// `register_project_op_adopts_under_the_owning_projects_name_7588`.
pub fn register_project_op(state: &Arc<DaemonState>, path: PathBuf) -> ProjectInfo {
    // #7171: an explicit registration is the "operator points trusty-mpm at
    // an existing repo" case `ensure_base_clone`'s equivalent write does not
    // cover — best-effort and non-fatal; `path` need not even be a git repo.
    crate::core::git_maintenance::disable_and_log(&path);
    let info = state.register_project(path.clone());
    // #7588: same store, same framework root, same function as registry B.
    let store_dir = crate::project::registry_data_dir_under(state.framework_root());
    let name = crate::project::project_owning_checkout(&store_dir, &path)
        .unwrap_or_else(|| info.name.clone());
    let report = crate::project::backfill_checkout(&store_dir, &name, &path, chrono::Utc::now());
    if report != crate::project::BackfillReport::default() {
        tracing::info!(
            project = %name,
            checkout = %path.display(),
            recorded = report.recorded,
            already_recorded = report.already_recorded,
            claimed_by_another = report.claimed_by_another,
            skipped = report.skipped,
            "#7588: `tm project init` adopted pre-existing worktrees too"
        );
    }
    info
}

/// `GET /projects`, `mpm.projects.list` — every announced directory.
///
/// Test: `parity_projects_list_agrees_across_transports`.
pub fn list_projects_op(state: &Arc<DaemonState>) -> ProjectsResponse {
    ProjectsResponse {
        projects: state.list_projects(),
    }
}

/// `GET /projects/current`, `mpm.projects.current` — resolve one directory.
///
/// # Errors
///
/// [`DaemonError::SessionNotFound`] when `path` is not a registered project —
/// the variant this route has always used, so its `404` is unchanged.
///
/// Test: `parity_projects_current_agrees_across_transports`,
/// `rpc_projects_current_unregistered_path_is_not_found`.
pub fn current_project_op(
    state: &Arc<DaemonState>,
    path: &Path,
) -> Result<ProjectInfo, DaemonError> {
    state
        .project(path)
        .ok_or_else(|| DaemonError::SessionNotFound {
            id: path.display().to_string(),
        })
}

/// `GET /projects/discover`, `mpm.projects.discover` — the projects Claude Code
/// already knows about.
///
/// Test: `parity_projects_discover_agrees_across_transports`.
pub fn discover_projects_op() -> DiscoverProjectsResponse {
    let projects = crate::core::project_discovery::ProjectDiscovery::discover()
        .into_iter()
        .map(|p| DiscoveredProjectInfo {
            path: p.path.display().to_string(),
            session_count: p.session_count,
            last_session: p.last_session.map(system_time_to_iso8601),
        })
        .collect();
    DiscoverProjectsResponse { projects }
}

/// Render a `SystemTime` as an ISO-8601 / RFC3339 UTC string.
fn system_time_to_iso8601(time: std::time::SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.to_rfc3339()
}

/// Mount the nine project methods.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    let r = router.typed::<NoParams, ProjectsResponse, _, _>("mpm.projects.list", move |_| {
        let s = Arc::clone(&held);
        async move { Ok(list_projects_op(&s)) }
    });

    let held = Arc::clone(state);
    let r = r.typed::<PathParams, ProjectInfo, _, _>("mpm.projects.register", move |p| {
        let s = Arc::clone(&held);
        async move { Ok(register_project_op(&s, p.path)) }
    });

    let held = Arc::clone(state);
    let r = r.typed::<PathParams, ProjectInfo, _, _>("mpm.projects.current", move |p| {
        let s = Arc::clone(&held);
        async move { current_project_op(&s, &p.path).map_err(Into::into) }
    });

    let r = r.typed::<NoParams, DiscoverProjectsResponse, _, _>(
        "mpm.projects.discover",
        move |_| async move { Ok(discover_projects_op()) },
    );

    let held = Arc::clone(state);
    let r = r.typed::<NoParams, reg::ProjectsListResponse, _, _>(
        "mpm.projects.registry.list",
        move |_| {
            let s = Arc::clone(&held);
            async move { reg::list_projects_registry_op(&s).await.map_err(Into::into) }
        },
    );

    let held = Arc::clone(state);
    // #7357: the response carries the worktree-adoption report beside the
    // record, identically on both transports.
    let r = r.typed::<reg::RegisterProjectBody, crate::daemon::project_adoption::RegisterProjectResponse, _, _>(
        "mpm.projects.registry.register",
        move |p| {
            let s = Arc::clone(&held);
            async move {
                reg::register_project_registry_op(&s, p)
                    .await
                    .map_err(Into::into)
            }
        },
    );

    let held = Arc::clone(state);
    let r = r.typed::<NameParams, Project, _, _>("mpm.projects.registry.get", move |p| {
        let s = Arc::clone(&held);
        async move {
            reg::get_project_registry_op(&s, &p.name)
                .await
                .map_err(Into::into)
        }
    });

    let held = Arc::clone(state);
    let r = r.typed::<PatchProjectParams, Project, _, _>("mpm.projects.registry.patch", move |p| {
        let s = Arc::clone(&held);
        async move {
            reg::patch_project_registry_op(&s, &p.name, p.body)
                .await
                .map_err(Into::into)
        }
    });

    let held = Arc::clone(state);
    r.typed::<NameParams, ProjectStatusResponse, _, _>("mpm.projects.status", move |p| {
        let s = Arc::clone(&held);
        async move {
            project_status::project_status_op(&s, &p.name)
                .await
                .map_err(Into::into)
        }
    })
}
