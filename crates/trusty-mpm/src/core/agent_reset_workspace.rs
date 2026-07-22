//! Sweep active session/project workspaces into `tm install --reset-agents`
//! (issue #2508).
//!
//! Why: [`super::agent_reset::reset_agents`] only ever targeted the USER-LEVEL
//! `~/.claude/agents/` directory `tm install` writes to. PROJECT-LEVEL
//! `.claude/agents/` directories — deployed by [`super::session_launch`] into
//! every managed session's workspace/worktree — carry their OWN independently
//! stale composition and never receive the fix `--reset-agents` applies to the
//! user-level copy. #2508's observed symptom: a user-level reset verified
//! clean while the active session worktree's `.claude/agents/` still had zero
//! files containing the new BASE-AGENT sections, reproducing the #2501 parking
//! failure the reset was meant to close. This module is the sweep that
//! reconciles every INTACT (non-decommissioned) session's workspace alongside
//! the user-level reset, using the session's OWN resolved harness plan so an
//! agent one project's manifest excludes is never resurrected there (the
//! #2462 cross-warning) even while a sibling project's copy IS refreshed.
//!
//! CRITICAL SAFETY GATE (#1511 incident class): NOT every session's
//! `workspace_path` is a directory trusty-mpm provisioned. A local-path/
//! adopted session's `workspace_path` points at the OPERATOR'S REAL, live
//! checkout — the very same directory class `session_manager::decommission`
//! refuses to `remove_dir_all` and `session_manager::search_gc` refuses to
//! drop the search index for. This module reuses that IDENTICAL ownership
//! predicate (`workspace_owned || is_session_worktree(path)`) before ever
//! writing a byte into a workspace's `.claude/agents/` — a sweep must never
//! force-recompose the bundled roster into a real repository the operator
//! did not hand to trusty-mpm to manage.
//! What: [`reset_active_workspace_agents`] loads the on-disk session store
//! (the SAME `sessions.json` the daemon and CLI both read — no live daemon
//! required), filters to sessions with an intact `workspace_path`, SKIPS any
//! session that fails the ownership gate above (recording a
//! [`WorkspaceResetOutcome`] with `skipped_reason` set so the operator sees a
//! deliberate exclusion rather than silent absence), resolves each remaining
//! session's [`crate::core::manifest::HarnessPlan`] exactly as
//! [`super::session_launch::prepare_session`] does, and calls
//! [`super::agent_reset::reset_project_agents`] against that workspace's
//! `.claude/agents/` with the plan's `agent_selected` predicate. Returns one
//! [`WorkspaceResetOutcome`] per considered workspace (reset OR skipped) so
//! the CLI can render a per-session report.
//! Test: `sweep_resets_intact_workspace`, `sweep_skips_decommissioned_session`,
//! `sweep_skips_session_without_workspace`,
//! `sweep_respects_per_workspace_manifest_exclude`,
//! `sweep_skips_unowned_non_worktree_session`.

use std::path::{Path, PathBuf};

use crate::core::agent_builder::AgentBuildError;
use crate::core::agent_reset::{ResetResult, reset_project_agents};
use crate::core::manifest::{HarnessPlan, ManifestSources, resolve_manifest};
use crate::core::paths::FrameworkPaths;
use crate::session_manager::decommission::is_session_worktree;
use crate::session_manager::{ManagedSessionState, SessionStore, StoreError};

/// The outcome of considering one session's project-local agent roster.
///
/// Why: the CLI reports per-session results (which session, which workspace,
/// what changed OR why it was skipped) so an operator sweeping dozens of
/// sessions can see exactly which ones were touched and which were
/// deliberately excluded by the #1511 ownership gate.
/// What: the session's tmux name (human-identifiable), its workspace path,
/// the [`ResetResult`] `reset_project_agents` produced for it (empty/default
/// when skipped), and `skipped_reason` — `None` for a normal reset, `Some`
/// human-readable explanation when the ownership gate rejected this session.
/// Test: `sweep_resets_intact_workspace`,
/// `sweep_skips_unowned_non_worktree_session`.
#[derive(Debug, Clone)]
pub struct WorkspaceResetOutcome {
    /// The session's tmux name (e.g. `tm-quiet-falcon`).
    pub tmux_name: String,
    /// The workspace/worktree directory the reset targeted (or would have).
    pub workspace_path: PathBuf,
    /// The reset result for this workspace's `.claude/agents/`. Default
    /// (all-empty) when `skipped_reason` is `Some`.
    pub result: ResetResult,
    /// `Some(reason)` when the #1511 ownership gate excluded this session
    /// from the reset — the workspace was NOT touched. `None` for a normal
    /// reset attempt.
    pub skipped_reason: Option<String>,
}

/// A failure raised while sweeping session workspaces.
///
/// Why: the sweep touches two independent I/O surfaces (the session store,
/// and each workspace's agent reset); a typed error lets the CLI distinguish
/// "could not even enumerate sessions" from "one workspace's reset failed".
/// What: `Store` wraps a [`StoreError`] loading `sessions.json`; `Reset` wraps
/// an [`AgentBuildError`] from one workspace's [`reset_project_agents`] call,
/// tagged with the tmux name that failed so the operator knows which session
/// to investigate.
/// Test: surfaced indirectly; the happy path is covered by this module's
/// other tests.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceSweepError {
    /// Loading the session store failed.
    #[error("failed to load session store: {0}")]
    Store(#[from] StoreError),
    /// Resetting one workspace's agents failed.
    #[error("agent reset failed for session {tmux_name}: {source}")]
    Reset {
        /// The tmux name of the session whose reset failed.
        tmux_name: String,
        /// The underlying reset error.
        #[source]
        source: AgentBuildError,
    },
}

/// Reset the project-local agent roster of every intact session workspace.
///
/// Why: this is the #2508 fix's sweep entry point — `tm install --reset-agents
/// --reset-agents-workspaces` calls it AFTER the normal user-level reset so a
/// single invocation reconciles both destinations with the same
/// adoption/backup semantics, each gated by its own project's manifest.
/// What: loads the session store at `<fw_root>/session-manager/sessions.json`,
/// keeps sessions whose `state` is not [`ManagedSessionState::Decommissioned`]
/// and whose `workspace_path` still exists on disk (a decommissioned or
/// never-provisioned session has no `.claude/agents/` to reconcile), and for
/// each one: resolves the harness manifest via [`ManifestSources::resolve`]
/// (project override > user config > catalog > default — identical precedence
/// to a real launch), builds the [`HarnessPlan`] via
/// [`FrameworkPaths::for_managed_project`] (agent SOURCE stays at `fw_root`;
/// only the deploy TARGET moves to the workspace's `.claude/agents/`), and
/// calls [`reset_project_agents`] with `names` and the plan's `agent_selected`
/// predicate. Returns one [`WorkspaceResetOutcome`] per swept workspace, in
/// session-store iteration order.
/// Test: `sweep_resets_intact_workspace`, `sweep_skips_decommissioned_session`,
/// `sweep_skips_session_without_workspace`,
/// `sweep_respects_per_workspace_manifest_exclude`,
/// `sweep_skips_unowned_non_worktree_session`.
pub async fn reset_active_workspace_agents(
    fw_root: &Path,
    names: Option<&[String]>,
) -> Result<Vec<WorkspaceResetOutcome>, WorkspaceSweepError> {
    let data_dir = fw_root.join("session-manager");
    let mut store = SessionStore::load(&data_dir).await?;
    let sessions = store.all().await?;

    let catalog_root = crate::content::catalog_root_for(fw_root);
    let mut outcomes = Vec::new();

    for session in sessions {
        if session.state == ManagedSessionState::Decommissioned {
            continue;
        }
        let Some(workspace_path) = &session.workspace_path else {
            continue;
        };
        if !workspace_path.is_dir() {
            continue;
        }

        // CRITICAL (#1511 incident class): only ever write into a workspace
        // trusty-mpm itself provisioned — a clone it owns, or an in-project
        // `.worktrees/<id>` worktree it created. A local-path/adopted
        // session's `workspace_path` is the operator's REAL, long-lived
        // checkout; force-recomposing the bundled roster into it would
        // silently overwrite real project files. Mirrors the identical
        // guard `session_manager::decommission`'s filesystem-removal path
        // and `session_manager::search_gc`'s index-lifecycle path both use.
        if !(session.workspace_owned || is_session_worktree(workspace_path)) {
            outcomes.push(WorkspaceResetOutcome {
                tmux_name: session.tmux_name,
                workspace_path: workspace_path.clone(),
                result: ResetResult::default(),
                skipped_reason: Some(
                    "skipped — not tm-owned (adopted/local-path session; refusing to \
                     force-recompose files into a real checkout)"
                        .to_string(),
                ),
            });
            continue;
        }

        let sources = ManifestSources::resolve(workspace_path, fw_root, &catalog_root);
        let manifest = resolve_manifest(&sources);
        let fw = FrameworkPaths::for_managed_project(fw_root, workspace_path);
        let plan = HarnessPlan::from_manifest(&manifest, &fw, &catalog_root);

        let result =
            reset_project_agents(&plan.agent_source, &fw.claude_agents_dir(), names, |n| {
                plan.agent_selected(n)
            })
            .map_err(|source| WorkspaceSweepError::Reset {
                tmux_name: session.tmux_name.clone(),
                source,
            })?;

        outcomes.push(WorkspaceResetOutcome {
            tmux_name: session.tmux_name,
            workspace_path: workspace_path.clone(),
            result,
            skipped_reason: None,
        });
    }

    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::record::{ManagedSessionId, SessionRecord};
    use chrono::Utc;
    use std::fs;
    use tempfile::TempDir;

    /// Write a minimal source agent pair (mirrors `agent_reset::tests`).
    fn write_sources(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("base-agent.md"),
            "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBase content.\n",
        )
        .unwrap();
        fs::write(
            dir.join("engineer.md"),
            "---\nname: engineer\nrole: engineer\nextends: base-agent\nmodel: sonnet\n---\n\n# Engineer\n\nEngineer content.\n",
        )
        .unwrap();
    }

    /// Build a minimal `SessionRecord` with no workspace (mirrors
    /// `session_manager::store::tests::make_record`).
    fn bare_session(tmux_name: &str) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: tmux_name.to_string(),
            cwd: PathBuf::from("/tmp"),
            task: "test task".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
            worktree_owner: None,
        }
    }

    /// Build a minimal intact, tm-OWNED `SessionRecord` pointing at
    /// `workspace` (simulates an SM-provisioned clone — passes the #1511
    /// ownership gate). Use [`bare_session`] directly (or set
    /// `workspace_owned: false` on an unqualified path) to build a
    /// gate-rejected fixture instead.
    fn intact_session(tmux_name: &str, workspace: &Path) -> SessionRecord {
        SessionRecord {
            workspace_path: Some(workspace.to_path_buf()),
            state: ManagedSessionState::Active,
            workspace_owned: true,
            ..bare_session(tmux_name)
        }
    }

    /// Build a `.trusty-mpm`-named framework root under a fresh temp dir.
    ///
    /// Why: [`FrameworkPaths::from_root`] special-cases a root literally named
    /// `.trusty-mpm` (round-tripping to the SAME path via its parent); a bare
    /// tempdir name does not round-trip, silently nesting an extra
    /// `.trusty-mpm` segment and breaking the `write_sources`/`agent_source_dir`
    /// path agreement these tests depend on. Mirrors production, where
    /// `fw_root` is always `~/.trusty-mpm`.
    fn fw_root_under(base: &TempDir) -> PathBuf {
        let root = base.path().join(".trusty-mpm");
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Seed `<fw_root>/session-manager/sessions.json` with `sessions` directly
    /// (bypassing the async manager) so tests stay hermetic and fast.
    async fn seed_sessions(fw_root: &Path, sessions: Vec<SessionRecord>) {
        let data_dir = fw_root.join("session-manager");
        fs::create_dir_all(&data_dir).unwrap();
        let mut store = SessionStore::load(&data_dir).await.unwrap();
        for session in sessions {
            store.upsert(session).await.unwrap();
        }
    }

    #[tokio::test]
    async fn sweep_resets_intact_workspace() {
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));
        let workspace = TempDir::new().unwrap();

        seed_sessions(
            &fw_root,
            vec![intact_session("tm-sweep-test", workspace.path())],
        )
        .await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].tmux_name, "tm-sweep-test");
        assert_eq!(outcomes[0].result.recomposed.len(), 2);
        assert!(workspace.path().join(".claude/agents/engineer.md").exists());
    }

    #[tokio::test]
    async fn sweep_skips_decommissioned_session() {
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));
        let workspace = TempDir::new().unwrap();

        let mut session = intact_session("tm-gone", workspace.path());
        session.state = ManagedSessionState::Decommissioned;
        seed_sessions(&fw_root, vec![session]).await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();
        assert!(
            outcomes.is_empty(),
            "a decommissioned session has no workspace to reconcile"
        );
    }

    #[tokio::test]
    async fn sweep_skips_session_without_workspace() {
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));

        let session = bare_session("tm-no-workspace");
        seed_sessions(&fw_root, vec![session]).await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();
        assert!(outcomes.is_empty());
    }

    #[tokio::test]
    async fn sweep_skips_unowned_non_worktree_session() {
        // CRITICAL (#1511 incident class): an adopted-shaped session —
        // `workspace_owned: false`, and a workspace path whose immediate
        // parent is NOT `.worktrees` (so `is_session_worktree` also says
        // no) — is the operator's REAL, long-lived checkout. The sweep must
        // neither touch its `.claude/agents/` contents NOR silently drop it
        // from the report: it must appear with `skipped_reason` set.
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));

        // A "real" checkout: NOT nested under `.worktrees`.
        let real_repo = TempDir::new().unwrap();
        assert_ne!(
            real_repo
                .path()
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some(".worktrees"),
            "fixture must not accidentally look like a session worktree"
        );
        let agents_dir = real_repo.path().join(".claude").join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        let sentinel_path = agents_dir.join("my-custom-agent.md");
        let original_bytes = b"---\nname: my-custom-agent\n---\n\nHand-authored, not bundled.\n";
        fs::write(&sentinel_path, original_bytes).unwrap();

        let mut session = intact_session("tm-adopted-real-repo", real_repo.path());
        session.workspace_owned = false;
        seed_sessions(&fw_root, vec![session]).await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();

        assert_eq!(
            outcomes.len(),
            1,
            "the skipped session must still be reported, not silently absent"
        );
        assert_eq!(outcomes[0].tmux_name, "tm-adopted-real-repo");
        let reason = outcomes[0]
            .skipped_reason
            .as_deref()
            .expect("unowned non-worktree session must carry a skip reason");
        assert!(reason.contains("not tm-owned"), "reason = {reason:?}");
        assert!(
            outcomes[0].result.recomposed.is_empty(),
            "a skipped session must not report any recomposed files"
        );

        // The decisive assertion: the real file's bytes are byte-for-byte
        // untouched — no `engineer.md`/`base-agent.md` were ever written.
        let after_bytes = fs::read(&sentinel_path).unwrap();
        assert_eq!(
            after_bytes, original_bytes,
            "a real, unowned checkout must never be written to"
        );
        assert!(
            !agents_dir.join("engineer.md").exists(),
            "the bundled roster must never be force-recomposed into a real checkout"
        );
        assert!(!agents_dir.join("base-agent.md").exists());
    }

    #[tokio::test]
    async fn sweep_respects_per_workspace_manifest_exclude() {
        // Issue #2462/#2508: a workspace whose PROJECT manifest excludes
        // `engineer` must come back from the sweep WITHOUT engineer.md, and
        // the exclusion must be visible in `deselected`.
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));
        let workspace = TempDir::new().unwrap();
        let project_manifest_dir = workspace.path().join(".trusty-mpm");
        fs::create_dir_all(&project_manifest_dir).unwrap();
        fs::write(
            project_manifest_dir.join("manifest.toml"),
            "[agents]\nexclude = [\"engineer\"]\n",
        )
        .unwrap();

        seed_sessions(
            &fw_root,
            vec![intact_session("tm-excluded", workspace.path())],
        )
        .await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0].result.recomposed,
            vec!["base-agent.md".to_string()]
        );
        assert_eq!(outcomes[0].result.deselected, vec!["engineer".to_string()]);
        assert!(
            !workspace.path().join(".claude/agents/engineer.md").exists(),
            "a project-excluded agent must never land in that project's workspace"
        );
    }
}
