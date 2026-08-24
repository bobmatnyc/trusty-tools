//! Sweep active session/project workspaces during `tm install --reset-agents`
//! (issue #2508), RETRACTING their bundled agents (issue #4409).
//!
//! Why (#2508, historical): [`super::agent_reset::reset_agents`] only ever
//! targeted the USER-LEVEL directory `tm install` writes to. PROJECT-LEVEL
//! `.claude/agents/` directories — which `session_launch` used to deploy into
//! every managed session's workspace/worktree — carried their OWN independently
//! stale composition and never received the fix `--reset-agents` applied to the
//! user-level copy. #2508's observed symptom: a user-level reset verified clean
//! while the active session worktree's `.claude/agents/` still had zero files
//! containing the new BASE-AGENT sections, reproducing the #2501 parking
//! failure the reset was meant to close.
//!
//! WHAT THIS SWEEP DOES SINCE #4409 — it RETRACTS, it does not recompose.
//! Bundled agents now deploy exclusively into the tm-managed `CLAUDE_CONFIG_DIR`
//! tier, and a project-tier copy OUTRANKS that tier in the harness's agent
//! resolution. Force-recomposing the bundled roster back into a workspace —
//! which is exactly what this module did before #4409, and into the workspaces
//! of LIVE sessions — would re-create the shadow the flip exists to remove, and
//! nothing would ever refresh it again. So the sweep is now the operator-driven
//! path that CLEARS a workspace an older binary provisioned, complementing the
//! automatic retraction `prepare_session`/`sync-assets` perform at launch. The
//! per-project harness plan is no longer consulted: a project's `[agents]`
//! selection governs what DEPLOYS, and nothing bundled deploys here any more,
//! so the only scope left to honour is the operator's explicit `--reset-agents
//! <names>` list.
//!
//! CRITICAL SAFETY GATE (#1511 incident class): NOT every session's
//! `workspace_path` is a directory trusty-mpm provisioned. A local-path/
//! adopted session's `workspace_path` points at the OPERATOR'S REAL, live
//! checkout — the very same directory class `session_manager::decommission`
//! refuses to `remove_dir_all` and `session_manager::search_gc` refuses to
//! drop the search index for. This module reuses that IDENTICAL ownership
//! predicate (`workspace_owned || is_session_worktree(path)`) before ever
//! touching a workspace's `.claude/agents/` — a sweep must never modify, and
//! since #4409 must never DELETE from, a real repository the operator did not
//! hand to trusty-mpm to manage. (Retraction is additionally constrained by the
//! ownership manifest: it can only remove files trusty-mpm itself deployed and
//! recorded, so even past the gate it cannot touch an operator's own file.)
//! SECOND SAFETY GATE — LIVENESS (#4204): ownership answers "may we write
//! here?", not "is there still anything here to write to?" The original
//! `workspace_path.is_dir()` filter conflated the two: a worktree whose `.git`
//! and source tree had been stripped still passes `is_dir()`, and on 2026-07-27
//! this sweep recomposed 45 agent files into exactly such a husk
//! (`.base/.worktrees/f443c12d-…`, absent from `git worktree list`). The filter
//! is now [`crate::core::workspace_liveness::workspace_liveness`], which skips
//! ONLY on positive evidence of death and treats an unverifiable observation as
//! live — see that module for why a naive `.join(".git").exists()` would have
//! been wrong in both directions.
//! What: [`reset_active_workspace_agents`] loads the on-disk session store
//! (the SAME `sessions.json` the daemon and CLI both read — no live daemon
//! required), filters to sessions with an intact `workspace_path`, SKIPS any
//! session that fails the ownership gate above (recording a
//! [`WorkspaceResetOutcome`] with `skipped_reason` set so the operator sees a
//! deliberate exclusion rather than silent absence), and calls
//! [`crate::core::agent_deployer::retract_framework_agents_filtered`] against
//! that workspace's `.claude/agents/`, scoped to the operator's requested
//! `names`. Removals land in [`ResetResult::retracted`]. Returns one
//! [`WorkspaceResetOutcome`] per considered workspace (retracted OR skipped) so
//! the CLI can render a per-session report.
//! Test: `sweep_retracts_intact_workspace`, `sweep_skips_decommissioned_session`,
//! `sweep_skips_session_without_workspace`,
//! `sweep_retraction_honors_requested_names`,
//! `sweep_never_removes_a_hand_placed_agent`,
//! `sweep_skips_unowned_non_worktree_session`,
//! `sweep_skips_gutted_worktree_whose_git_was_stripped`,
//! `sweep_serves_live_linked_worktree_with_git_file`.

use std::path::{Path, PathBuf};

use crate::core::agent_builder::AgentBuildError;
use crate::core::agent_deployer::retract_framework_agents_filtered;
use crate::core::agent_reset::ResetResult;
use crate::core::workspace_liveness::{WorkspaceLiveness, workspace_liveness};
use crate::session_manager::decommission::is_session_worktree;
use crate::session_manager::{ManagedSessionState, SessionStore, StoreError};

/// The outcome of considering one session's project-local agent roster.
///
/// Why: the CLI reports per-session results (which session, which workspace,
/// what changed OR why it was skipped) so an operator sweeping dozens of
/// sessions can see exactly which ones were touched and which were
/// deliberately excluded by the #1511 ownership gate.
/// What: the session's tmux name (human-identifiable), its workspace path,
/// the [`ResetResult`] the retraction produced for it (empty/default when
/// skipped — see [`ResetResult::retracted`]), and `skipped_reason` — `None`
/// for a normal sweep, `Some` human-readable explanation when the ownership
/// gate rejected this session.
/// Test: `sweep_retracts_intact_workspace`,
/// `sweep_skips_unowned_non_worktree_session`.
#[derive(Debug, Clone)]
pub struct WorkspaceResetOutcome {
    /// The session's tmux name (e.g. `tm-quiet-falcon`).
    pub tmux_name: String,
    /// The workspace/worktree directory the sweep targeted (or would have).
    pub workspace_path: PathBuf,
    /// The retraction result for this workspace's `.claude/agents/`. Default
    /// (all-empty) when `skipped_reason` is `Some`.
    pub result: ResetResult,
    /// `Some(reason)` when the #1511 ownership gate excluded this session
    /// from the sweep — the workspace was NOT touched. `None` for a normal
    /// sweep attempt.
    pub skipped_reason: Option<String>,
}

/// A failure raised while sweeping session workspaces.
///
/// Why: the sweep touches two independent I/O surfaces (the session store,
/// and each workspace's agent reset); a typed error lets the CLI distinguish
/// "could not even enumerate sessions" from "one workspace's reset failed".
/// What: `Store` wraps a [`StoreError`] loading `sessions.json`; `Reset` wraps
/// an [`AgentBuildError`] from one workspace's
/// [`retract_framework_agents_filtered`] call, tagged with the tmux name that
/// failed so the operator knows which session to investigate.
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

/// RETRACT the project-local bundled agents of every intact session workspace.
///
/// Why: this is the #2508 fix's sweep entry point — `tm install --reset-agents
/// --reset-agents-workspaces` calls it AFTER the normal user-level reset so a
/// single invocation reconciles both destinations. Since #4409 "reconcile a
/// workspace" means REMOVE the bundled copies, not recompose them: bundled
/// agents deploy exclusively into the tm-managed `CLAUDE_CONFIG_DIR` tier, and
/// a project-tier copy OUTRANKS it, so recomposing one back into a live
/// session's workspace would re-create the very shadow the flip removes.
/// What: loads the session store at `<fw_root>/session-manager/sessions.json`,
/// keeps sessions whose `state` is not [`ManagedSessionState::Decommissioned`]
/// and whose `workspace_path` is not positively dead per
/// [`crate::core::workspace_liveness::workspace_liveness`] (#4204 — a
/// decommissioned or never-provisioned session has no `.claude/agents/` to
/// reconcile, and neither does a gutted worktree husk), and for each one calls
/// [`retract_framework_agents_filtered`] against
/// `<workspace>/.claude/agents/`, scoped to `names` when the operator named an
/// agent set. Only manifest-tracked, FRAMEWORK-owned files are removed —
/// hand-placed and user-owned agents survive byte-identical, exactly as on the
/// session-launch retraction path. Returns one [`WorkspaceResetOutcome`] per
/// swept workspace, in session-store iteration order, with the removals
/// reported in [`ResetResult::retracted`].
/// A workspace whose liveness could NOT be determined is served, not skipped,
/// and logged at WARN — see [`crate::core::workspace_liveness`]'s invariant.
/// Test: `sweep_retracts_intact_workspace`, `sweep_skips_decommissioned_session`,
/// `sweep_skips_session_without_workspace`,
/// `sweep_retraction_honors_requested_names`,
/// `sweep_never_removes_a_hand_placed_agent`,
/// `sweep_skips_unowned_non_worktree_session`,
/// `sweep_reports_session_whose_workspace_vanished`.
pub async fn reset_active_workspace_agents(
    fw_root: &Path,
    names: Option<&[String]>,
) -> Result<Vec<WorkspaceResetOutcome>, WorkspaceSweepError> {
    let data_dir = fw_root.join("session-manager");
    let mut store = SessionStore::load(&data_dir).await?;
    let sessions = store.all().await?;

    let mut outcomes = Vec::new();

    for session in sessions {
        if session.state == ManagedSessionState::Decommissioned {
            continue;
        }
        let Some(workspace_path) = &session.workspace_path else {
            continue;
        };
        // CRITICAL (#4204): `is_dir()` alone is NOT a liveness check. A worktree
        // whose `.git` and source tree have been stripped keeps its directory
        // node, so the old gate waved it through and this sweep recomposed 45
        // agent files into the husk (observed 2026-07-27 in
        // `.base/.worktrees/f443c12d-…`). `workspace_liveness` skips ONLY on
        // POSITIVE evidence of death — an unverifiable observation is reported
        // as `Indeterminate` and deliberately still served, because silently
        // declining to reset a live workspace is a far quieter failure than
        // writing into a dead one.
        let liveness = workspace_liveness(workspace_path, session.workspace_owned);
        match &liveness {
            // A DEFINITE observation that the path is gone (or is not a
            // directory) — `workspace_liveness` routes an unreadable path to
            // `Indeterminate`, so this arm can no longer be reached by a mere
            // I/O failure. It is still REPORTED rather than skipped silently
            // (the pre-#4204 behavior): a live session record pointing at a
            // workspace that has vanished is a stale-record anomaly, and the
            // returned outcome is what surfaces it to the operator running
            // `tm install --reset-agents-workspaces`.
            WorkspaceLiveness::Absent => {
                tracing::warn!(
                    workspace = %workspace_path.display(),
                    session = %session.tmux_name,
                    "session workspace is gone — skipping its agent reset"
                );
                outcomes.push(WorkspaceResetOutcome {
                    tmux_name: session.tmux_name,
                    workspace_path: workspace_path.clone(),
                    result: ResetResult::default(),
                    skipped_reason: Some(format!("skipped — {liveness}")),
                });
                continue;
            }
            // The #4204 husk. Unlike `Absent` this IS reported, because a
            // surviving directory that lost its checkout is an anomaly the
            // operator should see (and is exactly what #3764 / PR #4202 detect).
            WorkspaceLiveness::Gutted => {
                outcomes.push(WorkspaceResetOutcome {
                    tmux_name: session.tmux_name,
                    workspace_path: workspace_path.clone(),
                    result: ResetResult::default(),
                    skipped_reason: Some(format!("skipped — {liveness}")),
                });
                continue;
            }
            WorkspaceLiveness::Indeterminate(_) => {
                tracing::warn!(
                    workspace = %workspace_path.display(),
                    liveness = %liveness,
                    "workspace liveness unverifiable — proceeding with the reset \
                     rather than treating an ambiguous observation as death"
                );
            }
            WorkspaceLiveness::Live => {}
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

        // #4409: retract, never recompose. The workspace's `.claude/agents/`
        // is spelled out from `workspace_path` rather than taken from a
        // `FrameworkPaths` field, matching `prepare_session_inner` — this is a
        // workspace operation and must not be reachable at a home tier.
        // No harness plan is resolved: a project's `[agents]` selection decides
        // what DEPLOYS, and nothing bundled deploys here any more, so the only
        // scope that still applies is the operator's explicit `names`.
        let retracted = retract_framework_agents_filtered(
            &workspace_path.join(".claude").join("agents"),
            |stem| names.is_none_or(|requested| requested.iter().any(|n| n == stem)),
        )
        .map_err(|source| WorkspaceSweepError::Reset {
            tmux_name: session.tmux_name.clone(),
            source,
        })?;
        let result = ResetResult {
            retracted: retracted.removed,
            ..ResetResult::default()
        };

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
            terminal_at: None,
            stop_cause: None,
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

    /// Make `dir` look like a LIVE linked git worktree (#4204).
    ///
    /// Why: every workspace this sweep legitimately serves was created by
    /// `GitBackend::worktree_add`, which writes `.git` as a FILE holding a
    /// `gitdir:` pointer — NOT a directory. The fixtures below must model that,
    /// or they would silently prove the opposite of what they claim (and a
    /// `.join(".git").is_dir()`-shaped guard would sail through them).
    /// What: writes a one-line `gitdir:` pointer file, mirroring both real
    /// `git worktree add` output and
    /// `provisioner::workspace::FakeGitBackend::worktree_add`.
    /// Test: used by `sweep_retracts_intact_workspace`,
    /// `sweep_serves_live_linked_worktree_with_git_file`,
    /// `sweep_ignores_the_project_manifest_exclude`.
    fn make_linked_worktree(dir: &Path) {
        fs::write(
            dir.join(".git"),
            "gitdir: /repo/.base/.git/worktrees/session\n",
        )
        .unwrap();
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

    /// Simulate a workspace an OLDER (pre-#4409) binary provisioned: a full,
    /// manifest-tracked bundled roster in `<workspace>/.claude/agents/`.
    ///
    /// Why: that is the exact state this sweep now exists to clear. Building it
    /// through the real deployer (rather than hand-writing files) is what makes
    /// the fixture's ownership manifest genuine, so the retraction under test is
    /// exercised against a real ledger rather than a contrived one.
    fn seed_legacy_workspace_roster(fw_root: &Path, workspace: &Path) {
        crate::core::agent_deployer::deploy_agents(
            &fw_root.join("framework").join("agents"),
            &workspace.join(".claude").join("agents"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn sweep_retracts_intact_workspace() {
        // #4409: the sweep REMOVES the shadowing bundled copies rather than
        // recomposing them. Recomposing (the pre-#4409 behavior) re-created the
        // project-tier shadow — into the workspaces of LIVE sessions — which is
        // precisely what the flip exists to eliminate.
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));
        let workspace = TempDir::new().unwrap();
        make_linked_worktree(workspace.path());
        seed_legacy_workspace_roster(&fw_root, workspace.path());
        assert!(workspace.path().join(".claude/agents/engineer.md").exists());

        seed_sessions(
            &fw_root,
            vec![intact_session("tm-sweep-test", workspace.path())],
        )
        .await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].tmux_name, "tm-sweep-test");
        assert_eq!(outcomes[0].result.retracted.len(), 2);
        assert!(
            outcomes[0].result.recomposed.is_empty(),
            "the sweep must never recompose a bundled agent into a workspace"
        );
        assert!(
            !workspace.path().join(".claude/agents/engineer.md").exists(),
            "the shadowing copy must be gone"
        );
    }

    #[tokio::test]
    async fn sweep_retraction_honors_requested_names() {
        // `--reset-agents <names>` still scopes the sweep: an agent the
        // operator did not name stays put (file AND ledger entry).
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));
        let workspace = TempDir::new().unwrap();
        make_linked_worktree(workspace.path());
        seed_legacy_workspace_roster(&fw_root, workspace.path());

        seed_sessions(
            &fw_root,
            vec![intact_session("tm-sweep-test", workspace.path())],
        )
        .await;

        let names = vec!["engineer".to_string()];
        let outcomes = reset_active_workspace_agents(&fw_root, Some(&names))
            .await
            .unwrap();

        assert_eq!(
            outcomes[0].result.retracted,
            vec!["engineer.md".to_string()]
        );
        assert!(!workspace.path().join(".claude/agents/engineer.md").exists());
        assert!(
            workspace
                .path()
                .join(".claude/agents/base-agent.md")
                .exists(),
            "an agent outside the requested scope must survive"
        );
    }

    #[tokio::test]
    async fn sweep_never_removes_a_hand_placed_agent() {
        // The sweep can only remove what trusty-mpm itself deployed and
        // recorded. An operator's own file is absent from the ledger and is
        // therefore invisible to it — the same guarantee the launch-time
        // retraction gives.
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));
        let workspace = TempDir::new().unwrap();
        make_linked_worktree(workspace.path());
        seed_legacy_workspace_roster(&fw_root, workspace.path());

        let hand_placed = workspace.path().join(".claude/agents/my-own-agent.md");
        let mine = "---\nname: my-own-agent\ndescription: mine\n---\n\nMine.\n";
        fs::write(&hand_placed, mine).unwrap();

        seed_sessions(
            &fw_root,
            vec![intact_session("tm-sweep-test", workspace.path())],
        )
        .await;

        reset_active_workspace_agents(&fw_root, None).await.unwrap();

        assert_eq!(
            fs::read_to_string(&hand_placed).unwrap(),
            mine,
            "a hand-placed agent must survive the sweep byte-identical"
        );
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
    async fn sweep_reports_session_whose_workspace_vanished() {
        // An active record pointing at a path that is DEFINITELY gone. This is
        // reported rather than skipped in silence, so the operator can see the
        // stale record. (Before PR #4209's review fix this arm was also where
        // an unreadable-but-live workspace landed; that case is now
        // `Indeterminate` and is served, per
        // `liveness_unreadable_parent_is_indeterminate_never_dead`.)
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));

        let gone = fw_base.path().join("vanished-workspace");
        assert!(!gone.exists());
        seed_sessions(&fw_root, vec![intact_session("tm-vanished", &gone)]).await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();
        assert_eq!(outcomes.len(), 1, "a vanished workspace must be reported");
        assert_eq!(outcomes[0].tmux_name, "tm-vanished");
        let reason = outcomes[0]
            .skipped_reason
            .as_deref()
            .expect("a vanished workspace must carry a skip reason");
        assert!(reason.contains("no longer exists"), "{reason}");
        assert!(outcomes[0].result.recomposed.is_empty());
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
    async fn sweep_skips_gutted_worktree_whose_git_was_stripped() {
        // ISSUE #4204, THE BUG. Observed 2026-07-27: worktree
        // `.base/.worktrees/f443c12d-…` had no `.git`, no source tree, and was
        // absent from `git worktree list` — yet `is_dir()` returned true, so
        // this sweep wrote 45 files into its `.claude/agents/`. The husk must
        // now be classified dead and left alone, AND reported (an anomaly the
        // operator should see) rather than silently dropped.
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));

        // A gutted worktree: correct `.worktrees/<id>` shape, directory node
        // survives, `.claude/` survives — but `.git` is gone.
        let base = TempDir::new().unwrap();
        let gutted = base
            .path()
            .join(".worktrees")
            .join("f443c12d-2fb6-4ce1-9f70-2e7695306e47");
        let agents_dir = gutted.join(".claude").join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        assert!(
            gutted.is_dir() && !gutted.join(".git").exists(),
            "fixture must be exactly what the old is_dir() gate waved through"
        );

        seed_sessions(&fw_root, vec![intact_session("tm-gutted", &gutted)]).await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();

        assert_eq!(outcomes.len(), 1, "a gutted husk must still be reported");
        assert_eq!(outcomes[0].tmux_name, "tm-gutted");
        let reason = outcomes[0]
            .skipped_reason
            .as_deref()
            .expect("a gutted workspace must carry a skip reason");
        assert!(reason.contains("gutted"), "reason = {reason:?}");
        assert!(outcomes[0].result.recomposed.is_empty());

        // The decisive assertion: not one byte was written into the husk.
        assert!(
            !agents_dir.join("engineer.md").exists(),
            "the sweep must never recompose the roster into a destroyed worktree"
        );
        assert!(!agents_dir.join("base-agent.md").exists());
        assert_eq!(
            fs::read_dir(&agents_dir).unwrap().count(),
            0,
            "the husk's .claude/agents/ must be left completely untouched"
        );
    }

    #[tokio::test]
    async fn sweep_serves_live_linked_worktree_with_git_file() {
        // THE ANTI-OVER-REFUSAL TEST, and it matters more than the one above.
        // In a LINKED worktree — which is what `WorkspaceProvisioner` creates
        // for every managed session — `.git` is a FILE containing a `gitdir:`
        // pointer, not a directory. A guard written as
        // `path.join(".git").is_dir()` would reject EVERY legitimate managed
        // workspace: the exact inverse of #4204. This test fails loudly if the
        // liveness predicate is ever tightened that way.
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));

        let base = TempDir::new().unwrap();
        let live = base
            .path()
            .join(".worktrees")
            .join("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        fs::create_dir_all(&live).unwrap();
        make_linked_worktree(&live);
        assert!(
            live.join(".git").is_file() && !live.join(".git").is_dir(),
            "fixture must model the linked-worktree .git FILE this test exists for"
        );

        seed_legacy_workspace_roster(&fw_root, &live);
        seed_sessions(&fw_root, vec![intact_session("tm-live-worktree", &live)]).await;

        let outcomes = reset_active_workspace_agents(&fw_root, None).await.unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].skipped_reason, None, "must NOT be refused");
        assert_eq!(outcomes[0].result.retracted.len(), 2);
        assert!(
            !live.join(".claude/agents/engineer.md").exists(),
            "a live linked worktree must still be served (and therefore cleaned)"
        );
    }

    #[tokio::test]
    async fn sweep_ignores_the_project_manifest_exclude() {
        // Issue #2462/#2508 made the sweep honour a project's `[agents]
        // exclude` because it was RECOMPOSING into that project's workspace,
        // and resurrecting an agent the project excluded would have been wrong.
        // #4409 inverts the situation: nothing bundled deploys into a workspace
        // any more, so a project's selection governs nothing here — and a
        // shadowing copy of an EXCLUDED agent is, if anything, more urgent to
        // remove, not exempt. This pins that the sweep clears the whole
        // framework-owned set regardless of the project manifest.
        let fw_base = TempDir::new().unwrap();
        let fw_root = fw_root_under(&fw_base);
        write_sources(&fw_root.join("framework").join("agents"));
        let workspace = TempDir::new().unwrap();
        make_linked_worktree(workspace.path());
        seed_legacy_workspace_roster(&fw_root, workspace.path());
        // #4832: the project manifest layer lives in `.trusty-mpm/framework/`.
        let project_manifest_dir = workspace.path().join(".trusty-mpm").join("framework");
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
        assert_eq!(outcomes[0].result.retracted.len(), 2);
        assert!(
            !workspace.path().join(".claude/agents/engineer.md").exists(),
            "an excluded agent's shadowing copy must be retracted, not exempted"
        );
        assert!(
            outcomes[0].result.deselected.is_empty(),
            "no plan is consulted any more, so nothing can be reported deselected"
        );
    }
}
