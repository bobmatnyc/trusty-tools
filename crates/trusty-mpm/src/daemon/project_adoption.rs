//! Worktree adoption at project registration, shared by BOTH transports
//! (#7357).
//!
//! Why: registration exists twice — the `project_register` MCP tool
//! ([`crate::daemon::mcp_project::project_register`]) and
//! `POST /api/v1/projects` /
//! `mpm.projects.registry.register`
//! ([`crate::daemon::managed_routes::project_registry_routes::register_project_registry_op`],
//! which is what `tm projects register` calls). #7357's first round added the
//! backfill to the MCP body only, so the CLI path registered a project and
//! adopted nothing — the exact bug, still reachable by the command an operator
//! is most likely to run. One function, called from both bodies, is what makes
//! that class of divergence unreachable rather than merely fixed once.
//! What: [`adopt_pre_existing_worktrees`] resolves the project's local checkout
//! and runs [`crate::project::backfill_checkout`] against the registry data
//! directory under the DAEMON's framework root (never `$HOME`, which an
//! isolated-managed daemon does not use), and [`RegisterProjectResponse`] is the
//! response body both transports return so the report reaches the caller.
//! Test: `parity_register_adopts_the_same_worktrees_across_transports`,
//! `register_response_names_a_skipped_worktree`.

use std::sync::Arc;

use serde::Serialize;

use crate::daemon::state::DaemonState;
use crate::project::{BackfillReport, Project};

/// The body both registration transports return (#7357).
///
/// Why: registration used to answer with the [`Project`] alone, so a caller
/// could not tell a checkout whose worktrees were all adopted from one where
/// every candidate was skipped or held by another project. Registration must
/// still SUCCEED in both cases — the report is the signal, not a failure — so
/// the record is flattened and the report rides beside it under one key. A
/// consumer that only knows `Project` (the `tm` HTTP client does) ignores the
/// extra key.
/// What: the persisted record's own fields, plus `adoption`, plus
/// `inactive_reason` when the record is decommissioned or archived (#11969).
/// Test: `register_response_names_a_skipped_worktree`,
/// `parity_projects_registry_register_agrees_across_transports`,
/// `mcp_project_register_keeps_a_decommissioned_project_and_names_the_way_back`,
/// `http_project_register_keeps_a_decommissioned_project_and_names_the_way_back`,
/// `register_of_an_active_project_omits_the_inactive_reason`.
#[derive(Debug, Serialize)]
pub struct RegisterProjectResponse {
    /// The persisted project record, inlined at the top level.
    #[serde(flatten)]
    pub project: Project,
    /// What registration did with the checkout's pre-existing worktrees.
    pub adoption: BackfillReport,
    /// Why the registered project is still inactive and how to reactivate it
    /// (#11969). Absent, never `null`, for an active project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inactive_reason: Option<String>,
}

impl RegisterProjectResponse {
    /// Build the response both transports return.
    ///
    /// Why: #11969 (Architect ruling, option B) — a register upsert never
    /// reactivates a decommissioned project (#10988); only a session start
    /// does, since it also unparks the index. A caller that re-registers one
    /// must learn that, on either transport.
    /// What: `inactive_reason` is set when `project` is decommissioned or
    /// archived, naming a session start as the way back; else `None`.
    /// Test: `mcp_project_register_keeps_a_decommissioned_project_and_names_the_way_back`,
    /// `http_project_register_keeps_a_decommissioned_project_and_names_the_way_back`,
    /// `register_of_an_active_project_omits_the_inactive_reason`.
    pub fn new(project: Project, adoption: BackfillReport) -> Self {
        let inactive_reason = inactive_reason(&project);
        Self {
            project,
            adoption,
            inactive_reason,
        }
    }
}

/// Why a registered project is still inactive, and how to reactivate it.
///
/// Why: #11969 and #11998 — every register surface (MCP `project_register`,
/// `POST /api/v1/projects`, `POST /projects`) must name the same way back, so
/// one function words it and the transports cannot drift.
/// What: `Some` when `project` is decommissioned or archived, naming a session
/// start as the way back; `None` for an active project.
/// Test: `register_of_an_active_project_omits_the_inactive_reason`,
/// `post_projects_keeps_a_decommissioned_project_and_names_the_way_back_11998`,
/// `post_projects_names_the_reason_only_while_the_project_is_inactive_11998`.
pub fn inactive_reason(project: &Project) -> Option<String> {
    let state = match (project.is_decommissioned(), project.is_archived()) {
        (true, true) => "decommissioned and archived",
        (true, false) => "decommissioned",
        (false, true) => "archived",
        (false, false) => return None,
    };
    Some(format!(
        "project {} is {state}; registering it does not reactivate it — start a \
         session for it (`tm session new`) to reactivate it and reload its \
         search index",
        project.name
    ))
}

/// Adopt the worktrees a project's checkout ALREADY has (#7357).
///
/// Why: before #7357 registration wrote a [`Project`] record and nothing else,
/// so a repo that had agent activity before it was registered stayed invisible
/// to `reconcile-worktrees`, `prune-worktrees --merged-prs`, `tm doctor` and the
/// Disk survey — all four read one scan, and that scan reaches a project only at
/// `<repos_root>/<owner>/<repo>`. The operator's only remaining move was
/// `git worktree remove --force` by hand. Running the backfill here is what
/// makes a pre-existing worktree indistinguishable, to every later pass, from
/// one provisioned after registration.
/// What: resolves the project's LOCAL checkout — `repo_url` when it names an
/// existing directory — and hands it to [`crate::project::backfill_checkout`],
/// which writes one adoption record per worktree it can read. A `repo_url` that
/// is a remote URL needs nothing: its checkout lives at
/// `<repos_root>/<owner>/<repo>`, which the walk already covers, and yields an
/// empty report.
///
/// NOT A GATE. Every failure below is reported, never raised: registration is
/// bookkeeping and must not fail because a checkout moved, a disk is unreadable,
/// or git will not answer. What #7357's review changed is that a skip is no
/// longer invisible — it reaches the caller in
/// [`BackfillReport::unrecorded`].
/// Test: `project_register_backfills_pre_existing_worktrees`,
/// `project_register_backfill_is_idempotent`,
/// `project_register_succeeds_when_the_checkout_is_not_a_repository`,
/// `plan_is_empty_without_a_key_or_a_checkout` (the search hook, #10531),
/// `parity_register_adopts_the_same_worktrees_across_transports`.
pub(crate) fn adopt_pre_existing_worktrees(
    state: &Arc<DaemonState>,
    project: &Project,
) -> BackfillReport {
    // #10531: both transports pass through here, so neither can skip the index.
    // Detached and best effort; registration never waits on the search daemon.
    crate::daemon::search_project_ensure::spawn_ensure_project_index(state, project);
    let checkout = std::path::Path::new(&project.repo_url);
    if !checkout.is_dir() {
        return BackfillReport::default();
    }
    // #8511: exclude the harness files and move legacy markers out of the trees.
    crate::session_manager::worktree_marker_migration::prepare_checkout(checkout);
    let store_dir = crate::project::registry_data_dir_under(state.framework_root());
    let report =
        crate::project::backfill_checkout(&store_dir, &project.name, checkout, chrono::Utc::now());
    if report != BackfillReport::default() {
        tracing::info!(
            project = %project.name,
            checkout = %checkout.display(),
            recorded = report.recorded,
            already_recorded = report.already_recorded,
            claimed_by_another = report.claimed_by_another,
            skipped = report.skipped,
            "#7357: adopted pre-existing worktrees at project registration"
        );
    }
    report
}

/// The daemon's one-shot startup marker migration (#8511).
///
/// Why: registration covers a project registered from now on; this pass covers
/// every project registered before the marker moved, once per daemon start.
/// What: refuses on a scratch framework root (a test process, #6348), then runs
/// [`crate::session_manager::worktree_marker_migration::migrate_registered_projects`]
/// over the orphan-GC's repos root and the adopted anchors on a blocking thread,
/// awaited by a small task that logs the tally or a panic (`JoinError`).
/// Test: the pass itself is `the_fleet_pass_migrates_every_tree_and_excludes_once`.
pub(crate) fn spawn_startup_marker_migration(state: &Arc<DaemonState>) {
    if let Some(reason) = crate::daemon::host_state_refusal(state) {
        tracing::info!("startup marker migration: skipped — {reason}");
        return;
    }
    // #11821: a broken install.toml skips the marker writes (df-D1t).
    let repos_root = match crate::daemon::managed_routes::inproject::try_repos_root() {
        Ok(root) => root,
        Err(e) => {
            tracing::warn!("startup marker migration: skipped — repos root unresolved: {e}");
            return;
        }
    };
    let adopted = crate::project::adopted_anchors_under(state.framework_root());
    let pass = tokio::task::spawn_blocking(move || {
        crate::session_manager::worktree_marker_migration::migrate_registered_projects(
            &repos_root,
            &adopted,
        )
    });
    tokio::spawn(async move {
        match pass.await {
            Ok(tally) => tracing::info!(
                migrated = tally.migrated,
                kept = tally.kept,
                "startup marker migration finished (#8511)"
            ),
            Err(e) => tracing::warn!("startup marker migration did not finish: {e} (#8511)"),
        }
    });
}

#[cfg(test)]
#[path = "project_adoption_tests.rs"]
mod project_adoption_tests;
