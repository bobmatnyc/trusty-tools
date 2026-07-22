//! Daemon-side implementation of the two PM pause/resume context MCP tools:
//! `session_context_catchup` and `session_context_pause`.
//!
//! Why: `/tm-session-resume` and `/tm-session-pause` used to shell out to
//! `tm session catchup` + `git log`/`git status` + hand-written snapshot files
//! so the PM could rebuild or save context across a pause. Those tools live in
//! their own file — distinct from `mcp_session.rs` (managed sub-session
//! lifecycle: `session_new`/`session_stop`/…) — because this is a DIFFERENT
//! concept: operations on the CALLING PM session's own project-local snapshot
//! state, not a spawned sub-session. Kept separate to stay under the 500-SLOC
//! production cap given `mcp_session.rs` is already large.
//! What: [`session_context_catchup`] wraps
//! [`trusty_common::catchup::generate_catchup_json`] (never advancing the
//! watermark — a manual peek, same contract as `tm session catchup`) plus
//! [`crate::core::catchup::session_finder::latest_trusty_mpm_snapshot`] for
//! `resolved_snapshot`. [`session_context_pause`] wraps
//! [`trusty_common::catchup::pause::write_pause_snapshot`] and, unless
//! `prune_worktrees` is `false`, the SAME [`crate::session_manager::SessionManager::prune_orphaned_worktrees`]
//! engine `tm session prune-worktrees` / the HTTP `prune-worktrees` route use —
//! called in-process rather than looped back over HTTP.
//! Test: `cargo test -p trusty-mpm daemon::mcp_context` plus the dispatch-level
//! mock tests in `crate::mcp::tests`.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::core::catchup::{CatchupOptions, generate_catchup_json, session_finder};
use crate::daemon::state::DaemonState;

/// Back the `session_context_catchup` MCP tool.
///
/// Why: gives the PM a typed, JSON-native resume digest instead of scraping
/// `tm session catchup`'s rendered markdown + separately shelling out to `git
/// log`/`git status` and reading snapshot files by hand.
/// What: validates `project_dir` exists; when `all_projects` is set, also
/// scans every project in the legacy claude-mpm registry (same discovery `tm
/// session catchup --all-projects` uses), merging every project's
/// sessions/commits/memory into flat arrays. Builds [`CatchupOptions`] from
/// the persisted `MpmConfig` catchup section (git/palace limits + toggles) and
/// calls [`generate_catchup_json`] per project — which NEVER persists a
/// watermark, so `watermark_advanced` in the result is unconditionally
/// `false`. `resolved_snapshot` is resolved against the primary `project_dir`
/// only (not every scanned project), using `session_id` when given.
/// Test: `session_context_catchup_missing_project_dir_errors`,
/// `session_context_catchup_returns_expected_shape`.
pub async fn session_context_catchup(
    project_dir: &str,
    session_id: Option<&str>,
    all_projects: bool,
    full: bool,
) -> Result<Value, String> {
    let primary = PathBuf::from(project_dir);
    if !primary.is_dir() {
        return Err(format!(
            "project_dir does not exist or is not a directory: {project_dir}"
        ));
    }

    let mut project_dirs = vec![primary.clone()];
    if all_projects {
        let registry = crate::core::claude_mpm_registry::default_registry_path();
        match crate::core::claude_mpm_registry::discover_claude_mpm_projects(&registry) {
            Ok(extra) => {
                for p in extra {
                    if !project_dirs.contains(&p) {
                        project_dirs.push(p);
                    }
                }
            }
            Err(e) => {
                // Fail-open: log and continue with just the primary project.
                eprintln!(
                    "session_context_catchup: warning: could not read claude-mpm registry: {e}"
                );
            }
        }
    }

    let config = crate::core::config::MpmConfig::load_default();
    let memory_url = trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable();

    let mut sessions = Vec::new();
    let mut recent_commits = Vec::new();
    let mut recent_memory = Vec::new();

    for dir in &project_dirs {
        let opts = CatchupOptions {
            project_dir: dir.clone(),
            memory_url: memory_url.clone(),
            include_git: config.catchup.include_git,
            include_palace: config.catchup.include_palace,
            git_limit: config.catchup.git_limit,
            drawer_limit: config.catchup.drawer_limit,
            full,
        };
        // Manual catch-up NEVER advances the watermark — only automatic
        // session-start injection does (core/session_launch/mod.rs).
        let digest = generate_catchup_json(&opts).await;
        sessions.extend(digest.sessions);
        recent_commits.extend(digest.recent_commits);
        recent_memory.extend(digest.recent_memory);
    }

    let resolved_snapshot = session_finder::latest_trusty_mpm_snapshot(&primary, session_id)
        .map(|p| p.display().to_string());

    Ok(json!({
        "sessions": sessions,
        "recent_commits": recent_commits,
        "recent_memory": recent_memory,
        "resolved_snapshot": resolved_snapshot,
        // Manual catch-up is a peek — this is always false by construction:
        // there is no code path here that calls `save_catchup_state`.
        "watermark_advanced": false,
    }))
}

/// Back the `session_context_pause` MCP tool.
///
/// Why: replaces the bash snapshot-write in `/tm-session-pause` with an
/// in-process writer that emits the exact section shape the catch-up reader
/// already parses, plus the same in-process worktree-prune engine the HTTP
/// `prune-worktrees` route uses (never a self-loopback HTTP call).
/// What: writes the snapshot + appends the pause log line via
/// [`trusty_common::catchup::pause::write_pause_snapshot`]; when
/// `prune_worktrees` is true (the default), lists active managed-session
/// workspace paths and calls
/// [`crate::session_manager::SessionManager::prune_orphaned_worktrees`] with
/// `dry_run: false` — mirroring
/// [`crate::daemon::managed_routes::prune::prune_worktrees_route`] exactly,
/// just called directly instead of over HTTP. A worktree-prune failure is
/// logged and reported as an empty list rather than failing the whole pause
/// (the snapshot write is the operation that must not silently fail).
/// Test: `session_context_pause_missing_project_dir_errors`,
/// `session_context_pause_requires_summary`,
/// `session_context_pause_writes_snapshot_without_pruning`.
#[allow(clippy::too_many_arguments)]
pub async fn session_context_pause(
    state: &Arc<DaemonState>,
    project_dir: &str,
    session_id: &str,
    summary: &str,
    completed: Vec<String>,
    in_progress: Vec<String>,
    next_steps: Vec<String>,
    tmux_window: Option<&str>,
    prune_worktrees: bool,
) -> Result<Value, String> {
    let project_path = PathBuf::from(project_dir);
    if !project_path.is_dir() {
        return Err(format!(
            "project_dir does not exist or is not a directory: {project_dir}"
        ));
    }
    if summary.trim().is_empty() {
        return Err("`summary` must not be empty".to_string());
    }

    let input = trusty_common::catchup::pause::PauseSnapshotInput {
        session_id,
        summary,
        completed: &completed,
        in_progress: &in_progress,
        next_steps: &next_steps,
        tmux_window,
    };
    let outcome = trusty_common::catchup::pause::write_pause_snapshot(&project_path, &input)
        .map_err(|e| format!("failed to write pause snapshot: {e}"))?;

    let pruned_worktrees: Vec<String> = if prune_worktrees {
        let mgr = state.session_manager().await;
        let records = mgr.list().await;
        let active_workspace_paths: Vec<PathBuf> = records
            .iter()
            .filter_map(|r| r.workspace_path.clone())
            .collect();
        let tt_config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
        let repos_root = crate::core::trusty_tools_config::workspace_root(&tt_config);
        match mgr
            .prune_orphaned_worktrees(&repos_root, &active_workspace_paths, false)
            .await
        {
            Ok(outcome) => outcome
                .removed
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            Err(e) => {
                tracing::warn!("session_context_pause: worktree prune failed: {e}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    Ok(json!({
        "snapshot_path": outcome.snapshot_path.display().to_string(),
        "timestamp": outcome.timestamp.to_rfc3339(),
        "pruned_worktrees": pruned_worktrees,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_context_catchup_missing_project_dir_errors() {
        let err = session_context_catchup("/nonexistent/does/not/exist", None, false, true)
            .await
            .unwrap_err();
        assert!(err.contains("project_dir"), "{err}");
    }

    #[tokio::test]
    async fn session_context_catchup_returns_expected_shape() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .output()
                .unwrap();
        };
        run(&["init"]);
        run(&["config", "user.email", "t@t.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(tmp.path().join("a.txt"), b"a").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);

        let result = session_context_catchup(tmp.path().to_str().unwrap(), None, false, true)
            .await
            .unwrap();
        assert_eq!(result["watermark_advanced"], false);
        assert!(result["sessions"].is_array());
        assert!(result["recent_commits"].is_array());
        assert!(result["recent_memory"].is_array());
        assert!(result["resolved_snapshot"].is_null());
    }

    #[tokio::test]
    async fn session_context_pause_missing_project_dir_errors() {
        let state = DaemonState::shared();
        let err = session_context_pause(
            &state,
            "/nonexistent/does/not/exist",
            "s1",
            "summary",
            vec![],
            vec![],
            vec![],
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.contains("project_dir"), "{err}");
    }

    #[tokio::test]
    async fn session_context_pause_requires_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = DaemonState::shared();
        let err = session_context_pause(
            &state,
            tmp.path().to_str().unwrap(),
            "s1",
            "   ",
            vec![],
            vec![],
            vec![],
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.contains("summary"), "{err}");
    }

    #[tokio::test]
    async fn session_context_pause_writes_snapshot_without_pruning() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = DaemonState::shared();
        let result = session_context_pause(
            &state,
            tmp.path().to_str().unwrap(),
            "s1",
            "Did the thing.",
            vec![],
            vec![],
            vec!["ship it".to_string()],
            None,
            false,
        )
        .await
        .unwrap();
        assert!(result["snapshot_path"].as_str().unwrap().ends_with(".md"));
        assert_eq!(result["pruned_worktrees"], json!([]));

        let sessions =
            crate::core::catchup::session_finder::find_paused_sessions(tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1, "the written snapshot should round-trip");
    }
}
