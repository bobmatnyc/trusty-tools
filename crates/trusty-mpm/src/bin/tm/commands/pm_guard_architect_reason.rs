//! Why a call is not the Architect's main thread (#8878 PR-I).
//!
//! Why: the Architect identity is a chain of checks, and a deny that says
//! only "not the Architect" leaves the operator guessing which one failed.
//! Plan comment 5899494811, section 6 "PR-I": the deny text, the audit line
//! and `tm fleet status` name the FIRST check that failed. The verdict stays
//! the one `is_architect_main_thread` gave before.
//! What: [`architect_main_thread`] runs the checks in the old order and
//! returns the first failure as a [`NotArchitect`]. [`session_binding`] runs
//! the same checks for `tm fleet status`, which has no hook payload.
//! [`with_identity`] appends the reason to a deny. No reason carries a PID,
//! a start time, an error detail or an environment value.
//! FAIL-CLOSED: every arm is `Err`, so every caller that denies on `Err`
//! denies on each arm, including a process-table or record read error.
//! Test: `pm_guard_architect_reason_tests.rs`.

use std::ffi::OsStr;

use serde_json::Value;
use trusty_mpm::core::architect_launch::{self, LaunchRefusal};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile::{self, NOT_ALLOW_LISTED, SUPERVISOR_PROFILE_ID};
use trusty_mpm::core::twin_identity::{ThreadKind, thread_kind};

use crate::commands::pm_guard_trust_anchor::{ANCHOR_ROOT, HookEnv};

/// The first Architect identity check a call failed.
///
/// Why: one distinct reason per check, so a deny names what to fix.
/// What: the thread, profile and location arms, then [`LaunchRefusal`] for
/// the process-bound half.
/// Test: `each_identity_failure_names_its_own_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotArchitect {
    /// The payload does not establish the session's main thread.
    UnknownThread,
    /// The call comes from a subagent.
    Subagent,
    /// `TRUSTY_MPM_SESSION_PROFILE` is not `supervisor`.
    NoSupervisorStamp,
    /// `CLAUDE_PROJECT_DIR` is unset or empty.
    NoProjectDir,
    /// The launch directory's `.trusty-mpm.toml` does not ask for the supervisor.
    ProfileNotRequested,
    /// The launch directory is not on the `[supervisor] projects` allowlist.
    NotAllowListed,
    /// The home directory is unknown.
    UnknownHome,
    /// The process-bound half failed.
    Launch(LaunchRefusal),
}

impl std::fmt::Display for NotArchitect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownThread => {
                "the payload does not establish the session's main thread (no `session_id`, or \
                 a malformed `agent_id`)"
            }
            Self::Subagent => "the call comes from a subagent",
            Self::NoSupervisorStamp => {
                "the session has no `supervisor` launch stamp (`TRUSTY_MPM_SESSION_PROFILE`)"
            }
            Self::NoProjectDir => "`CLAUDE_PROJECT_DIR` is unset or empty",
            Self::ProfileNotRequested => {
                "the launch directory's `.trusty-mpm.toml` does not request `profile = \
                 \"supervisor\"`"
            }
            Self::NotAllowListed => {
                "the launch directory is not in `[supervisor] projects` of \
                 ~/.trusty-mpm/config.toml"
            }
            Self::UnknownHome => "the home directory is unknown",
            Self::Launch(refusal) => return refusal.fmt(f),
        })
    }
}

/// `reason`, followed by the identity check that failed.
///
/// Why: the deny text and the audit line are the same string, so one
/// suffix names the reason in both.
/// Test: `a_deny_names_the_failed_identity_check`.
pub(crate) fn with_identity(reason: String, why: NotArchitect) -> String {
    format!("{reason} Identity: this call is not the Architect's main thread: {why}.")
}

/// Whether the call is the Architect's main thread; `Err` names why not.
///
/// Why: ruling A (#8878) binds the Architect to the `claude` `tm fleet init`
/// launched; a subagent of it is an agent. PR-I adds the reason.
/// What: [`thread_kind`] must be [`ThreadKind::Main`], then
/// [`session_binding_checks`]. The order is the old bool's, so the first
/// failure is the one that decided it.
/// Test: `each_identity_failure_names_its_own_reason`,
/// `the_reason_verdict_equals_the_bool_verdict`.
pub(crate) fn architect_main_thread(
    payload: &Value,
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> Result<(), NotArchitect> {
    match thread_kind(payload, env.sub_agent) {
        ThreadKind::Main => {}
        ThreadKind::Subagent => return Err(NotArchitect::Subagent),
        ThreadKind::Unknown => return Err(NotArchitect::UnknownThread),
    }
    session_binding_checks(env, config)
}

/// Whether the calling process is bound as the Architect, for `tm fleet status`.
///
/// Why: the CLI has no hook payload, so the thread is known only from
/// `CLAUDE_MPM_SUB_AGENT`.
/// What: [`NotArchitect::Subagent`] when that is set, else the same checks
/// as [`architect_main_thread`], so a failure prints the same reason.
/// Test: `fleet_status_names_why_this_session_is_not_bound`.
pub(crate) fn session_binding(
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> Result<(), NotArchitect> {
    if env.sub_agent {
        return Err(NotArchitect::Subagent);
    }
    session_binding_checks(env, config)
}

/// The profile, location and process checks, cheapest first.
///
/// What: the steps of `session_profile::hook_profile` one at a time — the
/// stamp, `CLAUDE_PROJECT_DIR`, then `config()` and the project's request
/// and allowlist, logging a refused request as `session_profile::resolve`
/// does — then the home, then [`architect_launch::check_launched_architect`].
fn session_binding_checks(
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> Result<(), NotArchitect> {
    if env.stamp.as_deref() != Some(OsStr::new(SUPERVISOR_PROFILE_ID)) {
        return Err(NotArchitect::NoSupervisorStamp);
    }
    let project = session_profile::hook_project_dir(env.project_dir.clone())
        .ok_or(NotArchitect::NoProjectDir)?;
    let config = config();
    if !session_profile::requested(&project).is_supervisor() {
        return Err(NotArchitect::ProfileNotRequested);
    }
    if !session_profile::is_allow_listed(&project, &config.supervisor) {
        tracing::warn!(project = %project.display(), "{NOT_ALLOW_LISTED}");
        return Err(NotArchitect::NotAllowListed);
    }
    let home = env.home.as_deref().ok_or(NotArchitect::UnknownHome)?;
    architect_launch::check_launched_architect(&home.join(ANCHOR_ROOT), &project, || {
        env.claude.get()
    })
    .map_err(NotArchitect::Launch)
}

#[cfg(test)]
#[path = "pm_guard_architect_reason_tests.rs"]
pub(crate) mod tests;
