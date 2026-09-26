//! The PM delegation rules, applied by session profile (#8453).
//!
//! Why: a supervisor session acts directly by design. It used to lift the PM
//! delegation rules with `TRUSTY_MPM_PM_UNRESTRICTED=1`, which also lifted
//! every absolute guard. The owner ruling is that the profile defines the
//! supervisor's guard behaviour: the delegation rules do not bind it, and the
//! destructive-command and worktree-discipline guards still do.
//! What: [`verdict`] is the last rule `tm hook --pm-guard` asks. It
//! runs after every absolute guard, so exempting a supervisor here exempts it
//! from the delegation rules only. The exemption needs the launch stamp, the
//! user-level allowlist and the project file to agree
//! ([`session_profile::hook_profile`]); a session cannot grant it to itself
//! by editing `.trusty-mpm.toml`.
//! Test: `a_supervisor_session_is_not_bound_by_the_pm_delegation_rules`,
//! `a_project_only_switch_keeps_the_pm_delegation_rules`,
//! `a_stamp_without_the_file_keeps_the_pm_delegation_rules`,
//! `every_absolute_guard_still_binds_a_supervisor`
//! (`tests/tm_hook_pm_guard_supervisor_8453.rs`).

use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile;

/// The PM delegation verdict for a tool call, or `None` for a supervisor.
///
/// Why: see the module doc.
/// What: [`session_profile::hook_profile`] over the launch stamp
/// (`TRUSTY_MPM_SESSION_PROFILE`), `CLAUDE_PROJECT_DIR` and the user-level
/// config, read only when the stamp says supervisor. A supervisor → `None`
/// (allow). Otherwise — including every case where any input is missing or
/// unreadable — the unchanged [`crate::commands::pm_guard::evaluate_tool`]
/// verdict.
/// Test: `a_supervisor_session_is_not_bound_by_the_pm_delegation_rules`,
/// `a_project_only_switch_keeps_the_pm_delegation_rules`.
pub(crate) fn verdict(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> Option<&'static str> {
    let profile = session_profile::hook_profile(
        std::env::var_os(session_profile::SESSION_PROFILE_ENV),
        std::env::var_os(session_profile::PROJECT_DIR_ENV),
        MpmConfig::load_default,
    );
    if profile.is_supervisor() {
        return None;
    }
    crate::commands::pm_guard::evaluate_tool(tool_name, tool_input)
}
