//! The built-in tools a tm-launched PM does not load (#8992).
//!
//! Why: a PM session never uses Claude Code's Artifact family, `Workflow` or
//! `DesignSync`, yet their schemas ride in every request. The owner's method
//! (#8992, 2026-10-01): turn Artifact off with `CLAUDE_CODE_DISABLE_ARTIFACT=1`, deny the other optional built-ins with
//! `--disallowedTools`, and KEEP the `Cron*` tools, which session resume uses.
//! The supervisor profile is not a PM and keeps its full tool set.
//! What: [`trim_env`] and [`trim_args`] return the variable and the argv a
//! launch of `profile` adds; [`stamped_profile`] reads the profile a launch's
//! own #8453 stamp names, for builders that receive the stamp rather than the
//! profile.
//! Test: `crates/trusty-mpm/src/core/pm_tool_trim_tests.rs`.

use crate::core::session_profile::{SESSION_PROFILE_ENV, SessionProfile};

/// The variable that turns Claude Code's Artifact tools off.
///
/// Why: one switch removes `Artifact`, `ArtifactComments` and `ArtifactData`
/// together; denying the names one by one would miss a new family member.
/// What: `"CLAUDE_CODE_DISABLE_ARTIFACT"`, assigned `"1"`.
/// Test: `a_pm_launch_disables_the_artifact_tools`.
pub const DISABLE_ARTIFACT_ENV: &str = "CLAUDE_CODE_DISABLE_ARTIFACT";

/// The Claude Code flag that removes named tools from the session.
pub const DISALLOWED_TOOLS_FLAG: &str = "--disallowedTools";

/// The optional built-ins a PM launch denies, as Claude Code permission rules.
///
/// Why: these are the #8992 tools a PM never calls. `mcp__claude-in-chrome`
/// is deliberately absent: `web-qa` drives the browser and a session-level deny
/// reaches subagents. `CronCreate`/`CronDelete`/`CronList` are also absent:
/// session resume schedules through them.
/// What: rule strings, joined with `,` into one argv value by [`trim_args`].
/// Test: `a_pm_launch_denies_the_optional_built_ins`,
/// `the_cron_tools_are_never_denied`, `the_chrome_tools_are_never_denied`.
pub const DISALLOWED_TOOLS: [&str; 2] = ["Workflow", "DesignSync"];

/// The environment a launch of `profile` adds.
///
/// What: `CLAUDE_CODE_DISABLE_ARTIFACT=1` for a PM; nothing for a supervisor.
/// Test: `a_pm_launch_disables_the_artifact_tools`,
/// `a_supervisor_launch_keeps_every_tool`.
pub fn trim_env(profile: SessionProfile) -> Vec<(String, String)> {
    match profile {
        SessionProfile::Pm => vec![(DISABLE_ARTIFACT_ENV.to_owned(), "1".to_owned())],
        SessionProfile::Supervisor => Vec::new(),
    }
}

/// The argv a launch of `profile` adds.
///
/// Why: ONE value token, comma-joined. `--disallowedTools` is variadic, so a
/// space-separated list would also swallow any bare word that followed it.
/// What: `["--disallowedTools", "Workflow,DesignSync"]`
/// for a PM; empty for a supervisor.
/// Test: `a_pm_launch_denies_the_optional_built_ins`,
/// `a_supervisor_launch_keeps_every_tool`.
pub fn trim_args(profile: SessionProfile) -> Vec<String> {
    match profile {
        SessionProfile::Pm => vec![DISALLOWED_TOOLS_FLAG.to_owned(), DISALLOWED_TOOLS.join(",")],
        SessionProfile::Supervisor => Vec::new(),
    }
}

/// The profile a launch's #8453 stamp names, from its assigned variables.
///
/// Why: the CLI builders receive the stamp inside `mcp_env`, not the profile,
/// and the stamp is what `tm hook --pm-guard` reads in the session, so keying
/// the trim on it keeps the tool set and the guard on one profile.
/// What: the LAST `TRUSTY_MPM_SESSION_PROFILE` assignment; `supervisor` gives
/// [`SessionProfile::Supervisor`], anything else or no stamp gives the default
/// [`SessionProfile::Pm`].
/// Test: `the_stamp_decides_the_profile`.
pub fn stamped_profile(env: &[(String, String)]) -> SessionProfile {
    match env
        .iter()
        .rev()
        .find(|(name, _)| name == SESSION_PROFILE_ENV)
    {
        // #9755: either stamp value is the Architect.
        Some((_, id)) if crate::core::architect_compat::is_architect_stamp(Some(id.as_ref())) => {
            SessionProfile::Supervisor
        }
        _ => SessionProfile::Pm,
    }
}

#[cfg(test)]
#[path = "pm_tool_trim_tests.rs"]
mod tests;
