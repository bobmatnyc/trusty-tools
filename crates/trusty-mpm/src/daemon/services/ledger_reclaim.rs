//! The daemon's ledger reclaim pass, and the one place its probes are bound
//! (#8994 S3a).
//!
//! Why: the issue's reclaim criterion — a merged-PR or gone-branch worktree is
//! removed, and the removal confirmed on disk — over EVERY tree the ledger
//! knows, not only the tm-provisioned trees the merged-PR reclaim walks. The
//! backlog (84 trees in one project) must not be bulk-deleted on the first
//! run, so removal waits for the Architect.
//! What: [`run_and_log`] runs inside the merged-PR reclaim's tick, under the
//! same maintenance permit. It links PRs and finds stale candidates (S2c),
//! plans them ([`plan_reclaim`]), and runs the plan through
//! [`execute_plan`] with the production probes. [`mode_at`] reads
//! `disk.ledger_reclaim_remove` from the trusty-mpm config: `true` removes;
//! absent, `false` or an unreadable config only REPORTS. Removed trees are
//! recorded through the S2c removal recorder, which appends `removed` only
//! after it re-checks the directory is gone. Every decision is logged.
//! Test: `daemon::services::ledger_reclaim::tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{info, warn};

use crate::core::trusty_tools_config::{CRATE_NAME, TrustyToolsConfig, load_disk_keep_list};
use crate::core::worktree_ledger::fold::fold;
use crate::core::worktree_ledger::reclaim_plan::plan_reclaim;
use crate::core::worktree_ledger::reclaim_probe::GitReclaimProbe;
use crate::core::worktree_ledger::stale::link_prs_and_find_stale;
use crate::core::worktree_ledger::stale_probe::GitGhStaleProbe;
use crate::core::worktree_ledger::{LEDGER_FILE, WorktreeLedger};
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_landing_refresh::refresh_landing_refs;
use crate::session_manager::worktree_ledger_reclaim::{
    LedgerReclaimOutcome, LedgerReclaimProbes, execute_plan, production_remover,
};
use crate::session_manager::worktree_liveness::process_holding;
use crate::session_manager::worktree_reclaim::{LiveClaims, ReclaimMode};
use crate::session_manager::worktree_reclaim_budget::reclaim_index;
use crate::session_manager::worktree_reclaim_landed::reclaim_landed_content;
use crate::session_manager::worktree_reclaim_launch::process_launch_dirs;

/// The config key that lets the pass remove (`disk:` section).
pub(crate) const REMOVE_KEY: &str = "disk.ledger_reclaim_remove";

/// The pass's mode from the config under `base`, failing closed to `Report`.
///
/// Why: this flag is the only thing between the backlog and its deletion, so
/// an unreadable config must never read as "remove".
/// What: `Remove` only when the config parses and sets the key `true`; an
/// absent file, an absent key or `false` is `Report`; a read or parse error is
/// `Report` plus the error, for the caller to log.
/// Test: `the_mode_is_report_unless_the_config_says_remove`.
pub(crate) fn mode_at(base: &Path) -> (ReclaimMode, Option<String>) {
    let path = trusty_common::crate_config::crate_config_path_at(base, CRATE_NAME);
    match trusty_common::crate_config::load_at::<TrustyToolsConfig>(&path) {
        Ok(Some(config))
            if config
                .disk
                .as_ref()
                .is_some_and(|d| d.ledger_reclaim_remove) =>
        {
            (ReclaimMode::Remove, None)
        }
        Ok(_) => (ReclaimMode::Report, None),
        Err(e) => (ReclaimMode::Report, Some(e.to_string())),
    }
}

/// [`mode_at`] for the operator's home; no home is `Report`.
fn configured_mode() -> ReclaimMode {
    let Some(home) = dirs::home_dir() else {
        return ReclaimMode::Report;
    };
    let (mode, error) = mode_at(&home);
    if let Some(e) = error {
        warn!("ledger reclaim: config unreadable, so the pass only reports ({REMOVE_KEY}): {e}");
    }
    mode
}

/// One ledger reclaim pass with every production probe bound.
///
/// Why: see the module doc; one assembly of the probes, as in
/// `merged_pr_reclaim::reclaim`.
/// What: the ledger at `<framework root>/worktrees.jsonl` (a relative root is
/// `Err`), then, on the blocking pool: fold, [`link_prs_and_find_stale`],
/// [`plan_reclaim`], [`execute_plan`] in `mode`, and the removal recorder on
/// the removed list. A ledger error or a panicked task is `Err`.
/// Test: `an_empty_ledger_removes_nothing`, `an_unreadable_ledger_runs_no_pass`.
pub(crate) async fn run(
    state: &Arc<DaemonState>,
    mode: ReclaimMode,
) -> Result<LedgerReclaimOutcome, String> {
    let root = state.framework_root().to_path_buf();
    if !root.is_absolute() {
        return Err(format!(
            "no absolute framework root ({}), so there is no ledger",
            root.display()
        ));
    }
    let ledger = WorktreeLedger::at(root.join(LEDGER_FILE));
    let recorder = super::worktree_removal_ledger::recorder_for(state);
    let launched_from: Vec<PathBuf> = process_launch_dirs();
    let mgr = state.session_manager().await.clone();
    let for_agents = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        // `None` keeps every tree; see `merged_pr_reclaim::reclaim`.
        let in_use_now = move || -> Option<LiveClaims> {
            let handle = tokio::runtime::Handle::try_current().ok()?;
            Some(handle.block_on(mgr.workspace_claims(None)))
        };
        let agent_state =
            move |owner: &crate::session_manager::worktree_ownership::AgentWorktreeOwner| {
                super::agent_worktree_reap::delegation_state_for_agent(&for_agents, &owner.agent_id)
            };
        let read = ledger.read().map_err(|e| e.to_string())?;
        let stale =
            link_prs_and_find_stale(&ledger, &fold(&read.events), &GitGhStaleProbe::default())
                .map_err(|e| e.to_string())?;
        let facts = GitReclaimProbe::default();
        let plan = plan_reclaim(&stale, &facts);
        let probes = LedgerReclaimProbes {
            in_use_now: &in_use_now,
            index_for: &reclaim_index,
            agent_state: &agent_state,
            keep_list: &load_disk_keep_list,
            launched_from: &launched_from,
            cwd_holder: &process_holding,
            landed: Some(&reclaim_landed_content),
            refresh: &refresh_landing_refs,
            facts: &facts,
            remove: &production_remover,
        };
        let outcome = execute_plan(&plan, mode, &probes);
        // #8994 S3a: `removed` reaches the ledger only through the S2c re-check.
        if !outcome.removed.is_empty() {
            recorder("ledger reclaim", &outcome.removed);
        }
        Ok(outcome)
    })
    .await
    .map_err(|e| format!("the ledger reclaim task failed: {e}"))?
}

/// Log one pass: one line per tree, a failure at `warn`. Returns whether any
/// removal failed.
///
/// Test: `a_failed_removal_is_logged_as_a_failure`.
pub(crate) fn log_outcome(mode: ReclaimMode, outcome: &LedgerReclaimOutcome) -> bool {
    for (path, why) in &outcome.would_remove {
        info!(
            path = %path.display(),
            "ledger reclaim: would remove ({why}); report only until {REMOVE_KEY} is true (#8994)"
        );
    }
    for path in &outcome.removed {
        info!(path = %path.display(), "ledger reclaim: removed, gone from disk (#8994)");
    }
    for (path, why) in &outcome.kept {
        info!(path = %path.display(), mode = ?mode, "ledger reclaim: kept — {why} (#8994)");
    }
    for (path, why) in &outcome.failed {
        warn!(
            path = %path.display(),
            "ledger reclaim: removal did not complete, the tree stays live — {why} (#8994)"
        );
    }
    !outcome.failed.is_empty()
}

/// Run one pass in the configured mode and log it; an error is a `warn`.
///
/// Test: the parts are `the_mode_is_report_unless_the_config_says_remove`,
/// `an_unreadable_ledger_runs_no_pass` and
/// `a_failed_removal_is_logged_as_a_failure`.
pub(crate) async fn run_and_log(state: &Arc<DaemonState>) {
    let mode = configured_mode();
    match run(state, mode).await {
        Ok(outcome) => {
            log_outcome(mode, &outcome);
        }
        Err(e) => warn!("ledger reclaim: the pass did not run, nothing removed: {e} (#8994)"),
    }
}

#[cfg(test)]
#[path = "ledger_reclaim_tests.rs"]
mod tests;
