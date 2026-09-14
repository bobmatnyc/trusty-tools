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
//! resolvable `outputStyle`, a configured `hooks` key, and — since #7849 —
//! every TOGGLE-DRIVEN hook group this project's current config asks for,
//! recomputed through the launch path's own builder
//! ([`crate::core::session_launch::project_hook_group_gaps`]). That last probe
//! is what lets `--repair` resync a flag that flipped after the settings file
//! was written; the repair itself is an in-place merge through the same locked
//! writer, not a re-run of the deploy pipeline.
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
//! `repair_is_a_noop_on_already_complete_workspace`,
//! `repair_closes_a_managed_tier_bundled_skill_gap` (issue #6586),
//! `a_stray_project_tier_bundled_skill_does_not_satisfy_completeness` (issue
//! #6586), `repair_adds_the_prompt_feedback_groups_after_the_flag_flips_on`,
//! `a_second_repair_leaves_the_settings_file_byte_identical`,
//! `repair_removes_a_stale_prompt_feedback_group_when_the_flag_is_off`,
//! `a_hook_group_repair_leaves_foreign_entries_untouched`,
//! `a_malformed_settings_file_is_a_fail_not_a_silent_pass`,
//! `a_hook_writer_failure_surfaces_rather_than_reporting_no_gaps`,
//! `an_unresolvable_hook_binary_is_an_incomplete_diagnostic_never_a_clean_report` (issue
//! #7849); the expected-set resolution itself is covered by `expected_set`'s
//! own test module.

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
    /// The skill manifest exists but could not be read — malformed, or an I/O
    /// failure (#5626). Distinct from missing: the ledger is there and its
    /// contents are undetermined, so nothing in the tier is attributable.
    SkillManifestCorrupt(String),
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
    /// A tm-owned hook group this project's CURRENT config asks for is absent
    /// from `.claude/settings.json` (#7849) — named by its event and the
    /// command the group would run.
    ProjectHookGroupMissing(String, String),
    /// `.claude/settings.json` carries a tm-owned hook group this project's
    /// current config no longer asks for (#7849) — a toggle that flipped back
    /// off, or a command naming a binary path that no longer resolves.
    ProjectHookGroupStale(String, String),
    /// The toggle-driven hook probe could not run, so whether the file carries
    /// the right groups is UNKNOWN (#7849) — carries the resolution error.
    ///
    /// Why this is a gap rather than a silent skip: an unknown answer reported
    /// as "complete" is the exact defect #7849 is about, moved one probe along
    /// and gated on binary resolution instead of on the toggle.
    ProjectHookDiagnosticIncomplete(String),
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
            Self::SkillManifestCorrupt(detail) => format!("skill manifest is corrupt: {detail}"),
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
            Self::ProjectHookGroupMissing(event, command) => format!(
                "settings.json has no `{event}` hook group running `{command}` — this \
                 project's config asks for it; run `tm validate --repair` to add it"
            ),
            Self::ProjectHookGroupStale(event, command) => format!(
                "settings.json carries a `{event}` hook group running `{command}` that this \
                 project's config no longer asks for; run `tm validate --repair` to remove it"
            ),
            Self::ProjectHookDiagnosticIncomplete(detail) => format!(
                "the toggle-driven hook-group check could not run, so this project's hook set \
                 is UNVERIFIED: {detail} — install tm (`cargo install trusty-mpm`) so the hook \
                 commands can be resolved, then re-run `tm validate --repair`"
            ),
        }
    }

    /// Whether this gap is a project-tier hook group the in-place resync closes.
    ///
    /// Why (#7849): a hook-group gap does not need the deploy pipeline. Running
    /// it would also rewrite the output style, the plugin allowlist and the
    /// statusline — work the resync does not need, and a fatal refusal it does
    /// not deserve.
    /// What: `true` for the three `#7849` variants, `false` for every other.
    /// [`Self::ProjectHookDiagnosticIncomplete`] is included so an unverifiable
    /// hook set still takes the resync branch: that branch calls the writer,
    /// which resolves the same binary and surfaces the same refusal as
    /// `repair_error`, instead of the deploy pipeline refusing for an unrelated
    /// reason.
    /// Test: `repair_adds_the_prompt_feedback_groups_after_the_flag_flips_on`,
    /// `an_unresolvable_hook_binary_is_an_incomplete_diagnostic_never_a_clean_report`.
    fn is_project_hook_group(&self) -> bool {
        matches!(
            self,
            Self::ProjectHookGroupMissing(..)
                | Self::ProjectHookGroupStale(..)
                | Self::ProjectHookDiagnosticIncomplete(..)
        )
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
    validate_workspace_with_exe(fw, None)
}

/// [`validate_workspace`] with the hook binary pinned by the caller.
///
/// Why (#7849): the toggle-driven hook probe compares the file against the
/// COMMANDS the writer would produce, and those carry the resolved stable
/// binary path. `resolve_stable_hook_exe` refuses a build-artifact binary, and
/// a test process is one — so without this seam the probe would silently skip
/// in every test that exercises it. Mirrors the seam
/// [`validate_and_repair_with_exe`] already carries for the repair half.
/// What: binds `hook_exe` into the hook-group probe and delegates to
/// [`validate_workspace_with_probe`]. Production callers pass `None` and keep
/// resolving the running installed binary.
/// Test: `repair_adds_the_prompt_feedback_groups_after_the_flag_flips_on`.
pub fn validate_workspace_with_exe(
    fw: &FrameworkPaths,
    hook_exe: Option<&Path>,
) -> ValidationReport {
    validate_workspace_with_probe(fw, &|fw, project_dir, settings| {
        crate::core::session_launch::project_hook_group_gaps(fw, project_dir, settings, hook_exe)
    })
}

/// The hook-group probe [`validate_workspace_with_probe`] runs.
///
/// Why: named so the `&dyn Fn` in two signatures cannot drift apart.
/// What: `(paths, project dir, parsed settings) -> gaps or the resolution
/// error`.
/// Test: see [`validate_workspace_with_probe`].
type HookGroupProbe<'a> = &'a dyn Fn(
    &FrameworkPaths,
    &Path,
    &serde_json::Value,
) -> Result<
    crate::core::session_launch::ProjectHookGroupGaps,
    crate::core::standalone::hooks::StableHookExeError,
>;

/// [`validate_workspace`] with the hook-group probe supplied by the caller.
///
/// Why (#7849): `resolve_stable_hook_exe` rescues a refused `exe_override` from
/// `$PATH` and from the well-known daemon directories, so the REFUSAL arm is
/// unreachable on any host that has `tm` installed — and pinning a path is
/// therefore not enough to test what happens when resolution fails. This seam
/// hands the probe's answer in directly, the same convention
/// [`crate::core::session_launch`]'s writers use for the identical reason.
/// What: the body of [`validate_workspace`], with `probe` replacing the bound
/// [`crate::core::session_launch::project_hook_group_gaps`] call.
/// Test: `an_unresolvable_hook_binary_is_an_incomplete_diagnostic_never_a_clean_report`.
pub(crate) fn validate_workspace_with_probe(
    fw: &FrameworkPaths,
    probe: HookGroupProbe<'_>,
) -> ValidationReport {
    let mut gaps = Vec::new();
    validate_agents(fw, &mut gaps);
    validate_skills(fw, &mut gaps);
    validate_settings(fw, probe, &mut gaps);
    ValidationReport { gaps }
}

/// Probe the deployed agent roster against the EXPECTED per-project set
/// (issue #2171 — see [`expected_set::expected_agent_stems`]).
fn validate_agents(fw: &FrameworkPaths, gaps: &mut Vec<DeploymentGap>) {
    // #4409: the deployed agent roster lives in the tm-managed config dir, not
    // in the workspace's `.claude/agents/`. Probing the workspace tier here
    // after the flip would report every expected agent missing and drive the
    // spawn/resume gate into a permanent repair loop.
    let target = fw.agent_deploy_dir();
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
    // #6586: the expected set is the BUNDLED roster, and bundled skills are
    // user-tier only now — so probing the project tier for them would report
    // every one of them missing on a workspace that is in fact complete.
    // `FrameworkPaths::skill_deploy_dir` is the ONE derivation of that tier;
    // `session_launch::skills` deploys through it too, so this probe and the
    // repair that closes its gaps read the same directory by construction.
    // A stray bundled copy left in a project's own `.claude/skills` by an older
    // binary is deliberately NOT consulted here — `tm doctor`'s
    // `skill_project_tier` reports it, and it must not satisfy completeness.
    let target = fw.skill_deploy_dir();
    let manifest_present = target.join(skill_manifest::SKILL_MANIFEST_FILE).is_file();
    if !manifest_present {
        gaps.push(DeploymentGap::SkillManifestMissing);
    }
    // #5626: a ledger that is present but unreadable is its own gap. Folding it
    // into `None` made `expected_skill_stems` fall back to the bundled roster
    // and report every skill as deployed-or-missing against a ledger nobody read.
    let manifest = match manifest_present.then(|| skill_manifest::SkillManifest::load(&target)) {
        None => None,
        Some(Ok(m)) => Some(m),
        Some(Err(e)) => {
            gaps.push(DeploymentGap::SkillManifestCorrupt(format!(
                "{}: {e}",
                target.join(skill_manifest::SKILL_MANIFEST_FILE).display()
            )));
            None
        }
    };
    for name in expected_skill_stems(fw, manifest.as_ref()) {
        if !target.join(&name).join("SKILL.md").is_file() {
            gaps.push(DeploymentGap::SkillMissing(name));
        }
    }
}

/// Probe `.claude/settings.json` for a resolvable `outputStyle`, a configured
/// `hooks` key, and (#7849) the toggle-driven hook groups this project's
/// current config asks for.
fn validate_settings(
    fw: &FrameworkPaths,
    probe: HookGroupProbe<'_>,
    gaps: &mut Vec<DeploymentGap>,
) {
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
        return;
    }

    // #7849: a non-empty `hooks` key was the whole test, so a toggle that
    // flipped on after this file was written was invisible — and a toggle that
    // flipped off left its group firing forever. The diff recomputes what the
    // launch-path writer would produce for THIS project and names both
    // directions. A file tm's project tier never provisioned yields nothing,
    // so a foreign project is never told to adopt tm's hooks.
    let project_dir = fw.claude_home_dir();
    // #7849 (fail-open check): an Err here means the expected commands could
    // not be resolved, so whether the file is right is UNKNOWN. Reporting
    // UNKNOWN as complete is the same defect this issue is about, gated on the
    // hook binary instead of on the toggle — so it becomes a gap the caller
    // sees, and `--repair` still attempts the resync (which surfaces the same
    // refusal as `repair_error`).
    let group_gaps = match probe(fw, &project_dir, &value) {
        Ok(group_gaps) => group_gaps,
        Err(e) => {
            gaps.push(DeploymentGap::ProjectHookDiagnosticIncomplete(
                e.to_string(),
            ));
            return;
        }
    };
    for (event, command) in group_gaps.missing {
        gaps.push(DeploymentGap::ProjectHookGroupMissing(event, command));
    }
    for (event, command) in group_gaps.stale {
        gaps.push(DeploymentGap::ProjectHookGroupStale(event, command));
    }
}

/// The outcome of [`validate_and_repair`].
///
/// Why: callers (the spawn/resume gate, `tm validate --repair`) need to know
/// not just the final state but whether a repair was attempted and whether it
/// actually closed the gaps found before it ran.
/// What: the pre-repair and post-repair [`ValidationReport`]s, whether the
/// repair succeeded, and the repair error (if any). Whether a repair was
/// ATTEMPTED is `!before.is_complete()` — an attempt runs exactly when the
/// pre-repair report had gaps.
/// Test: `repair_closes_gaps_on_incomplete_workspace`,
/// `repair_is_a_noop_on_already_complete_workspace`,
/// `repair_is_not_reported_when_the_pipeline_fails_fatally`.
#[derive(Debug)]
pub struct RepairOutcome {
    /// Validation result before any repair attempt.
    pub before: ValidationReport,
    /// Validation result after the repair attempt (equals `before` when no
    /// repair ran).
    pub after: ValidationReport,
    /// Whether the repair SUCCEEDED — it ran, the pipeline returned no error,
    /// and it left the workspace complete.
    ///
    /// #4781: this used to mean "an attempt ran", set unconditionally on the
    /// repair path, so a fatally-refused repair still reported `true`.
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
/// re-validates, and returns both reports plus any repair error. `repaired` is
/// derived from the re-validation, so it is `true` only when the repair left no
/// error and no gaps (#4781).
/// Test: `repair_closes_gaps_on_incomplete_workspace`,
/// `repair_is_a_noop_on_already_complete_workspace`,
/// `repair_is_not_reported_when_the_pipeline_fails_fatally`.
pub fn validate_and_repair(
    fw: &FrameworkPaths,
    workspace: &Path,
    repo_url: Option<&str>,
) -> RepairOutcome {
    validate_and_repair_with_exe(fw, workspace, repo_url, None)
}

/// [`validate_and_repair`] with the hook binary pinned by the caller.
///
/// Why (#7244): the repair pipeline writes the project's hooks, and that write
/// refuses a build-artifact binary. A test process IS one, and a CI runner has
/// no installed `tm`, so `HooksMissing` stayed a gap and the repair could never
/// report complete. Pinning the path keeps the assertion about the pipeline.
/// What: the body of [`validate_and_repair`], forwarding `hook_exe` to
/// [`crate::core::session_launch::prepare_session_with_repo_url_and_exe`].
/// Test: `repair_closes_gaps_on_incomplete_workspace`.
pub fn validate_and_repair_with_exe(
    fw: &FrameworkPaths,
    workspace: &Path,
    repo_url: Option<&str>,
    hook_exe: Option<&Path>,
) -> RepairOutcome {
    let before = validate_workspace_with_exe(fw, hook_exe);
    if before.is_complete() {
        return RepairOutcome {
            after: before.clone(),
            before,
            repaired: false,
            repair_error: None,
        };
    }

    // #7849: a hook-group-only gap is resynced in place through the SAME locked
    // writer the launch path uses, not by re-running the deploy pipeline — that
    // pipeline closes this gap too (it calls the same writer), but it also
    // rewrites the output style, the plugin allowlist and the statusline, and
    // it can refuse fatally for a reason a hook resync has nothing to do with.
    // An Err here is reported: a repair that could not write must never leave
    // the caller reading "no gaps found" (the fail-open shape this issue is).
    if before.gaps.iter().all(DeploymentGap::is_project_hook_group) {
        let repair_error =
            crate::core::session_launch::ensure_project_hooks_with(fw, workspace, hook_exe)
                .err()
                .map(|e| e.to_string());
        let after = validate_workspace_with_exe(fw, hook_exe);
        let repaired = repair_error.is_none() && after.is_complete();
        return RepairOutcome {
            before,
            after,
            repaired,
            repair_error,
        };
    }

    let repair_error = match crate::core::session_launch::prepare_session_with_repo_url_and_exe(
        fw, workspace, repo_url, hook_exe,
    ) {
        Ok(report) if !report.roster_errors.is_empty() => Some(report.roster_errors.join("; ")),
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    };

    let after = validate_workspace_with_exe(fw, hook_exe);
    // #4781: `repaired` is a statement about the OUTCOME, never about the
    // attempt — a fatally-refused repair left the workspace exactly as broken
    // as it found it.
    let repaired = repair_error.is_none() && after.is_complete();
    RepairOutcome {
        before,
        after,
        repaired,
        repair_error,
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
