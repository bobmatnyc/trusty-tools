//! The `Stop` / `SubagentStop` prompt-feedback hook groups (#7688).
//!
//! Why: capturing the `## Prompt feedback` addendum needs a hook that fires as
//! a turn ends, and it must exist ONLY where the flag is on — a project with
//! the feature off should not spawn a `tm` process at the end of every turn to
//! discover there is nothing to do. This module owns the two groups that wire
//! it, kept out of [`super::project_hooks`] and [`super::settings`] for the
//! reason #6887 kept [`super::divert_hooks`] out: both neighbours sit close to
//! the 500-SLOC production cap, and the command string, the group shape, and
//! the identity predicate are one concern that moves together.
//!
//! What: [`prompt_feedback_hook_groups`] builds one group per captured event,
//! invoking `<abs-path> hook --prompt-feedback`;
//! [`is_prompt_feedback_hook_command`] is the matching identity predicate
//! [`super::project_hooks::is_project_managed_hook_command`] folds in, so the
//! replace-by-identity strip removes a stale entry when the flag flips back off
//! — the #5034 lesson, restated for this toggle.
//!
//! These groups are ADDITIVE to the lifecycle triad, which already registers a
//! bare `<exe> hook` on both events. Two groups on one event both fire; the
//! triad's relay is untouched, so nothing it records changes either way.
//! Test: `project_hooks_tests.rs`
//! (`project_managed_hook_additions_includes_prompt_feedback_when_enabled`,
//! `project_managed_hook_additions_omits_prompt_feedback_when_disabled`,
//! `is_project_managed_hook_command_recognises_prompt_feedback`).

use super::settings::resolve_statusline_binary;

/// The `tm hook` sub-flag the capture is invoked with.
///
/// Why: the builder, the identity predicate, and the build-tree classifier
/// (#7262) must agree on one spelling, or the strip stops recognising what the
/// writer wrote and every relaunch appends a duplicate group (the #2948 failure
/// mode). It therefore lives in the lower layer beside the other argv tails and
/// is re-exported here rather than spelled out twice.
/// What: the literal suffix, including its leading space.
/// Test: `is_project_managed_hook_command_recognises_prompt_feedback`.
pub(super) use crate::core::standalone::hooks::build_tree::PROMPT_FEEDBACK_SUFFIX;

/// Hook events the capture is registered for.
///
/// Why: `Stop` ends the PM's own turn and `SubagentStop` ends a dispatched
/// agent's, so between them every response that could carry the addendum is
/// covered exactly once. No tool event appears here — the addendum is a
/// property of a finished RESPONSE, not of a tool call.
/// What: the two event keys, in the order their groups are appended.
/// Test: `project_managed_hook_additions_includes_prompt_feedback_when_enabled`.
const PROMPT_FEEDBACK_EVENTS: [&str; 2] = ["Stop", "SubagentStop"];

/// Build the handler groups that register the capture, keyed by event.
///
/// Why: the single composition point for this hook's command string, so
/// [`is_prompt_feedback_hook_command`] has exactly one shape to recognise. The
/// binary is resolved to an ABSOLUTE path via [`resolve_statusline_binary`] for
/// the same reason the PM guard is (#1914): a bare `tm` silently no-ops under
/// Claude Code's minimal `PATH`, which would leave the hook un-fired and the
/// feature invisibly dead.
/// What: `(event, group)` pairs — one per [`PROMPT_FEEDBACK_EVENTS`] entry,
/// each an empty-matcher group invoking `<abs-path> hook --prompt-feedback`
/// with the same 10-second timeout the PM guard uses. An empty matcher because
/// neither event carries a tool name to match on.
/// Test: `project_managed_hook_additions_includes_prompt_feedback_when_enabled`.
pub(super) fn prompt_feedback_hook_groups() -> Vec<(&'static str, serde_json::Value)> {
    let command = format!("{}{}", resolve_statusline_binary(), PROMPT_FEEDBACK_SUFFIX);
    PROMPT_FEEDBACK_EVENTS
        .iter()
        .map(|event| {
            (
                *event,
                serde_json::json!({
                    "matcher": "",
                    "hooks": [
                        {
                            "type": "command",
                            "command": command,
                            "timeout": 10
                        }
                    ]
                }),
            )
        })
        .collect()
}

/// Recognise a prompt-feedback hook command.
///
/// Why (#5034 lesson, restated for this toggle): without this arm the
/// replace-by-identity strip in [`super::settings::write_project_hooks`] never
/// removes the groups a previous launch wrote, so turning the flag back off
/// would leave them firing forever — and, because the strip also dedupes, every
/// relaunch with the flag ON would append a second copy instead of replacing
/// the first.
/// What: `true` when `cmd` ends with [`PROMPT_FEEDBACK_SUFFIX`]. Deliberately
/// NOT covered by
/// [`is_mpm_hook_command`](crate::core::standalone::hooks::is_mpm_hook_command),
/// which requires the command to end with exactly ` hook`.
/// Test: `is_project_managed_hook_command_recognises_prompt_feedback`.
pub(super) fn is_prompt_feedback_hook_command(cmd: &str) -> bool {
    cmd.ends_with(PROMPT_FEEDBACK_SUFFIX)
}
