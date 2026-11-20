//! The `tm hook --pm-guard-shadow` registration (#11431).
//!
//! Why: the minimal pm-guard runs log-only beside the old guard for the
//! brief's 7-day shadow window, in every PM, subagent and Architect session.
//! The hook picks the rules per profile at run time (Architect: rule 1 only).
//! What: [`push_shadow_guard_group`] appends one `PreToolUse` group, matched
//! to the tools the rules read, invoking `<exe> hook --pm-guard-shadow`.
//! It is independent of `[pm_guard] enabled`. [`is_shadow_guard_hook_command`]
//! lets the strip replace a stale one.
//! Test: `shadow_guard_hooks_tests.rs`.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::core::standalone::hooks::build_tree::PM_GUARD_SHADOW_SUFFIX;

/// The hook timeout, in seconds; each `git` probe inside is bounded at 2 s.
const SHADOW_TIMEOUT_SECS: u64 = 10;
/// The tools whose input the shadow rules read.
const SHADOW_MATCHER: &str = "Bash|Write|Edit|MultiEdit|NotebookEdit";

/// The exact shadow-guard group for `exe`.
pub(super) fn shadow_guard_group(exe: &Path) -> Value {
    json!({
        "matcher": SHADOW_MATCHER,
        "hooks": [{
            "type": "command",
            "command": format!("{}{PM_GUARD_SHADOW_SUFFIX}", exe.display()),
            "timeout": SHADOW_TIMEOUT_SECS,
        }],
    })
}

/// Append the shadow group onto `PreToolUse`, deduped by deep equality.
///
/// Why: appended right after the egress guard, so every group the toggles add
/// later still only appends.
/// What: no `onFailure` — the hook never blocks, and a failed run must not either.
/// Test: `every_session_registers_the_shadow_guard_after_the_egress_guard`.
pub(super) fn push_shadow_guard_group(hooks: &mut Map<String, Value>, exe: &Path) {
    let group = shadow_guard_group(exe);
    let target = hooks
        .entry("PreToolUse".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(groups) = target.as_array_mut()
        && !groups.contains(&group)
    {
        groups.push(group);
    }
}

/// Recognise a shadow-guard hook command (#11431).
pub(super) fn is_shadow_guard_hook_command(cmd: &str) -> bool {
    cmd.ends_with(PM_GUARD_SHADOW_SUFFIX)
}

#[cfg(test)]
#[path = "shadow_guard_hooks_tests.rs"]
mod tests;
