//! The PM session's `tm hook --turn-task` registration (#11398).
//!
//! Why: the Tasks view attributes a PM turn by a `Task: <id>` line; a
//! `UserPromptSubmit` hook is the one place the harness can put that line into
//! every turn. The Architect keeps its own hooks, unchanged.
//! What: [`push_turn_task_group`] appends one empty-matcher group invoking
//! `<exe> hook --turn-task` onto `UserPromptSubmit`;
//! [`is_turn_task_hook_command`] lets the strip replace a stale one and remove
//! it when the project becomes an Architect. Only the PM branch of
//! [`super::project_hooks::project_managed_hook_additions_for_profile`] calls
//! the writer.
//! Test: `turn_task_hooks_tests.rs`.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::core::standalone::hooks::build_tree::TURN_TASK_SUFFIX;
use crate::core::turn_task_stamp::TURN_TASK_EVENT;

/// The hook timeout, in seconds; the hook itself bounds its only subprocess at 2 s.
const TURN_TASK_TIMEOUT_SECS: u64 = 5;

/// The exact `tm hook --turn-task` group for `exe`.
pub(super) fn turn_task_group(exe: &Path) -> Value {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{}{TURN_TASK_SUFFIX}", exe.display()),
            "timeout": TURN_TASK_TIMEOUT_SECS,
        }],
    })
}

/// Append the turn-task group onto `UserPromptSubmit`, deduped by deep equality.
///
/// Why: appended last, so every other group keeps its bytes and position.
/// What: no `onFailure` — a failed stamp lets the turn through.
/// Test: `pm_sessions_register_the_turn_task_group_and_the_architect_does_not`.
pub(super) fn push_turn_task_group(hooks: &mut Map<String, Value>, exe: &Path) {
    let group = turn_task_group(exe);
    let target = hooks
        .entry(TURN_TASK_EVENT.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(groups) = target.as_array_mut()
        && !groups.contains(&group)
    {
        groups.push(group);
    }
}

/// Recognise a turn-task hook command (#11398).
pub(super) fn is_turn_task_hook_command(cmd: &str) -> bool {
    cmd.ends_with(TURN_TASK_SUFFIX)
}

#[cfg(test)]
#[path = "turn_task_hooks_tests.rs"]
mod tests;
