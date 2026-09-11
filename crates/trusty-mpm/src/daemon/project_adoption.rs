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
/// What: the persisted record's own fields, plus `adoption`.
/// Test: `register_response_names_a_skipped_worktree`,
/// `parity_projects_registry_register_agrees_across_transports`.
#[derive(Debug, Serialize)]
pub struct RegisterProjectResponse {
    /// The persisted project record, inlined at the top level.
    #[serde(flatten)]
    pub project: Project,
    /// What registration did with the checkout's pre-existing worktrees.
    pub adoption: BackfillReport,
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
/// `parity_register_adopts_the_same_worktrees_across_transports`.
pub(crate) fn adopt_pre_existing_worktrees(
    state: &Arc<DaemonState>,
    project: &Project,
) -> BackfillReport {
    let checkout = std::path::Path::new(&project.repo_url);
    if !checkout.is_dir() {
        return BackfillReport::default();
    }
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

#[cfg(test)]
#[path = "project_adoption_tests.rs"]
mod project_adoption_tests;
