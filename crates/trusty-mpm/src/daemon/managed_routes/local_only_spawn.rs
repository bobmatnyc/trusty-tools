//! Spawn a managed session in a repository with no `origin` remote (#8934).
//!
//! Why: the owner ruled on 2026-09-30 that a managed session must run in a
//! local-only repository — the Architect's supervisor directory is one by
//! design — and that such a session uses local-only worktrees: worktrees as
//! usual, no push or PR, merges kept local. Before #8934 every no-origin
//! repository fell through to `spawn_managed_local`'s refusal, "managed
//! sessions require a GitHub remote".
//! What: [`SessionSource`] carries what the record and session preparation
//! know about a project, so the launch-on-main and worktree spawn flows serve
//! a GitHub checkout and a local-only repository alike.
//! [`spawn_managed_local_only`] routes a local-only repository root: on the
//! main checkout itself by default, or on a per-session worktree at
//! `<repo>/.worktrees/<name>` cut from the repository root's `HEAD` (the local
//! default branch — there is no `origin/<default>` to fetch) when the launch
//! asked for one. The record carries no `repo_url` and no `source_id`, so no
//! downstream step reads a GitHub identity into it, and the session's gh is
//! disabled by `core::gh_account::resolve_gh_account_env_for_registry`.
//! Test: `a_local_only_repo_spawns_on_its_main_checkout_with_gh_disabled`,
//! `a_local_only_repo_spawns_a_worktree_from_its_local_head`,
//! `a_repo_with_an_origin_is_not_routed_local_only` in
//! `local_only_spawn_tests.rs`.

use std::path::Path;
use std::sync::Arc;

use tracing::info;

use super::lifecycle::SpawnParams;
use crate::core::remote_mode::LOCAL_ONLY_SKIP;
use crate::daemon::state::DaemonState;
use crate::runtime::RuntimeKind;
use crate::session_manager::{ManagedSessionId, SessionRecord};

/// What a managed session's record and preparation know about its project.
///
/// Why: both spawn flows used to take a GitHub `owner`/`repo` pair and build a
/// synthetic `https://github.com/<owner>/<repo>` URL from it; a local-only
/// repository has neither, and a made-up GitHub URL would hand workstream
/// labels and project registration an identity that does not exist.
/// What: `source_id` (`owner/repo`, the reconnect key) and `repo_url` are
/// `None` for a local-only repository; `name` is what session naming falls
/// back to.
/// Test: `session_source_local_only_carries_no_github_identity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionSource {
    /// `owner/repo`, or `None` for a local-only repository.
    pub(super) source_id: Option<String>,
    /// `https://github.com/<owner>/<repo>`, or `None` for a local-only repository.
    pub(super) repo_url: Option<String>,
    /// The repository name, or the local-only directory's name.
    pub(super) name: String,
}

impl SessionSource {
    /// A GitHub checkout of `owner/repo`.
    pub(super) fn github(owner: &str, repo: &str) -> Self {
        Self {
            source_id: Some(format!("{owner}/{repo}")),
            repo_url: Some(format!("https://github.com/{owner}/{repo}")),
            name: repo.to_string(),
        }
    }

    /// A local-only repository rooted at `dir`.
    pub(super) fn local_only(dir: &Path) -> Self {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "local".to_string());
        Self {
            source_id: None,
            repo_url: None,
            name,
        }
    }
}

/// Spawn a managed session in the local-only repository rooted at
/// `local_path` (#8934).
///
/// Why: see the module docs.
/// What: logs the one-line local-only notice, then — with no worktree
/// requested — reconnects to a live Active session already on this exact
/// checkout (unless `force_new`), else runs
/// [`super::launch_on_main::spawn_managed_on_main`] in `local_path` itself;
/// with a worktree requested, keeps `.worktrees/` out of `git status`,
/// reserves the worktree through the same `reserve_inproject_worktree` a
/// GitHub checkout uses (base = `local_path`, start point = its `HEAD`), and
/// runs `spawn_managed_inproject` there. A reservation failure is an error:
/// an explicit worktree request is never quietly placed elsewhere (ADR-0037).
/// Test: see the module docs.
pub(super) async fn spawn_managed_local_only(
    state: &Arc<DaemonState>,
    session_id: &ManagedSessionId,
    params: &SpawnParams,
    runtime: RuntimeKind,
    local_path: &Path,
    config: &crate::core::trusty_tools_config::TrustyToolsConfig,
) -> Result<SessionRecord, String> {
    info!(
        id = %session_id,
        path = %local_path.display(),
        "spawn_managed: {LOCAL_ONLY_SKIP}; worktrees and merges stay local and gh is disabled (#8934)"
    );
    let source = SessionSource::local_only(local_path);
    if !params.worktree {
        if let Some(live) = live_session_on(state, params.force_new, local_path).await {
            return Ok(live);
        }
        return super::launch_on_main::spawn_managed_on_main(
            state, session_id, params, runtime, local_path, &source,
        )
        .await;
    }
    crate::core::worktree_naming::ensure_worktrees_gitignored(local_path)?;
    let (worktree, reserved_name) = super::lifecycle::reserve_inproject_worktree(
        state,
        session_id,
        params,
        local_path,
        local_path,
        &source.name,
        config,
    )
    .await?;
    super::lifecycle::spawn_managed_inproject(
        state,
        session_id,
        params,
        runtime,
        worktree,
        source,
        reserved_name,
    )
    .await
}

/// The live Active session already running in exactly `dir`, when reconnect
/// is wanted — the local-only stand-in for the `source_id` reconnect a GitHub
/// checkout gets (#1707), since a local-only record has no `source_id`.
async fn live_session_on(
    state: &Arc<DaemonState>,
    force_new: bool,
    dir: &Path,
) -> Option<SessionRecord> {
    if force_new {
        return None;
    }
    let mgr = state.session_manager().await;
    let existing = mgr.list().await;
    let tmux = mgr.tmux_driver();
    super::launch_on_main::has_concurrent_main_checkout_session(&existing, dir)
        .filter(|r| tmux.session_exists(&r.tmux_name))
        .cloned()
}

/// Refuse a local directory the surviving in-project path could not serve.
///
/// Why: ADR-0055 (#6000) removed trusty-mpm's own workspace provisioner, and
/// this function was its second call site — it cloned the directory's origin
/// into `<workspace-root>/<owner>/<repo>` and added a worktree there. With that
/// gone there is no second route to try: a local checkout with a GitHub origin
/// is served by `spawn_managed_routed`'s in-project branch, and everything
/// reaching here has already failed that branch's own preconditions.
/// Moved here from `lifecycle.rs` by #8934, which made a repository ROOT with
/// no `origin` a spawnable local-only repository.
/// What: reports which precondition failed — a directory that is not a git
/// repository root (a non-git directory, or a subdirectory of a local-only
/// repository), an origin no
/// GitHub `owner/repo` parses out of, or (the ADR-0055 case) a directory that
/// is inside a repository but is not its root, so it has no `.git` of its own
/// for the in-project path to open.
/// Test: `a_non_root_directory_without_an_origin_is_refused` in tests/local_spawn.rs;
/// the ADR-0055 arm by `a_subdirectory_of_a_repo_is_refused_not_provisioned`
/// in tests/session_new_requires_a_local_path.rs.
pub(super) fn spawn_managed_local(session_id: &ManagedSessionId, params: &SpawnParams) -> String {
    let local_dir = std::path::PathBuf::from(&params.repo_url);

    // #4734: `?` first — a git failure is its own error, not "no remote".
    let origin_url = match super::inproject::get_origin_url(&local_dir) {
        Err(e) => return e,
        Ok(None) => {
            return format!(
                "spawn failed: '{}' is not a git repository root. Pass the directory \
                 holding `.git`; a repository with no origin remote runs local-only \
                 (#8934). Use `tm connect` to run in a directory that is not a repository.",
                local_dir.display()
            );
        }
        Ok(Some(url)) => url,
    };

    if trusty_common::github_path::parse_github_path(&origin_url).is_none() {
        return format!(
            "spawn failed: could not parse a GitHub owner/repo from origin remote \
             '{origin_url}' for '{}'. \
             Use `tm connect` to run in the live checkout instead.",
            local_dir.display()
        );
    }

    // #6000 / ADR-0055: git answered with a remote, so this directory sits
    // INSIDE a repository — but the in-project branch already declined it,
    // which for a directory with a remote means it is not the repository root
    // (no `.git` of its own). trusty-mpm no longer clones a workspace to stand
    // in for one.
    format!(
        "spawn failed for session {session_id}: '{}' resolves to the repository at \
         '{origin_url}' but is not that repository's root, so there is no checkout to run \
         the session in. trusty-mpm no longer provisions one for you (ADR-0055): pass the \
         repository ROOT — the directory holding `.git` — instead.",
        local_dir.display()
    )
}

#[cfg(test)]
#[path = "local_only_spawn_tests.rs"]
mod tests;
