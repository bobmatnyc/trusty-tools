//! The session instruction profile: a delegating PM, or a fleet supervisor (#8453).
//!
//! Why: a supervisor session ran on the full PM instructions and the
//! `trusty-mpm` output style, then contradicted them with CLAUDE.md override
//! blocks and the all-or-nothing `TRUSTY_MPM_PM_UNRESTRICTED=1`. An output
//! style cannot be overridden from CLAUDE.md, so its "work only through
//! delegation" floor reached the supervisor anyway. The owner ruling
//! (2026-09-23) is a separate supervisor profile with its own instructions,
//! output style, guard behaviour and model.
//! What: [`resolve`] reads the committed `.trusty-mpm.toml` `profile` key.
//! [`supervisor_prompt`] composes the bundled supervisor sections,
//! [`SUPERVISOR_OUTPUT_STYLE_ID`] names its style and [`SUPERVISOR_MODEL`] its
//! model. The prompt composer, the style selector, the launch model and
//! `tm hook --pm-guard` each branch on [`resolve`].
//!
//! FAIL-OPEN: anything that prevents a positive "supervisor" answer — an
//! absent file, an unreadable or malformed file, an absent key, an unknown
//! value — resolves to [`SessionProfile::Pm`], the full PM profile with every
//! PM guard. The supervisor profile is never partial: its prompt is compiled
//! in and a non-empty composition is asserted by test.
//!
//! Test: `session_profile_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::config::MpmConfig;
use crate::core::project_config::ProjectLevelConfig;

/// `profile` value selecting the delegating PM (the default).
pub const PM_PROFILE_ID: &str = "pm";

/// `profile` value selecting the fleet supervisor.
pub const SUPERVISOR_PROFILE_ID: &str = "supervisor";

/// Output-style id a supervisor session runs on.
pub const SUPERVISOR_OUTPUT_STYLE_ID: &str = "trusty-mpm-supervisor";

/// Model a supervisor session launches on: the Opus tier alias.
///
/// Why: owner ruling (2026-09-23) — the supervisor runs on the latest Opus,
/// never a pinned id. Claude Code resolves the bare alias to its current Opus.
/// What: `"opus"`, passed to `--model` and written to the project settings
/// verbatim. It never goes through `MpmConfig::expand_model_alias`, which maps
/// `opus` to a pinned id.
/// Test: `the_supervisor_model_is_the_opus_tier_alias`.
pub const SUPERVISOR_MODEL: &str = "opus";

/// Which instruction profile a session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionProfile {
    /// The delegating PM: PM instructions, PM output style, PM guards.
    #[default]
    Pm,
    /// The fleet supervisor: supervisor instructions and style, direct action.
    Supervisor,
}

impl SessionProfile {
    /// Whether this is the supervisor profile.
    pub fn is_supervisor(self) -> bool {
        self == SessionProfile::Supervisor
    }
}

/// The supervisor profile's sections, in composition order.
///
/// Why: one markdown file per kept item of #8453, so a review diff names the
/// item it changes; the order here is the order delivered.
/// What: `(name, authored text)`; `name` matches the file stem under
/// `assets/instructions/supervisor/`.
/// Test: `golden_supervisor_prompt`, `every_kept_item_reaches_the_supervisor_prompt`.
pub const SUPERVISOR_SECTIONS: [(&str, &str); 8] = [
    (
        "identity",
        include_str!("../assets/instructions/supervisor/identity.md"),
    ),
    (
        "hard-limits",
        include_str!("../assets/instructions/supervisor/hard-limits.md"),
    ),
    (
        "relay-protocol",
        include_str!("../assets/instructions/supervisor/relay-protocol.md"),
    ),
    (
        "evidence-labels",
        include_str!("../assets/instructions/supervisor/evidence-labels.md"),
    ),
    (
        "decisions",
        include_str!("../assets/instructions/supervisor/decisions.md"),
    ),
    (
        "monitoring",
        include_str!("../assets/instructions/supervisor/monitoring.md"),
    ),
    (
        "tool-priority",
        include_str!("../assets/instructions/supervisor/tool-priority.md"),
    ),
    (
        "prose-style",
        include_str!("../assets/instructions/supervisor/prose-style.md"),
    ),
];

/// The composed supervisor instructions.
///
/// Why: the text a supervisor session receives in place of the PM prompt.
/// What: each [`SUPERVISOR_SECTIONS`] body folded on its own (authoring
/// comments removed, as the PM composer does), trimmed, and joined with a
/// blank line. Pure and deterministic.
/// Test: `golden_supervisor_prompt`, `the_supervisor_prompt_is_never_empty`.
pub fn supervisor_prompt() -> String {
    SUPERVISOR_SECTIONS
        .iter()
        .map(|(_, body)| crate::core::instruction_fold::fold_block(body.trim()))
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The profile a parsed project config selects.
///
/// Why: separates the decision from the file read so the fail-open rule is
/// testable without a filesystem.
/// What: `Some(cfg)` with `profile = "supervisor"` (case and surrounding
/// whitespace ignored) → [`SessionProfile::Supervisor`]. Everything else →
/// [`SessionProfile::Pm`]; an unknown value is logged at `warn`.
/// Test: `an_unknown_profile_value_falls_back_to_the_pm_profile`.
pub fn profile_from_config(config: Option<&ProjectLevelConfig>) -> SessionProfile {
    let Some(raw) = config.and_then(|c| c.profile.as_deref()) else {
        return SessionProfile::Pm;
    };
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        SUPERVISOR_PROFILE_ID => SessionProfile::Supervisor,
        PM_PROFILE_ID | "" => SessionProfile::Pm,
        _ => {
            tracing::warn!(
                profile = raw,
                "unknown `.trusty-mpm.toml` profile; using the `{PM_PROFILE_ID}` profile \
                 (valid: `{PM_PROFILE_ID}`, `{SUPERVISOR_PROFILE_ID}`)"
            );
            SessionProfile::Pm
        }
    }
}

/// The profile a session launched in `project_dir` runs.
///
/// Why: the one selector the prompt composer, the style selector, the launch
/// model and the guard all call, so they cannot disagree.
/// What: [`crate::core::project_config::load_or_report`] (absent → silent,
/// unreadable or malformed → logged at `error`), then
/// [`profile_from_config`]. Fails open to [`SessionProfile::Pm`].
/// Test: `a_malformed_config_falls_back_to_the_pm_profile`,
/// `a_supervisor_config_selects_the_supervisor_profile`.
pub fn resolve(project_dir: &Path) -> SessionProfile {
    profile_from_config(crate::core::project_config::load_or_report(project_dir).as_ref())
}

/// The model a session launched in `project_dir` runs on.
///
/// Why: `tm launch` passes `--model`, which outranks the project settings, so
/// it must name the supervisor's model too.
/// What: [`SUPERVISOR_MODEL`] for a supervisor; otherwise the PM model from
/// [`crate::core::model_inject::resolve_pm_model`], unchanged.
/// Test: `the_supervisor_model_is_the_opus_tier_alias`.
pub fn launch_model(project_dir: &Path, config: &MpmConfig) -> String {
    match resolve(project_dir) {
        SessionProfile::Supervisor => SUPERVISOR_MODEL.to_string(),
        SessionProfile::Pm => crate::core::model_inject::resolve_pm_model(config, None),
    }
}

/// The project directory a `PreToolUse` hook call belongs to.
///
/// Why: the hook's `cwd` follows the session's `cd`, so a supervisor that
/// changes into a watched project would be judged by that project's profile.
/// Claude Code exports `CLAUDE_PROJECT_DIR` — the directory the session was
/// launched in — to every hook.
/// What: `project_dir_env` when set and non-empty, else `hook_cwd`.
/// Test: `the_hook_reads_the_launch_directory_before_the_cwd`.
pub fn hook_project_dir(project_dir_env: Option<std::ffi::OsString>, hook_cwd: &Path) -> PathBuf {
    project_dir_env
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| hook_cwd.to_path_buf())
}

#[cfg(test)]
#[path = "session_profile_tests.rs"]
mod tests;
