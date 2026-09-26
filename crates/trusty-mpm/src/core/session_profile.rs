//! The session instruction profile: a delegating PM, or a fleet supervisor (#8453).
//!
//! Why: a supervisor session ran on the full PM instructions and the
//! `trusty-mpm` output style, then contradicted them with CLAUDE.md override
//! blocks and the all-or-nothing `TRUSTY_MPM_PM_UNRESTRICTED=1`. An output
//! style cannot be overridden from CLAUDE.md, so its "work only through
//! delegation" floor reached the supervisor anyway. The owner ruling
//! (2026-09-23) is a separate supervisor profile with its own instructions,
//! output style, guard behaviour and model.
//!
//! What: a session is a supervisor only when the operator allow-listed the
//! project in the USER-level `~/.trusty-mpm/config.toml` (`[supervisor]
//! projects`) AND the project's committed `.trusty-mpm.toml` says
//! `profile = "supervisor"` ([`resolve`]). A project can write its own
//! `.trusty-mpm.toml`, so the file alone never exempts it (#3981). A launch
//! resolves the profile once and stamps it into the spawned `claude`'s
//! environment as [`SESSION_PROFILE_ENV`] ([`launch_env`]); `tm hook
//! --pm-guard` lifts the PM delegation rules only when the stamp AND both
//! conditions still say supervisor ([`hook_profile`]).
//!
//! FAIL-CLOSED: anything that prevents a positive "supervisor" answer — an
//! absent file, an unreadable or malformed file, an absent key, an unknown
//! value, a project missing from the allowlist, a missing or different stamp —
//! resolves to [`SessionProfile::Pm`], the full PM profile with every PM
//! guard. The supervisor profile is never partial: its prompt is compiled in
//! and a non-empty composition is asserted by test.
//!
//! Test: `session_profile_tests.rs`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::core::config::MpmConfig;
use crate::core::project_config::ProjectLevelConfig;

/// `profile` value selecting the delegating PM (the default).
pub const PM_PROFILE_ID: &str = "pm";

/// `profile` value selecting the fleet supervisor.
pub const SUPERVISOR_PROFILE_ID: &str = "supervisor";

/// Output-style id a supervisor session runs on.
pub const SUPERVISOR_OUTPUT_STYLE_ID: &str = "trusty-mpm-supervisor";

/// The launch stamp: the profile a session was launched as (#8453).
///
/// Why: the guard used to re-read `.trusty-mpm.toml` on every tool call, and
/// a session can write that file, so a PM could promote itself mid-session.
/// What: set on the spawned `claude` by every tm launch path that composes
/// the prompt; Claude Code passes it on to its hooks. Value: a profile id.
pub const SESSION_PROFILE_ENV: &str = "TRUSTY_MPM_SESSION_PROFILE";

/// The launch directory Claude Code exports to every hook.
pub const PROJECT_DIR_ENV: &str = "CLAUDE_PROJECT_DIR";

/// Why a project that asks for the supervisor profile did not get it.
pub const NOT_ALLOW_LISTED: &str = "project requests the supervisor profile but is not \
     allow-listed in ~/.trusty-mpm/config.toml (`[supervisor] projects`); running the PM profile";

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

    /// The profile id, as written in `.trusty-mpm.toml` and the launch stamp.
    pub fn id(self) -> &'static str {
        match self {
            SessionProfile::Pm => PM_PROFILE_ID,
            SessionProfile::Supervisor => SUPERVISOR_PROFILE_ID,
        }
    }
}

/// `[supervisor]` in the user-level `~/.trusty-mpm/config.toml` (#8453).
///
/// Why: the operator's grant, kept outside every project's write boundary so
/// a project cannot exempt itself from the PM rules (#3981 won't-do ruling).
/// What: `projects` — absolute project paths allowed to run the supervisor
/// profile, matched after canonicalization. A relative entry matches nothing.
/// Test: `an_allowlist_entry_for_another_path_does_not_match`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SupervisorConfig {
    /// Absolute paths of the projects allowed to run the supervisor profile.
    pub projects: Vec<PathBuf>,
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

/// The profile a parsed project config asks for.
///
/// Why: separates the decision from the file read so the fail-closed rule is
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

/// The profile `project_dir`'s committed `.trusty-mpm.toml` asks for.
///
/// Why: condition (b) of [`resolve`], on its own, for the doctor and the
/// style warning.
/// What: [`crate::core::project_config::load_or_report`] (absent → silent,
/// unreadable or malformed → logged at `error`), then [`profile_from_config`].
/// Test: `a_malformed_config_falls_back_to_the_pm_profile`.
pub fn requested(project_dir: &Path) -> SessionProfile {
    profile_from_config(crate::core::project_config::load_or_report(project_dir).as_ref())
}

/// Whether the operator allow-listed `project_dir` for the supervisor profile.
///
/// Why: condition (a) of [`resolve`]. Canonical paths, so a symlink, a
/// trailing slash or `/var` versus `/private/var` cannot decide the answer.
/// What: `true` when some absolute `allowed.projects` entry canonicalizes to
/// the canonical `project_dir`. A path that cannot be canonicalized matches
/// nothing.
/// Test: `an_allowlist_entry_for_another_path_does_not_match`,
/// `a_symlinked_allowlist_entry_is_canonicalized`.
pub fn is_allow_listed(project_dir: &Path, allowed: &SupervisorConfig) -> bool {
    let Ok(project) = std::fs::canonicalize(project_dir) else {
        return false;
    };
    allowed
        .projects
        .iter()
        .filter(|entry| entry.is_absolute())
        .filter_map(|entry| std::fs::canonicalize(entry).ok())
        .any(|entry| entry == project)
}

/// The refusal reason when `project_dir` asks for the supervisor profile and
/// does not get it; `None` otherwise.
///
/// Why: `tm doctor` names the reason; the launch logs it.
/// What: [`NOT_ALLOW_LISTED`] when (b) holds and (a) does not.
/// Test: `the_doctor_reason_names_the_missing_allowlist_entry`.
pub fn refusal(project_dir: &Path, config: &MpmConfig) -> Option<&'static str> {
    (requested(project_dir).is_supervisor() && !is_allow_listed(project_dir, &config.supervisor))
        .then_some(NOT_ALLOW_LISTED)
}

/// The profile a session launched in `project_dir` runs.
///
/// Why: the one launch-time selector, so a project cannot promote itself.
/// What: [`SessionProfile::Supervisor`] only when `project_dir` is
/// allow-listed in `config` (the user-level config) AND [`requested`] says
/// supervisor. Otherwise [`SessionProfile::Pm`]; a refused request is logged
/// at `warn` with [`NOT_ALLOW_LISTED`].
/// Test: `a_supervisor_config_selects_the_supervisor_profile`,
/// `a_project_only_switch_stays_pm`.
pub fn resolve(project_dir: &Path, config: &MpmConfig) -> SessionProfile {
    if !requested(project_dir).is_supervisor() {
        return SessionProfile::Pm;
    }
    if is_allow_listed(project_dir, &config.supervisor) {
        return SessionProfile::Supervisor;
    }
    tracing::warn!(project = %project_dir.display(), "{NOT_ALLOW_LISTED}");
    SessionProfile::Pm
}

/// [`resolve`] against the operator's `~/.trusty-mpm/config.toml`.
///
/// Why: the prompt composer and the style report have no config in hand.
/// What: [`MpmConfig::load_default`], then [`resolve`].
/// Test: `a_project_only_switch_stays_pm`.
pub fn resolve_ambient(project_dir: &Path) -> SessionProfile {
    resolve(project_dir, &MpmConfig::load_default())
}

/// The launch stamp for `profile`: `(SESSION_PROFILE_ENV, id)`.
///
/// Why: the PM stamp is written too, so an inherited supervisor stamp can
/// never reach a session launched as a PM.
/// Test: `the_launch_stamp_names_the_profile`.
pub fn launch_env(profile: SessionProfile) -> (String, String) {
    (SESSION_PROFILE_ENV.to_owned(), profile.id().to_owned())
}

/// The model a session of `profile` launches on.
///
/// Why: `tm launch` passes `--model`, which outranks the project settings, so
/// it must name the supervisor's model too.
/// What: [`SUPERVISOR_MODEL`] for a supervisor; otherwise the PM model from
/// [`crate::core::model_inject::resolve_pm_model`], unchanged.
/// Test: `the_supervisor_model_is_the_opus_tier_alias`.
pub fn launch_model(profile: SessionProfile, config: &MpmConfig) -> String {
    match profile {
        SessionProfile::Supervisor => SUPERVISOR_MODEL.to_string(),
        SessionProfile::Pm => crate::core::model_inject::resolve_pm_model(config, None),
    }
}

/// The directory a `PreToolUse` hook call's session was launched in.
///
/// Why: the hook's `cwd` follows the session's `cd`, so a supervisor that
/// changes into a watched project would be judged by that project's file.
/// Claude Code exports `CLAUDE_PROJECT_DIR` — the launch directory — to every
/// hook.
/// What: `project_dir_env` when set and non-empty; otherwise `None`, which
/// the caller treats as the PM profile. The payload `cwd` is never used.
/// Test: `the_hook_reads_only_the_launch_directory`.
pub fn hook_project_dir(project_dir_env: Option<OsString>) -> Option<PathBuf> {
    project_dir_env
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// The profile `tm hook --pm-guard` applies to a tool call.
///
/// Why: a stamp alone could come from an older file state, and the files
/// alone can be edited by the session; only all three together are the
/// operator's current grant.
/// What: [`SessionProfile::Supervisor`] only when `stamp` is exactly
/// [`SUPERVISOR_PROFILE_ID`], `project_dir_env` names a directory, and
/// [`resolve`] over `config()` says supervisor. `config` is called only after
/// the stamp matches, so a PM session reads no file.
/// Test: `the_hook_needs_the_stamp_the_allowlist_and_the_file`.
pub fn hook_profile(
    stamp: Option<OsString>,
    project_dir_env: Option<OsString>,
    config: impl FnOnce() -> MpmConfig,
) -> SessionProfile {
    if stamp.as_deref() != Some(std::ffi::OsStr::new(SUPERVISOR_PROFILE_ID)) {
        return SessionProfile::Pm;
    }
    let Some(project_dir) = hook_project_dir(project_dir_env) else {
        return SessionProfile::Pm;
    };
    resolve(&project_dir, &config())
}

#[cfg(test)]
#[path = "session_profile_tests.rs"]
mod tests;
