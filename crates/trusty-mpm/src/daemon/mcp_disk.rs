//! Daemon-side implementation of the `disk_survey` MCP tool (#6927).
//!
//! Why: `StateBackend` (in `mcp_backend.rs`) has to service the Disk
//! dashboard's survey, and inlining its body there would push that file over
//! the 500-SLOC production cap — the same reason `mcp_console.rs` and
//! `mcp_project.rs` exist. This module is also where the tool's real-world
//! inputs are resolved: the operator's workspace root and keep-list, the live
//! session store, the delegation registry, `gh`, and the daemon's shared size
//! index. [`crate::disk::survey_run`] itself resolves none of them, which is
//! what keeps it hermetically testable.
//! What: one free async function, [`disk_survey`], returning DOC-73 §16.5's
//! tree as JSON.
//! Test: `crate::mcp::tests::dispatch_disk_survey_tool` drives the dispatch
//! path against the mock backend; `crate::disk::survey_tests` covers the survey
//! itself over scratch repositories.
//!
//! READ-ONLY. Nothing here removes, prunes, or writes anything — DOC-73 §16.6
//! item 4 owns the clear action, behind an explicit operator confirm.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::core::trusty_tools_config::{self, TrustyToolsConfig};
use crate::daemon::state::DaemonState;
use crate::disk::size_index::DirSize;
use crate::disk::survey_run::{self, DiskProbes, run};
use crate::session_manager::worktree_ownership::AgentWorktreeOwner;
use crate::session_manager::worktree_reclaim::{
    BranchPrState, LiveClaims, PrIndex, WorkspaceClaim, pr_state_for_branch,
};
use crate::session_manager::worktree_registry::ScannedWorktree;
use crate::session_manager::worktree_safety::inspect_dirt;

/// Survey every managed worktree for the console Disk view (#6927).
///
/// Why: see the module docs. The whole pass runs on the blocking pool because
/// every probe under it is a synchronous subprocess — `git status`,
/// `git worktree list`, `gh pr list` — and a byte walk; running that on a
/// runtime worker would stall the daemon's other routes.
/// What: resolves the workspace root and keep-list from the operator's config,
/// snapshots the live session claims, and hands `disk::survey_run::run` a
/// `gh`-backed pull-request probe plus a measurement closure over the daemon's
/// SHARED [`crate::disk::size_index::DirSizeIndex`] — never a second index, so
/// a poll every few seconds costs cache hits rather than walks, and the mutex
/// is held for each measurement alone rather than for the whole pass.
/// `budget_seconds` bounds classification; worktrees past it are listed as
/// `review`, never omitted and never `stale`.
/// Test: `crate::disk::survey_tests`, and `dispatch_disk_survey_tool` for the
/// dispatch wiring.
pub async fn disk_survey(
    state: &Arc<DaemonState>,
    project: Option<&str>,
    budget_seconds: Option<u64>,
) -> Result<Value, String> {
    let config = TrustyToolsConfig::load();
    let repos_root = trusty_tools_config::workspace_root(&config);
    // #6927: read FALLIBLY, separately from the lenient `load` above. A config
    // that will not parse yields a keep-list that keeps everything and says so
    // in `keep_list.error`, rather than an empty one that protects nothing.
    let keep_list = trusty_tools_config::load_disk_keep_list();
    // The claim set is a SNAPSHOT, which is correct here and would not be on a
    // destructive path: this tool only displays, and the delete path re-reads
    // liveness per candidate immediately before each removal (#2919).
    let manager = state.session_manager().await;
    let claims = LiveClaims {
        claims: manager
            .list()
            .await
            .into_iter()
            .filter_map(|r| {
                r.workspace_path
                    .map(|p| WorkspaceClaim::new(r.id.to_string(), p))
            })
            .collect(),
        // The console is not a managed session, so it names none — every claim
        // it sees is foreign, which is the conservative reading.
        caller: None,
    };
    let index = state.disk_size_index();
    let state_for_agents = Arc::clone(state);
    let project = project.map(str::to_string);
    let deadline = budget_seconds.map(|s| Instant::now() + Duration::from_secs(s));

    tokio::task::spawn_blocking(move || {
        let agent_state = |owner: &AgentWorktreeOwner| {
            crate::daemon::services::agent_worktree_reap::delegation_state_for_agent(
                &state_for_agents,
                &owner.agent_id,
            )
        };
        // One `gh` index per repository, not one per worktree — the same
        // amortization `survey_with_index` applies, for the same reason.
        let indexes: RefCell<BTreeMap<PathBuf, PrIndex>> = RefCell::new(BTreeMap::new());
        let pr_state = |scanned: &ScannedWorktree| -> BranchPrState { pr_for(&indexes, scanned) };
        // #6927 review: the lock spans ONE measurement, not the pass. Held
        // across `run` it would also cover every `git status` and `gh pr list`
        // that `classify` shells out to, so a second `disk_survey` — or the
        // #6926 background refresher — would queue behind minutes of network
        // work for an index it only wanted to read.
        let measure = |path: &Path| -> Option<DirSize> {
            let mut index = index.lock();
            survey_run::measure(&mut index, path)
        };
        let probes = DiskProbes {
            pr_state: &pr_state,
            claims: &claims,
            agent_state: &agent_state,
            dirt: &inspect_dirt,
            measure: &measure,
        };
        let survey = run(
            &repos_root,
            &keep_list,
            &probes,
            deadline,
            project.as_deref(),
        );
        serde_json::to_value(survey).map_err(|e| format!("disk_survey: serialize error: {e}"))
    })
    .await
    .map_err(|e| format!("disk_survey: the survey pass panicked: {e}"))?
}

/// The pull-request state of one scanned worktree's branch.
///
/// Why: named rather than inlined so the truncation fallback is stated once.
/// The bulk index cannot reach a branch older than its window, and on this
/// repository that is nearly every worktree the dashboard is about — so an
/// unresolved branch is retried with a targeted lookup, exactly as the reclaim
/// survey does. Without it the view would render `review` for worktrees whose
/// pull requests merged months ago.
fn pr_for(
    indexes: &RefCell<BTreeMap<PathBuf, PrIndex>>,
    scanned: &ScannedWorktree,
) -> BranchPrState {
    let mut cache = indexes.borrow_mut();
    let index = cache
        .entry(scanned.registry_root.clone())
        .or_insert_with(|| PrIndex::from_gh(&scanned.registry_root));
    let pr = index.state_for(scanned.branch.as_deref());
    if matches!(
        pr,
        BranchPrState::Unknown | BranchPrState::LookupFailed { .. }
    ) && !index.is_complete()
        && let Some(branch) = scanned.branch.as_deref()
    {
        return pr_state_for_branch(&scanned.registry_root, branch);
    }
    pr
}
