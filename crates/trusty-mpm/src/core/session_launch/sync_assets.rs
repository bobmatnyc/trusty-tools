//! Re-sync a live managed session's deployed assets against the current
//! catalog (issue #2444).
//!
//! Why: [`super::prepare_session_inner`] deploys agents, skills, and output
//! styles exactly ONCE, at launch time (DOC-34/#2002). `tm sessions
//! sync-assets` needs a pure, OFFLINE re-run of that same roster+style deploy
//! step — no catalog git sync (that is `tm catalog sync`'s job,
//! [`crate::content::CatalogSync`]) — targeted at an EXISTING session's
//! workspace, so a mid-session catalog/bundled-asset fix reaches a long-lived
//! session without a full relaunch. This module deliberately reuses the exact
//! deployers `prepare_session_inner` and [`crate::core::update_check::apply_catalog`]
//! already call (`deploy_agents_filtered`, `deploy_all_skill_tiers`,
//! `deploy_output_style`) rather than inventing a third redeploy path.
//! What: [`sync_session_assets`] resolves the harness manifest/plan for
//! `project_dir` exactly as launch does, redeploys the manifest-selected agents
//! (folding in the DOC-42 co-deploy set) and the 3-tier skills, and refreshes
//! the project-tier output styles. It intentionally does NOT touch CLAUDE.md,
//! hooks, or MCP injection — those are session-identity concerns outside
//! asset-catalog sync, and re-running them risks clobbering session-local state
//! (e.g. the trusty-memory palace pin) sync-assets has no business touching.
//! Overwrite semantics match launch: a MANAGED file (recorded in the checksum
//! manifest) whose on-disk content still matches what tm wrote is refreshed; a
//! user-modified or user-owned file is left alone (the deployers' own
//! ownership rule — see `agent_deployer`/`skill_deployer`/`skill_tiers`).
//! Test: `sync_assets_redeploys_new_agent`, `sync_assets_refreshes_stale_skill`,
//! `sync_assets_refreshes_project_output_style`.

use std::path::Path;

use super::settings::deploy_output_style;
use crate::core::agent_deployer::{DeployResult, deploy_agents_filtered, retract_framework_agents};
use crate::core::agent_skill_codeploy::co_deploy_skill_set;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::DeployStats;
use crate::core::skill_tiers::deploy_all_skill_tiers;

/// A failure raised while re-syncing a session's deployed assets.
///
/// Why: the daemon route and the `tm sessions sync-assets` CLI need a typed
/// surface naming which stage failed, mirroring [`crate::core::update_check::apply::ApplyError`]'s
/// shape (minus the `Sync`/`Prune` variants, which do not apply here).
/// What: variants for the agent-deploy and skill-deploy stages.
/// Test: surfaced indirectly — the happy path is covered by this module's
/// `sync_assets_*` tests; deploy-failure mapping is exercised the same way
/// `apply.rs`'s own error variants are (no dedicated failure-injection test,
/// since the underlying deployer error paths are already covered there).
#[derive(Debug, thiserror::Error)]
pub enum SyncAssetsError {
    /// Redeploying agents failed.
    #[error("agent redeploy failed: {0}")]
    AgentDeploy(String),
    /// Redeploying skills failed.
    #[error("skill redeploy failed: {0}")]
    SkillDeploy(String),
}

/// Summary of one [`sync_session_assets`] run.
///
/// Why: the daemon route and CLI both need the deployed/skipped counts to
/// render an honest "N agents refreshed, M skipped (user-modified)" report.
/// What: mirrors [`crate::core::update_check::apply::ApplyReport`]'s
/// agent/skill fields (no `fetched`/prune fields — sync-assets never syncs the
/// catalog checkout or prunes deselected files).
/// Test: `sync_assets_redeploys_new_agent`, `sync_assets_refreshes_stale_skill`.
#[derive(Debug, Default)]
pub struct SyncAssetsReport {
    /// Agent filenames (re)written this run.
    pub agents_deployed: Vec<String>,
    /// Agent filenames left as-is (user-modified / unchanged).
    pub agents_skipped: Vec<String>,
    /// Skill stems (re)written this run.
    pub skills_deployed: Vec<String>,
    /// Skill stems left as-is.
    pub skills_skipped: Vec<String>,
    /// Whether the project-tier output styles were refreshed successfully.
    ///
    /// `false` only on a write failure (disk full, permissions); non-fatal —
    /// the agent/skill sync above still counts as having run.
    pub output_style_synced: bool,
    /// Why the #4409 workspace retraction could not run, if it failed.
    ///
    /// Why: a failed retraction means this workspace's `.claude/agents/` may
    /// still be SHADOWING the canonical config-dir roster — the one condition
    /// an operator running sync-assets needs told about. `prepare_session`
    /// reports the same failure through `PrepReport::roster_errors`; leaving it
    /// as a bare log line here made the two paths disagree about whether the
    /// signal is worth surfacing.
    /// What: `None` on success (including the common "nothing to retract"
    /// no-op); `Some(detail)` when [`retract_framework_agents`] returned `Err`
    /// — e.g. a corrupt ownership ledger, which it refuses to act on.
    /// Test: `sync_assets_reports_retraction_failure_on_corrupt_manifest`.
    pub retraction_error: Option<String>,
    /// What the #4448 shadowing-agent quarantine did, if anything.
    ///
    /// Why: this sweep RENAMES files in the operator's working tree. Reporting
    /// a bare count would be worse than silence — the operator needs the
    /// receipt path to see what moved and how to put it back. `None` on a clean
    /// sweep so a project with nothing to fix reports nothing.
    /// What: [`super::quarantine_shadows::summarize`]'s line on a sweep that
    /// moved or failed on something; a refusal message when the sweep declined
    /// to run at all (corrupt ledger, empty roster).
    /// Test: `sync_assets_quarantines_a_shadowing_workspace_agent`,
    /// `sync_assets_is_silent_when_nothing_shadows`.
    pub quarantine_summary: Option<String>,
}

/// Re-run the roster + output-style deploy against an EXISTING session's
/// workspace, without touching CLAUDE.md, hooks, or MCP injection.
///
/// Why: this is the concrete redeploy `tm sessions sync-assets` offers,
/// answering issue #2444 — a long-lived session's deployed `.claude/{agents,
/// skills}` otherwise never re-syncs once the bundled/catalog source moves.
/// What: resolves the harness manifest + [`crate::core::manifest::HarnessPlan`]
/// for `project_dir` exactly as [`super::prepare_session_inner`] does, then:
/// (1) redeploys agents via [`deploy_agents_filtered`] against
/// `fw.agent_deploy_dir()` — the tm-managed config-dir tier, issue #4409 — and
/// retracts any bundled copy an older binary left in the workspace's own
/// `.claude/agents/`; (2) folds the redeployed agents' declared `skills:`
/// into the skill `select` predicate (DOC-42 co-deploy) and redeploys the
/// 3-tier skill set via [`deploy_all_skill_tiers`]; (3) best-effort refreshes
/// the PROJECT-tier output styles via [`deploy_output_style`] (issue #2125 item
/// 2 — the tier a managed session's `--setting-sources project,local` launch
/// actually reads). `fw` MUST be workspace-scoped
/// (`FrameworkPaths::for_managed_workspace(project_dir)`) so the deploy
/// destinations resolve under `project_dir/.claude/…` rather than the real
/// home; the bundled/catalog SOURCE paths still resolve from the shared
/// framework install regardless.
/// Test: `sync_assets_redeploys_new_agent`, `sync_assets_refreshes_stale_skill`,
/// `sync_assets_refreshes_project_output_style`,
/// `sync_assets_never_touches_hand_placed_agent`,
/// `sync_assets_repairs_drifted_bundled_agent`,
/// `sync_assets_retracts_workspace_bundled_agents`.
pub fn sync_session_assets(
    fw: &FrameworkPaths,
    project_dir: &Path,
) -> Result<SyncAssetsReport, SyncAssetsError> {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let sources =
        crate::core::manifest::ManifestSources::resolve(project_dir, &fw.root, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = crate::core::manifest::HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    // #4409: agents refresh in the tm-managed config-dir tier, never
    // per-workspace, and any bundled copy an older binary left in this
    // workspace's `.claude/agents/` is retracted so it stops shadowing the
    // canonical roster. Retraction is non-fatal: a workspace that cannot be
    // cleaned must not block the asset refresh itself.
    let deploy: DeployResult =
        deploy_agents_filtered(&plan.agent_source, &fw.agent_deploy_dir(), |name| {
            plan.agent_selected(name)
        })
        .map_err(|e| SyncAssetsError::AgentDeploy(e.to_string()))?;

    // Targets `project_dir`'s own `.claude/agents`, not `fw.claude_agents_dir()`
    // — see the identical note in `prepare_session_inner`. Retraction is a
    // WORKSPACE operation, and a home-tier `fw` would otherwise aim it at the
    // operator's `~/.claude/agents`.
    let retraction_error =
        match retract_framework_agents(&project_dir.join(".claude").join("agents")) {
            Ok(_) => None,
            Err(err) => {
                // Surfaced on the report, not just logged: whether a workspace is
                // still shadowing the canonical roster is the one signal an
                // operator running sync-assets actually needs, and the
                // `prepare_session` path already reports it via `roster_errors`.
                tracing::warn!(
                    project_dir = %project_dir.display(),
                    "workspace bundled-agent retraction failed during sync-assets: {err}"
                );
                Some(err.to_string())
            }
        };

    // #4448: retraction's blind spot — an untracked copy written before the
    // ownership ledger existed. Same wiring `prepare_session_inner` uses (one
    // entry point, so the two paths cannot aim differently), same
    // after-retraction ordering, same four-gate refusal set. Never deletes.
    // Test: `sync_assets_quarantines_a_shadowing_workspace_agent`.
    let quarantine_summary =
        match super::quarantine_shadows::quarantine_workspace_shadows(fw, project_dir) {
            Ok(report) => super::quarantine_shadows::summarize(&report),
            Err(err) => {
                tracing::warn!(
                    project_dir = %project_dir.display(),
                    "shadowing-agent quarantine refused to run during sync-assets: {err}"
                );
                Some(format!("agent quarantine refused: {err}"))
            }
        };

    let co_deploy_skills = co_deploy_skill_set(&deploy.declared_skills);
    let skill_deploy: DeployStats = deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &fw.claude_skills_dir(),
        |name| plan.skill_selected(name) || co_deploy_skills.contains(name),
    )
    .map_err(|e| SyncAssetsError::SkillDeploy(e.to_string()))?
    .stats;

    // Non-fatal, mirroring `prepare_session_inner`'s own treatment of this step.
    let output_style_synced = deploy_output_style(project_dir).is_ok();

    Ok(SyncAssetsReport {
        agents_deployed: deploy.deployed,
        agents_skipped: deploy.skipped,
        skills_deployed: skill_deploy.deployed,
        skills_skipped: skill_deploy.skipped,
        output_style_synced,
        retraction_error,
        quarantine_summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_assets_redeploys_new_agent() {
        // An agent added to the bundled source AFTER a session's original
        // deploy must appear in the workspace on the next sync-assets run.
        let tmp = tempfile::tempdir().unwrap();
        let mut fw = FrameworkPaths::under(tmp.path().join("home"));
        fw.trusty_mpm_root = None;
        let bundled = fw.agent_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(
            bundled.join("rust-engineer.md"),
            "---\nname: rust-engineer\n---\nv1",
        )
        .unwrap();

        let project_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&project_dir).unwrap();
        let mut ws_fw = fw.clone();
        ws_fw.claude_agents = project_dir.join(".claude").join("agents");
        ws_fw.claude_skills = project_dir.join(".claude").join("skills");

        let first = sync_session_assets(&ws_fw, &project_dir).unwrap();
        assert_eq!(first.agents_deployed, vec!["rust-engineer.md".to_string()]);
        assert!(!ws_fw.agent_deploy_dir().join("qa.md").exists());

        // Catalog gains a new agent mid-session.
        std::fs::write(bundled.join("qa.md"), "---\nname: qa\n---\nv1").unwrap();
        let second = sync_session_assets(&ws_fw, &project_dir).unwrap();
        assert!(second.agents_deployed.contains(&"qa.md".to_string()));
        assert!(ws_fw.agent_deploy_dir().join("qa.md").exists());
    }

    #[test]
    fn sync_assets_refreshes_stale_skill() {
        // The exact #2444 scenario for skills: a bundled skill bumped after the
        // session's original deploy must be refreshed on disk.
        let tmp = tempfile::tempdir().unwrap();
        let mut fw = FrameworkPaths::under(tmp.path().join("home"));
        fw.trusty_mpm_root = None;
        let bundled = fw.skill_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(bundled.join("tm-doctor.md"), "v1.0.0").unwrap();

        let project_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&project_dir).unwrap();
        let mut ws_fw = fw.clone();
        ws_fw.claude_agents = project_dir.join(".claude").join("agents");
        ws_fw.claude_skills = project_dir.join(".claude").join("skills");

        sync_session_assets(&ws_fw, &project_dir).unwrap();
        let deployed_path = ws_fw.claude_skills_dir().join("tm-doctor").join("SKILL.md");
        assert_eq!(std::fs::read_to_string(&deployed_path).unwrap(), "v1.0.0");

        // The catalog/bundled skill bumps version mid-session.
        std::fs::write(bundled.join("tm-doctor.md"), "v1.1.0").unwrap();
        let report = sync_session_assets(&ws_fw, &project_dir).unwrap();
        assert!(report.skills_deployed.contains(&"tm-doctor".to_string()));
        assert_eq!(std::fs::read_to_string(&deployed_path).unwrap(), "v1.1.0");
    }

    #[test]
    fn sync_assets_refreshes_project_output_style() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fw = FrameworkPaths::under(tmp.path().join("home"));
        fw.trusty_mpm_root = None;

        let project_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&project_dir).unwrap();
        let mut ws_fw = fw.clone();
        ws_fw.claude_agents = project_dir.join(".claude").join("agents");
        ws_fw.claude_skills = project_dir.join(".claude").join("skills");

        let report = sync_session_assets(&ws_fw, &project_dir).unwrap();
        assert!(report.output_style_synced);
        assert!(
            project_dir
                .join(".claude")
                .join("output-styles")
                .join("trusty-mpm.md")
                .exists()
        );
    }

    #[test]
    fn sync_assets_never_touches_hand_placed_agent() {
        // A hand-placed agent — a file the operator dropped into
        // `.claude/agents/` that trusty-mpm never deployed, so it is absent
        // from the deploy manifest — must survive session-asset sync
        // byte-identical, even when it shares a bundled agent's filename.
        let tmp = tempfile::tempdir().unwrap();
        let mut fw = FrameworkPaths::under(tmp.path().join("home"));
        fw.trusty_mpm_root = None;
        let bundled = fw.agent_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(
            bundled.join("rust-engineer.md"),
            "---\nname: rust-engineer\n---\nv1",
        )
        .unwrap();

        let project_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&project_dir).unwrap();
        let mut ws_fw = fw.clone();
        ws_fw.claude_agents = project_dir.join(".claude").join("agents");
        ws_fw.claude_skills = project_dir.join(".claude").join("skills");

        // The operator's own file lands first — never deployed by trusty-mpm.
        std::fs::create_dir_all(ws_fw.agent_deploy_dir()).unwrap();
        let hand_placed = ws_fw.agent_deploy_dir().join("rust-engineer.md");
        let content = "---\nname: rust-engineer\n---\noperator's own agent";
        std::fs::write(&hand_placed, content).unwrap();

        let report = sync_session_assets(&ws_fw, &project_dir).unwrap();
        assert!(
            report
                .agents_skipped
                .contains(&"rust-engineer.md".to_string())
        );
        assert_eq!(std::fs::read_to_string(&hand_placed).unwrap(), content);
    }

    #[test]
    fn sync_assets_repairs_drifted_bundled_agent() {
        // Issue #4408: a BUNDLED agent this deployer wrote is framework-owned
        // (the `InstallPolicy::Overwrite` tier), so a deployed copy that
        // drifted from its manifest checksum — here the degenerate stub from
        // the incident — is refreshed from the bundle rather than frozen as a
        // user edit. Freezing it made the agent unrecoverable in that
        // workspace forever.
        let tmp = tempfile::tempdir().unwrap();
        let mut fw = FrameworkPaths::under(tmp.path().join("home"));
        fw.trusty_mpm_root = None;
        let bundled = fw.agent_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(
            bundled.join("rust-engineer.md"),
            "---\nname: rust-engineer\n---\nv1",
        )
        .unwrap();

        let project_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&project_dir).unwrap();
        let mut ws_fw = fw.clone();
        ws_fw.claude_agents = project_dir.join(".claude").join("agents");
        ws_fw.claude_skills = project_dir.join(".claude").join("skills");

        sync_session_assets(&ws_fw, &project_dir).unwrap();
        let deployed = ws_fw.agent_deploy_dir().join("rust-engineer.md");
        std::fs::write(&deployed, "---\nname: rust-engineer\n---\n\nv1\n").unwrap();

        std::fs::write(
            bundled.join("rust-engineer.md"),
            "---\nname: rust-engineer\n---\nv2",
        )
        .unwrap();
        let report = sync_session_assets(&ws_fw, &project_dir).unwrap();
        assert!(
            report
                .agents_deployed
                .contains(&"rust-engineer.md".to_string()),
            "drifted bundled agent must be repaired, got: {report:?}"
        );
        assert!(
            !report
                .agents_skipped
                .contains(&"rust-engineer.md".to_string())
        );
        assert!(
            std::fs::read_to_string(&deployed).unwrap().contains("v2"),
            "the current bundled composition must be restored"
        );
    }

    #[test]
    fn sync_assets_retracts_workspace_bundled_agents() {
        // Issue #4409: a workspace provisioned by an OLDER binary carries a
        // full bundled roster in its own `.claude/agents/`, which outranks the
        // config-dir tier. Syncing must clear those framework-owned copies —
        // and must leave a hand-placed agent in the same directory alone.
        let tmp = tempfile::tempdir().unwrap();
        let mut fw = FrameworkPaths::under(tmp.path().join("home"));
        fw.trusty_mpm_root = None;
        let bundled = fw.agent_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(
            bundled.join("rust-engineer.md"),
            "---\nname: rust-engineer\n---\nv1",
        )
        .unwrap();

        let project_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&project_dir).unwrap();
        let mut ws_fw = fw.clone();
        ws_fw.claude_agents = project_dir.join(".claude").join("agents");
        ws_fw.claude_skills = project_dir.join(".claude").join("skills");

        // Simulate the legacy per-workspace deploy an older binary performed.
        let legacy = ws_fw.claude_agents_dir();
        deploy_agents_filtered(&bundled, &legacy, |_| true).unwrap();
        assert!(legacy.join("rust-engineer.md").exists());

        // …plus an agent the operator dropped in by hand, which trusty-mpm
        // never deployed and must never remove.
        let hand_placed = legacy.join("my-own-agent.md");
        let mine = "---\nname: my-own-agent\ndescription: mine\n---\n\nMine.\n";
        std::fs::write(&hand_placed, mine).unwrap();

        sync_session_assets(&ws_fw, &project_dir).unwrap();

        assert!(
            !legacy.join("rust-engineer.md").exists(),
            "the legacy per-workspace bundled copy must be retracted"
        );
        assert_eq!(
            std::fs::read_to_string(&hand_placed).unwrap(),
            mine,
            "a hand-placed agent must survive retraction byte-identical"
        );
        assert!(
            ws_fw.agent_deploy_dir().join("rust-engineer.md").exists(),
            "the canonical config-dir tier must carry the roster instead"
        );
    }

    #[test]
    fn sync_assets_reports_retraction_failure_on_corrupt_manifest() {
        // Whether a workspace is still shadowing the canonical roster is the
        // signal an operator running sync-assets needs. A bare log line meant
        // the CLI and daemon route could not see it, while the
        // `prepare_session` path reported the identical failure through
        // `roster_errors` — an asymmetry on the one thing worth surfacing.
        let tmp = tempfile::tempdir().unwrap();
        let mut fw = FrameworkPaths::under(tmp.path().join("home"));
        fw.trusty_mpm_root = None;
        let bundled = fw.agent_source_dir();
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(
            bundled.join("rust-engineer.md"),
            "---\nname: rust-engineer\n---\nv1",
        )
        .unwrap();

        let project_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&project_dir).unwrap();
        let mut ws_fw = fw.clone();
        ws_fw.claude_agents = project_dir.join(".claude").join("agents");
        ws_fw.claude_skills = project_dir.join(".claude").join("skills");

        // An unreadable ownership ledger in the workspace: retraction refuses
        // to act on it (correctly), and that refusal must reach the report.
        let legacy = project_dir.join(".claude").join("agents");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join(crate::core::agent_manifest::MANIFEST_FILE),
            b"not valid json{{{",
        )
        .unwrap();

        let report = sync_session_assets(&ws_fw, &project_dir).unwrap();

        assert!(
            report
                .retraction_error
                .as_deref()
                .is_some_and(|e| e.contains("corrupt")),
            "the refusal must be reported, got: {:?}",
            report.retraction_error
        );
        // The asset refresh itself still succeeded — retraction is non-fatal.
        assert!(
            report
                .agents_deployed
                .contains(&"rust-engineer.md".to_string())
        );
    }
}
