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
//! `validate_stale_broken_frontmatter_is_a_gap` (issue #3556),
//! `validate_well_formed_agent_is_not_flagged` (issue #3556),
//! `repair_closes_gaps_on_incomplete_workspace`,
//! `repair_is_a_noop_on_already_complete_workspace`; the expected-set
//! resolution itself is covered by `expected_set`'s own test module.

use std::path::Path;

use trusty_agents_common::agents::frontmatter::validate_frontmatter;

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
    /// A deployed agent (`<name>.md`) is present but its frontmatter fails a
    /// strict YAML parse (issue #3556) — e.g. a stale copy predating the
    /// quote-on-emit fix in `trusty-agents-common::agents::builder`, still
    /// carrying an unquoted scalar that contains a colon. Deployed but
    /// unparseable is worse than missing: the agent silently fails to load at
    /// `tagent`/Claude Code runtime instead of surfacing here at deploy time.
    AgentFrontmatterInvalid(String, String),
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
            Self::AgentFrontmatterInvalid(name, detail) => format!(
                "agent `{name}` is deployed but its frontmatter is not valid YAML: {detail} \
                 — run `tm install --reset-agents` (or delete the file and redeploy) to refresh it"
            ),
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
        let path = target.join(format!("{name}.md"));
        if !path.is_file() {
            gaps.push(DeploymentGap::AgentMissing(name));
            continue;
        }
        // Issue #3556: presence on disk is not enough — a deployed copy can
        // predate the quote-on-emit composer fix and still carry frontmatter
        // a strict YAML parser rejects. Catch that here, at deploy-validation
        // time, rather than letting it fail silently at `tagent` runtime.
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Err(detail) = validate_frontmatter(&content)
        {
            gaps.push(DeploymentGap::AgentFrontmatterInvalid(name, detail));
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
#[path = "tests.rs"]
mod tests;
