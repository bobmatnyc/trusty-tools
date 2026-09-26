//! The PM delegation rules, applied by session profile (#8453).
//!
//! Why: a supervisor session acts directly by design. It used to lift the PM
//! delegation rules with `TRUSTY_MPM_PM_UNRESTRICTED=1`, which also lifted
//! every absolute guard. The owner ruling is that the profile defines the
//! supervisor's guard behaviour: the delegation rules do not bind it, and the
//! destructive-command and worktree-discipline guards still do.
//! What: [`verdict`] is the last rule `tm hook --pm-guard` asks. It
//! runs after every absolute guard, so exempting a supervisor here exempts it
//! from the delegation rules only.
//! Test: `a_supervisor_session_is_not_bound_by_the_pm_delegation_rules`,
//! `a_pm_session_is_still_bound_by_the_pm_delegation_rules`,
//! `an_undecidable_profile_keeps_the_pm_guards`,
//! `a_supervisor_session_is_still_refused_a_destructive_delete`
//! (`tests/tm_hook_pm_guard_supervisor_8453.rs`).

use std::path::Path;

use trusty_mpm::core::session_profile;

/// The PM delegation verdict for a tool call, or `None` for a supervisor.
///
/// Why: see the module doc.
/// What: resolves the session's profile from `CLAUDE_PROJECT_DIR` (the
/// launch directory Claude Code exports to hooks), falling back to
/// `hook_cwd`. A supervisor → `None` (allow). Otherwise, including every case
/// where the profile cannot be read, the unchanged
/// [`crate::commands::pm_guard::evaluate_tool`] verdict.
/// Test: `a_supervisor_session_is_not_bound_by_the_pm_delegation_rules`,
/// `an_undecidable_profile_keeps_the_pm_guards`.
pub(crate) fn verdict(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    hook_cwd: &Path,
) -> Option<&'static str> {
    let project_dir =
        session_profile::hook_project_dir(std::env::var_os("CLAUDE_PROJECT_DIR"), hook_cwd);
    if session_profile::resolve(&project_dir).is_supervisor() {
        return None;
    }
    crate::commands::pm_guard::evaluate_tool(tool_name, tool_input)
}
