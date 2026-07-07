//! Validate a managed workspace's deployed `.claude/{agents,skills}` payload
//! and `settings.json` against the expected per-project roster (issue #2158,
//! refined by #2171).
//!
//! Why: a managed worktree can launch with an INCOMPLETE `.claude/` payload —
//! missing agents, no deployed skills, a stripped `settings.json` with no
//! `outputStyle`/hooks, no per-workspace ownership manifest — silently,
//! because nothing ever diffed the deployed payload against what a complete
//! deploy must contain. This module is that diff: given a resolved
//! [`FrameworkPaths`], it enumerates every gap so callers (`tm doctor`, `tm
//! validate`, and the daemon spawn/resume path) can auto-repair or fail
//! loudly instead of handing the operator a half-provisioned session.
//! What: [`validate_workspace`] compares the deployed `.claude/agents/` and
//! `.claude/skills/` directories (plus their ownership manifests) against the
//! EXPECTED set [`expected_set::expected_agent_stems`] /
//! [`expected_set::expected_skill_stems`] resolve for this workspace — the
//! SAME per-project [`crate::core::manifest::HarnessPlan`]
//! `prepare_session_inner` computes before calling
//! [`crate::core::agent_deployer::deploy_agents_filtered`] /
//! [`crate::core::skill_deployer::deploy_skills_filtered`], not the
//! unconditional full bundled roster (issue #2171 — a workspace legitimately
//! provisioned from a FILTERED roster, e.g. every specialist `*-engineer`
//! present but the generic `engineer` catch-all deliberately excluded, was
//! previously reported incomplete for every excluded entry). When the plan
//! cannot be usefully reconstructed (its resolved source directory is empty
//! or missing), `expected_set` falls back to the workspace's OWN deployed
//! ownership manifest (validating internal consistency: every entry the
//! manifest claims to manage must exist on disk), and only when neither
//! yields anything falls back further to the unconditional full canonical
//! bundled roster (the pre-#2171 behavior) — see `expected_set`'s module doc
//! for the full fallback contract. [`validate_workspace`] also checks
//! `.claude/settings.json` (at [`FrameworkPaths::claude_home_dir`]) for a
//! resolvable `outputStyle` and a configured `hooks` key.
//! [`validate_and_repair`] re-runs
//! [`crate::core::session_launch::prepare_session_with_repo_url`] — the exact
//! deploy pipeline `spawn_managed`/`resume_managed` already use — when the
//! initial validation finds gaps, then re-validates so callers can tell
//! whether the repair actually closed them.
//! Test: `validate_missing_agent_manifest_is_a_gap`,
//! `validate_missing_agent_is_a_gap`, `validate_missing_skill_manifest_is_a_gap`,
//! `validate_missing_skill_is_a_gap`, `validate_settings_missing_is_a_gap`,
//! `validate_missing_output_style_key_is_a_gap`,
//! `validate_unknown_output_style_id_is_a_gap`,
//! `validate_output_style_file_missing_is_a_gap`, `validate_missing_hooks_is_a_gap`,
//! `validate_complete_workspace_has_no_gaps`,
//! `validate_filtered_but_manifest_matching_workspace_has_no_gaps`,
//! `validate_entry_missing_on_disk_but_in_manifest_is_still_a_gap`,
//! `repair_closes_gaps_on_incomplete_workspace`,
//! `repair_is_a_noop_on_already_complete_workspace`; the expected-set
//! resolution itself is covered by `expected_set`'s own test module.

use std::path::Path;

use crate::core::agent_manifest::{self, AgentManifest, ManifestLoad};
use crate::core::bundle::OUTPUT_STYLES;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_manifest;

mod expected_set;
use expected_set::{expected_agent_stems, expected_skill_stems};

/// One concrete gap between a deployed workspace and the canonical roster.
///
/// Why: `tm doctor`/`tm validate` and the spawn/resume auto-repair gate all
/// need to distinguish WHICH thing is missing, not just "incomplete" — the
/// doctor/CLI surfaces render `describe()`; the auto-repair gate only cares
/// whether [`ValidationReport::is_complete`] is `false`.
/// What: one variant per gap class this module can detect.
/// Test: one dedicated test per variant, listed on the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentGap {
    /// `.claude/agents/.trusty-mpm-manifest.json` does not exist.
    AgentManifestMissing,
    /// The agent manifest exists but failed to parse.
    AgentManifestCorrupt(String),
    /// A canonical bundled agent (`<name>.md`) is not deployed on disk.
    AgentMissing(String),
    /// `.claude/skills/.trusty-mpm-skills-manifest.json` does not exist.
    SkillManifestMissing,
    /// A canonical bundled skill (`<name>/SKILL.md`) is not deployed on disk.
    SkillMissing(String),
    /// `.claude/settings.json` itself does not exist.
    SettingsMissing,
    /// `.claude/settings.json` exists but is not valid JSON.
    SettingsMalformed(String),
    /// `.claude/settings.json` has no `outputStyle` key set.
    OutputStyleKeyMissing,
    /// `outputStyle` names an id unknown to the bundled style catalog.
    OutputStyleUnknownId(String),
    /// The resolved style id's file is missing or empty under
    /// `.claude/output-styles/`.
    OutputStyleFileMissing(String),
    /// `.claude/settings.json` has no (non-empty) `hooks` key configured.
    HooksMissing,
}

impl DeploymentGap {
    /// One-line, operator-facing description of the gap.
    ///
    /// Why: `tm doctor` and `tm validate` render gaps as plain text; keeping
    /// the phrasing here means both surfaces stay identical.
    /// What: a short, human-readable sentence naming what is wrong.
    /// Test: `describe_is_non_empty_for_every_variant`.
    pub fn describe(&self) -> String {
        match self {
            Self::AgentManifestMissing => format!(
                "agent ownership manifest ({}) is missing",
                agent_manifest::MANIFEST_FILE
            ),
            Self::AgentManifestCorrupt(detail) => format!("agent manifest is corrupt: {detail}"),
            Self::AgentMissing(name) => format!("agent `{name}` is not deployed"),
            Self::SkillManifestMissing => format!(
                "skill ownership manifest ({}) is missing",
                skill_manifest::SKILL_MANIFEST_FILE
            ),
            Self::SkillMissing(name) => format!("skill `{name}` is not deployed"),
            Self::SettingsMissing => ".claude/settings.json is missing".to_string(),
            Self::SettingsMalformed(detail) => {
                format!(".claude/settings.json is not valid JSON: {detail}")
            }
            Self::OutputStyleKeyMissing => {
                "settings.json has no outputStyle key configured".to_string()
            }
            Self::OutputStyleUnknownId(id) => {
                format!("outputStyle {id:?} is not a known trusty-mpm style")
            }
            Self::OutputStyleFileMissing(id) => {
                format!("outputStyle {id:?} has no deployed style file")
            }
            Self::HooksMissing => "settings.json has no hooks configured".to_string(),
        }
    }
}

/// The outcome of [`validate_workspace`].
///
/// Why: callers need both the raw gap list (for detailed reporting) and a
/// single completeness verdict (for the spawn/resume gate).
/// What: an ordered [`Vec<DeploymentGap>`]; empty means complete.
/// Test: `validate_complete_workspace_has_no_gaps`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// Every gap found, in the order the probes ran.
    pub gaps: Vec<DeploymentGap>,
}

impl ValidationReport {
    /// Whether the deployment is complete (no gaps found).
    ///
    /// Why: the spawn/resume gate and `tm validate`'s exit code both reduce
    /// to this single boolean.
    /// What: `true` iff `gaps` is empty.
    /// Test: `validate_complete_workspace_has_no_gaps`.
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }
}

/// Validate `fw`'s deployed `.claude/{agents,skills}` payload and
/// `settings.json` against the canonical bundled roster.
///
/// Why: the single entry point behind `tm doctor`'s manifest-completeness
/// check, `tm validate`, and the daemon's pre-handoff spawn/resume gate — one
/// implementation means the three surfaces can never disagree about what
/// "complete" means.
/// What: runs the agent, skill, and settings probes in order and folds their
/// findings into a [`ValidationReport`]. Pure filesystem reads — no writes.
/// Test: every `validate_*` test in this module's `tests` submodule.
pub fn validate_workspace(fw: &FrameworkPaths) -> ValidationReport {
    let mut gaps = Vec::new();
    validate_agents(fw, &mut gaps);
    validate_skills(fw, &mut gaps);
    validate_settings(fw, &mut gaps);
    ValidationReport { gaps }
}

/// Probe the deployed agent roster against the EXPECTED per-project set
/// (issue #2171 — see [`expected_set::expected_agent_stems`]).
fn validate_agents(fw: &FrameworkPaths, gaps: &mut Vec<DeploymentGap>) {
    let target = fw.claude_agents_dir();
    let manifest = match AgentManifest::load_checked(&target) {
        ManifestLoad::Corrupt(detail) => {
            gaps.push(DeploymentGap::AgentManifestCorrupt(detail));
            None
        }
        ManifestLoad::Ok(m) => {
            if !target.join(agent_manifest::MANIFEST_FILE).is_file() {
                gaps.push(DeploymentGap::AgentManifestMissing);
            }
            Some(m)
        }
    };
    for name in expected_agent_stems(fw, manifest.as_ref()) {
        if !target.join(format!("{name}.md")).is_file() {
            gaps.push(DeploymentGap::AgentMissing(name));
        }
    }
}

/// Probe the deployed skill roster against the EXPECTED per-project set
/// (issue #2171 — see [`expected_set::expected_skill_stems`]).
fn validate_skills(fw: &FrameworkPaths, gaps: &mut Vec<DeploymentGap>) {
    let target = fw.claude_skills_dir();
    let manifest_present = target.join(skill_manifest::SKILL_MANIFEST_FILE).is_file();
    if !manifest_present {
        gaps.push(DeploymentGap::SkillManifestMissing);
    }
    let manifest = manifest_present.then(|| skill_manifest::SkillManifest::load(&target));
    for name in expected_skill_stems(fw, manifest.as_ref()) {
        if !target.join(&name).join("SKILL.md").is_file() {
            gaps.push(DeploymentGap::SkillMissing(name));
        }
    }
}

/// Probe `.claude/settings.json` for a resolvable `outputStyle` and a
/// configured `hooks` key.
fn validate_settings(fw: &FrameworkPaths, gaps: &mut Vec<DeploymentGap>) {
    let settings_path = fw.claude_home_dir().join(".claude").join("settings.json");
    let text = match std::fs::read_to_string(&settings_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            gaps.push(DeploymentGap::SettingsMissing);
            return;
        }
        Err(e) => {
            gaps.push(DeploymentGap::SettingsMalformed(e.to_string()));
            return;
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            gaps.push(DeploymentGap::SettingsMalformed(e.to_string()));
            return;
        }
    };

    match value.get("outputStyle").and_then(|v| v.as_str()) {
        None => gaps.push(DeploymentGap::OutputStyleKeyMissing),
        Some(id) => match OUTPUT_STYLES.iter().find(|s| s.id == id) {
            None => gaps.push(DeploymentGap::OutputStyleUnknownId(id.to_string())),
            Some(style) => {
                let style_path = fw
                    .claude_home_dir()
                    .join(".claude")
                    .join("output-styles")
                    .join(style.file_name);
                let deployed = std::fs::metadata(&style_path)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false);
                if !deployed {
                    gaps.push(DeploymentGap::OutputStyleFileMissing(id.to_string()));
                }
            }
        },
    }

    let hooks_present = value
        .get("hooks")
        .and_then(|h| h.as_object())
        .is_some_and(|o| !o.is_empty());
    if !hooks_present {
        gaps.push(DeploymentGap::HooksMissing);
    }
}

/// The outcome of [`validate_and_repair`].
///
/// Why: callers (the spawn/resume gate, `tm validate --repair`) need to know
/// not just the final state but whether a repair was attempted and whether it
/// actually closed the gaps found before it ran.
/// What: the pre-repair and post-repair [`ValidationReport`]s, whether a
/// repair attempt ran at all, and the repair error (if any).
/// Test: `repair_closes_gaps_on_incomplete_workspace`,
/// `repair_is_a_noop_on_already_complete_workspace`,
/// `repair_reports_error_when_prepare_session_fails`.
#[derive(Debug)]
pub struct RepairOutcome {
    /// Validation result before any repair attempt.
    pub before: ValidationReport,
    /// Validation result after the repair attempt (equals `before` when no
    /// repair ran).
    pub after: ValidationReport,
    /// Whether a repair attempt actually ran (only when `before` was incomplete).
    pub repaired: bool,
    /// The repair pipeline's error, if [`crate::core::session_launch::prepare_session_with_repo_url`]
    /// returned `Err`. A repair that ran but left gaps (a partial repair) is
    /// NOT an error here — check `after.is_complete()` for that.
    pub repair_error: Option<String>,
}

impl RepairOutcome {
    /// Whether the workspace is complete after this repair attempt.
    ///
    /// Why: the spawn/resume gate's final pass/fail decision reduces to this
    /// one call.
    /// What: delegates to `after.is_complete()`.
    /// Test: covered by every `repair_*` test.
    pub fn is_complete(&self) -> bool {
        self.after.is_complete()
    }
}

/// Validate `workspace`, and if incomplete, re-run the deploy pipeline and
/// re-validate.
///
/// Why (#2158): a managed session must never be handed to the operator with a
/// silently-incomplete `.claude/` payload. Auto-repair is preferred over
/// failing outright — most gaps (a missing agent, a stale output-style file)
/// are exactly what re-running the deploy step fixes, and the deploy
/// machinery ([`crate::core::agent_deployer::deploy_agents_filtered`] /
/// [`crate::core::skill_deployer::deploy_skills_filtered`]) is already safe
/// to re-run (checksum-based skip of user-modified files, additive `.mcp.json`
/// merges). This reuses the EXACT pipeline `spawn_managed`/
/// `spawn_managed_inproject`/`resume_managed` already call for a fresh
/// session — no parallel repair implementation to drift from it.
/// What: calls [`validate_workspace`]; if complete, returns immediately with
/// `repaired: false`. Otherwise calls
/// [`crate::core::session_launch::prepare_session_with_repo_url`]`(fw,
/// workspace, repo_url)` (the #2149 roster/output-style/hooks pipeline),
/// re-validates, and returns both reports plus any repair error.
/// Test: `repair_closes_gaps_on_incomplete_workspace`,
/// `repair_is_a_noop_on_already_complete_workspace`.
pub fn validate_and_repair(
    fw: &FrameworkPaths,
    workspace: &Path,
    repo_url: Option<&str>,
) -> RepairOutcome {
    let before = validate_workspace(fw);
    if before.is_complete() {
        return RepairOutcome {
            after: before.clone(),
            before,
            repaired: false,
            repair_error: None,
        };
    }

    let repair_error =
        match crate::core::session_launch::prepare_session_with_repo_url(fw, workspace, repo_url) {
            Ok(report) if !report.roster_errors.is_empty() => Some(report.roster_errors.join("; ")),
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        };

    let after = validate_workspace(fw);
    RepairOutcome {
        before,
        after,
        repaired: true,
        repair_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a hermetic `FrameworkPaths` whose SOURCE dirs are seeded and
    /// whose `trusty_mpm_root` is forced to `None`, mirroring the pattern
    /// `doctor_fs_checks.rs` uses so the resolution never escapes the temp dir
    /// into the real checkout the test binary happens to run inside.
    fn hermetic_paths(base: &Path) -> FrameworkPaths {
        let mut fw = FrameworkPaths::under(base);
        fw.trusty_mpm_root = None;
        fw
    }

    fn seed_agent_source(fw: &FrameworkPaths, names: &[&str]) {
        std::fs::create_dir_all(&fw.agents).unwrap();
        for name in names {
            std::fs::write(
                fw.agents.join(format!("{name}.md")),
                format!("---\nname: {name}\ndescription: d\n---\n\nBody.\n"),
            )
            .unwrap();
        }
    }

    fn seed_skill_source(fw: &FrameworkPaths, names: &[&str]) {
        std::fs::create_dir_all(&fw.skills).unwrap();
        for name in names {
            std::fs::write(fw.skills.join(format!("{name}.md")), "skill body").unwrap();
        }
    }

    fn write_settings(fw: &FrameworkPaths, json: &str) {
        let claude_dir = fw.claude_home_dir().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("settings.json"), json).unwrap();
    }

    fn deploy_style_file(fw: &FrameworkPaths) {
        let style_dir = fw.claude_home_dir().join(".claude").join("output-styles");
        std::fs::create_dir_all(&style_dir).unwrap();
        let default = OUTPUT_STYLES[0];
        std::fs::write(style_dir.join(default.file_name), default.content).unwrap();
    }

    /// A minimal, deterministic agent-manifest entry for the entries the
    /// #2171 fallback tests record directly (no real deploy pipeline run).
    fn sample_agent_entry(source_name: &str) -> crate::core::agent_manifest::ManifestEntry {
        crate::core::agent_manifest::ManifestEntry {
            source_chain: vec![source_name.to_string()],
            checksum: agent_manifest::checksum("agent"),
            deployed_at: "2026-01-01T00:00:00Z".to_string(),
            origin: crate::core::agent_manifest::Origin::Bundled,
        }
    }

    /// A fully-provisioned workspace matching everything `prepare_session`
    /// would have written, used as the positive-path baseline every negative
    /// test starts from and mutates one field of.
    fn fully_provisioned(base: &Path) -> FrameworkPaths {
        let fw = hermetic_paths(base);
        seed_agent_source(&fw, &["engineer", "BASE-AGENT"]);
        seed_skill_source(&fw, &["tm-doctor"]);

        let agents_dir = fw.claude_agents_dir();
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("engineer.md"), "agent").unwrap();
        std::fs::write(agents_dir.join("BASE-AGENT.md"), "base").unwrap();
        AgentManifest::default().save(&agents_dir).unwrap();

        let skills_dir = fw.claude_skills_dir();
        std::fs::create_dir_all(skills_dir.join("tm-doctor")).unwrap();
        std::fs::write(skills_dir.join("tm-doctor").join("SKILL.md"), "skill").unwrap();
        crate::core::skill_manifest::SkillManifest::default()
            .save(&skills_dir)
            .unwrap();

        deploy_style_file(&fw);
        write_settings(
            &fw,
            r#"{"outputStyle": "trusty-mpm", "hooks": {"SessionStart": []}}"#,
        );
        fw
    }

    #[test]
    fn validate_complete_workspace_has_no_gaps() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        let report = validate_workspace(&fw);
        assert!(
            report.is_complete(),
            "expected no gaps, got: {:?}",
            report.gaps
        );
    }

    #[test]
    fn validate_filtered_but_manifest_matching_workspace_has_no_gaps() {
        // #2171: a workspace provisioned from a FILTERED per-project roster
        // (a project manifest override excludes the generic `engineer`
        // catch-all; only `rust-engineer` is deployed, matching its own
        // ownership manifest exactly) must validate COMPLETE. The pre-fix
        // validator diffed against the unconditional full bundled roster and
        // falsely reported `engineer` missing even though it was never
        // supposed to be deployed here.
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        fw.trusty_mpm_root = None;

        seed_agent_source(&fw, &["engineer", "rust-engineer"]);
        seed_skill_source(&fw, &["tm-doctor"]);

        let manifest_dir = workspace.join(".trusty-mpm");
        std::fs::create_dir_all(&manifest_dir).unwrap();
        std::fs::write(
            manifest_dir.join("manifest.toml"),
            "[agents]\nexclude = [\"engineer\"]\n",
        )
        .unwrap();

        let agents_dir = fw.claude_agents_dir();
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("rust-engineer.md"), "agent").unwrap();
        let mut agent_manifest = AgentManifest::default();
        agent_manifest.managed.insert(
            "rust-engineer.md".to_string(),
            sample_agent_entry("rust-engineer"),
        );
        agent_manifest.save(&agents_dir).unwrap();

        let skills_dir = fw.claude_skills_dir();
        std::fs::create_dir_all(skills_dir.join("tm-doctor")).unwrap();
        std::fs::write(skills_dir.join("tm-doctor").join("SKILL.md"), "skill").unwrap();
        crate::core::skill_manifest::SkillManifest::default()
            .save(&skills_dir)
            .unwrap();

        deploy_style_file(&fw);
        write_settings(
            &fw,
            r#"{"outputStyle": "trusty-mpm", "hooks": {"SessionStart": []}}"#,
        );

        let report = validate_workspace(&fw);
        assert!(
            report.is_complete(),
            "filtered-but-complete workspace must validate as complete, got: {:?}",
            report.gaps
        );
    }

    #[test]
    fn validate_entry_missing_on_disk_but_in_manifest_is_still_a_gap() {
        // The plan's bundled source directory is unpopulated (a binary-only
        // install with no `framework/agents` yet), so expected-set resolution
        // falls through to tier (b): the workspace's own deployed manifest.
        // An entry the manifest claims to manage but which is NOT actually
        // present on disk must still be reported — the fallback must never
        // suppress a genuine gap, and an entry that IS on disk must not be
        // falsely flagged.
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        fw.trusty_mpm_root = None;
        // No seed_agent_source call — the plan's bundled source stays empty.

        let agents_dir = fw.claude_agents_dir();
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("rust-engineer.md"), "agent").unwrap();
        let mut agent_manifest = AgentManifest::default();
        agent_manifest.managed.insert(
            "rust-engineer.md".to_string(),
            sample_agent_entry("rust-engineer"),
        );
        agent_manifest.managed.insert(
            "python-engineer.md".to_string(),
            sample_agent_entry("python-engineer"),
        );
        agent_manifest.save(&agents_dir).unwrap();

        let report = validate_workspace(&fw);
        assert!(
            report
                .gaps
                .contains(&DeploymentGap::AgentMissing("python-engineer".to_string())),
            "manifest-recorded entry missing on disk must still be reported, got: {:?}",
            report.gaps
        );
        assert!(
            !report
                .gaps
                .contains(&DeploymentGap::AgentMissing("rust-engineer".to_string())),
            "the entry that IS on disk must not be falsely reported missing, got: {:?}",
            report.gaps
        );
    }

    #[test]
    fn validate_missing_agent_manifest_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        std::fs::remove_file(fw.claude_agents_dir().join(agent_manifest::MANIFEST_FILE)).unwrap();
        let report = validate_workspace(&fw);
        assert!(report.gaps.contains(&DeploymentGap::AgentManifestMissing));
    }

    #[test]
    fn validate_missing_agent_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        std::fs::remove_file(fw.claude_agents_dir().join("engineer.md")).unwrap();
        let report = validate_workspace(&fw);
        assert!(
            report
                .gaps
                .contains(&DeploymentGap::AgentMissing("engineer".to_string()))
        );
    }

    #[test]
    fn validate_missing_skill_manifest_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        std::fs::remove_file(
            fw.claude_skills_dir()
                .join(skill_manifest::SKILL_MANIFEST_FILE),
        )
        .unwrap();
        let report = validate_workspace(&fw);
        assert!(report.gaps.contains(&DeploymentGap::SkillManifestMissing));
    }

    #[test]
    fn validate_missing_skill_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        std::fs::remove_dir_all(fw.claude_skills_dir().join("tm-doctor")).unwrap();
        let report = validate_workspace(&fw);
        assert!(
            report
                .gaps
                .contains(&DeploymentGap::SkillMissing("tm-doctor".to_string()))
        );
    }

    #[test]
    fn validate_settings_missing_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        std::fs::remove_file(fw.claude_home_dir().join(".claude").join("settings.json")).unwrap();
        let report = validate_workspace(&fw);
        assert!(report.gaps.contains(&DeploymentGap::SettingsMissing));
    }

    #[test]
    fn validate_missing_output_style_key_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        write_settings(&fw, r#"{"hooks": {"SessionStart": []}}"#);
        let report = validate_workspace(&fw);
        assert!(report.gaps.contains(&DeploymentGap::OutputStyleKeyMissing));
    }

    #[test]
    fn validate_unknown_output_style_id_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        write_settings(
            &fw,
            r#"{"outputStyle": "claude_mpm", "hooks": {"SessionStart": []}}"#,
        );
        let report = validate_workspace(&fw);
        assert!(report.gaps.contains(&DeploymentGap::OutputStyleUnknownId(
            "claude_mpm".to_string()
        )));
    }

    #[test]
    fn validate_output_style_file_missing_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        let style_dir = fw.claude_home_dir().join(".claude").join("output-styles");
        std::fs::remove_dir_all(&style_dir).unwrap();
        let report = validate_workspace(&fw);
        assert!(report.gaps.contains(&DeploymentGap::OutputStyleFileMissing(
            "trusty-mpm".to_string()
        )));
    }

    #[test]
    fn validate_missing_hooks_is_a_gap() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        write_settings(&fw, r#"{"outputStyle": "trusty-mpm"}"#);
        let report = validate_workspace(&fw);
        assert!(report.gaps.contains(&DeploymentGap::HooksMissing));
    }

    #[test]
    fn describe_is_non_empty_for_every_variant() {
        let gaps = [
            DeploymentGap::AgentManifestMissing,
            DeploymentGap::AgentManifestCorrupt("bad".to_string()),
            DeploymentGap::AgentMissing("engineer".to_string()),
            DeploymentGap::SkillManifestMissing,
            DeploymentGap::SkillMissing("tm-doctor".to_string()),
            DeploymentGap::SettingsMissing,
            DeploymentGap::SettingsMalformed("bad json".to_string()),
            DeploymentGap::OutputStyleKeyMissing,
            DeploymentGap::OutputStyleUnknownId("x".to_string()),
            DeploymentGap::OutputStyleFileMissing("x".to_string()),
            DeploymentGap::HooksMissing,
        ];
        for gap in gaps {
            assert!(!gap.describe().is_empty());
        }
    }

    #[test]
    fn repair_is_a_noop_on_already_complete_workspace() {
        let tmp = TempDir::new().unwrap();
        let fw = fully_provisioned(tmp.path());
        let outcome = validate_and_repair(&fw, &fw.claude_home_dir(), None);
        assert!(!outcome.repaired);
        assert!(outcome.is_complete());
        assert_eq!(outcome.before, outcome.after);
    }

    #[test]
    fn repair_closes_gaps_on_incomplete_workspace() {
        // Seed only the framework SOURCE roster (what `prepare_session_inner`
        // would deploy from) but leave the workspace `.claude/` entirely
        // unprovisioned — the exact "spawned with an incomplete payload"
        // scenario #2158 describes. The repair path must deploy everything
        // and leave the workspace complete.
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        let mut fw = fw;
        fw.trusty_mpm_root = None;
        seed_agent_source(&fw, &["engineer"]);
        seed_skill_source(&fw, &["tm-doctor"]);

        let before = validate_workspace(&fw);
        assert!(!before.is_complete(), "expected gaps before repair");

        let outcome = validate_and_repair(&fw, &workspace, None);
        assert!(outcome.repaired);
        assert!(
            outcome.is_complete(),
            "expected repair to close every gap, remaining: {:?}",
            outcome.after.gaps
        );
    }
}
