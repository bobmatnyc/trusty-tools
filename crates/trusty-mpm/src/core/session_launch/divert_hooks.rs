//! The `PreToolUse` bulk-read diversion hook groups (#6887).
//!
//! Why: a session that `Read`s a 2000-line file pays for every one of those
//! lines in its own context on its own expensive model. Spotify's `shunt`
//! plugin intercepts that read with a `PreToolUse` hook and steers the agent to
//! a cheap worker instead; the #6882 POC measured -45% cost and -54% output
//! tokens on this repo. This module owns the two hook groups that wire the
//! interception, kept OUT of
//! [`super::project_hooks`] and [`super::settings`] because both sit close to
//! the 500-SLOC production cap and because the identity predicate, the command
//! string, and the group shape are one concern that should move together.
//!
//! What: [`divert_hook_groups`] builds the two `PreToolUse` handler groups
//! (matcher `"Read"`, matcher `"Bash"`) invoking `<abs-path> hook
//! --divert-check`; [`is_divert_hook_command`] is the matching identity
//! predicate [`super::project_hooks::is_project_managed_hook_command`] folds in
//! so the replace-by-identity strip removes a stale entry when `[divert]
//! enabled` flips back to false.
//! Test: `project_hooks_tests.rs`
//! (`project_managed_hook_additions_includes_divert_when_enabled`,
//! `project_managed_hook_additions_omits_divert_when_disabled`,
//! `is_project_managed_hook_command_recognises_divert_check`,
//! `write_project_hooks_strips_stale_divert_when_disabled`).

use super::settings::resolve_statusline_binary;

/// The `tm hook` sub-flag the diversion check is invoked with.
///
/// Why: the identity predicate and the command builder must agree on one
/// spelling, or the strip stops recognising what the writer wrote and every
/// relaunch appends a duplicate group (the #2948 failure mode).
/// What: the literal suffix, including its leading space.
/// Test: `is_project_managed_hook_command_recognises_divert_check`.
pub(super) const DIVERT_CHECK_SUFFIX: &str = " hook --divert-check";

/// Tool names the diversion hook is registered for.
///
/// Why: bulk READS only (#6887 scope). `Read` is Claude Code's own file
/// reader; `Bash` covers `cat`/`head`/`tail`/`less`/`more`, which reach the
/// same bytes by another route. Write diversion is explicitly a follow-up, so
/// no edit tool appears here.
/// What: the two `matcher` values, in the order they are appended.
/// Test: `project_managed_hook_additions_includes_divert_when_enabled`.
const DIVERT_MATCHERS: [&str; 2] = ["Read", "Bash"];

/// Build the `PreToolUse` handler groups that register the diversion check.
///
/// Why: this is the single composition point for the hook's command string, so
/// [`is_divert_hook_command`] has exactly one shape to recognise. The binary is
/// resolved to an ABSOLUTE path via
/// [`resolve_statusline_binary`] for the same reason the PM guard is (#1914): a
/// bare `tm` silently no-ops under Claude Code's minimal `PATH`, which would
/// leave the hook un-fired and the feature invisibly dead.
/// What: returns one group per [`DIVERT_MATCHERS`] entry, each invoking
/// `<abs-path> hook --divert-check` with the same 10-second timeout the PM
/// guard uses. NO credential and no worker model appear in the command string
/// — the resolved config reaches the hook through the session's environment
/// (see [`crate::core::mcp_session_env`]), and credentials are resolved inside
/// the `tm` subprocess at invocation time.
/// Test: `project_managed_hook_additions_includes_divert_when_enabled`.
pub(super) fn divert_hook_groups() -> Vec<serde_json::Value> {
    let command = format!("{}{}", resolve_statusline_binary(), DIVERT_CHECK_SUFFIX);
    DIVERT_MATCHERS
        .iter()
        .map(|matcher| {
            serde_json::json!({
                "matcher": matcher,
                "hooks": [
                    {
                        "type": "command",
                        "command": command,
                        "timeout": 10
                    }
                ]
            })
        })
        .collect()
}

/// Recognise a diversion-check hook command.
///
/// Why (#5034 lesson, restated for this toggle): without this arm the
/// replace-by-identity strip in
/// [`super::settings::write_project_hooks`] never removes the groups a
/// previous launch wrote, so setting `[divert] enabled = false` would leave
/// them firing forever — and, because the strip also dedupes, every relaunch
/// with the toggle ON would append a second copy instead of replacing the
/// first.
/// What: returns `true` when `cmd` ends with [`DIVERT_CHECK_SUFFIX`]. Note this
/// is deliberately NOT covered by
/// [`crate::core::standalone::hooks::is_mpm_hook_command`], which requires the
/// command to end with exactly ` hook`.
/// Test: `is_project_managed_hook_command_recognises_divert_check`.
pub(super) fn is_divert_hook_command(cmd: &str) -> bool {
    cmd.ends_with(DIVERT_CHECK_SUFFIX)
}
