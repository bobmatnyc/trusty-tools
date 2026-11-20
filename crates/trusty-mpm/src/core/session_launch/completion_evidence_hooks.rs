//! The PM session's `tm hook --completion-evidence` registration (#10575).
//!
//! Why: the completion-standard check reads the PM's final message, which only
//! a `Stop` hook sees. PM sessions only; the Architect's hooks are unchanged.
//! What: [`push_completion_evidence_group`] appends one empty-matcher group
//! invoking `<exe> hook --completion-evidence` onto `Stop`;
//! [`is_completion_evidence_hook_command`] lets the strip replace a stale one.
//! Test: `architect_hook_settings_golden`, `write_project_hooks_replaces_existing`.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::core::standalone::hooks::build_tree::COMPLETION_EVIDENCE_SUFFIX;

/// The hook timeout, in seconds; the hook reads at most a 2 MiB transcript tail.
const COMPLETION_EVIDENCE_TIMEOUT_SECS: u64 = 10;

/// The exact `tm hook --completion-evidence` group for `exe`.
pub(super) fn completion_evidence_group(exe: &Path) -> Value {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{}{COMPLETION_EVIDENCE_SUFFIX}", exe.display()),
            "timeout": COMPLETION_EVIDENCE_TIMEOUT_SECS,
        }],
    })
}

/// Append the group onto `Stop`, deduped by deep equality, after every other group.
pub(super) fn push_completion_evidence_group(hooks: &mut Map<String, Value>, exe: &Path) {
    let group = completion_evidence_group(exe);
    let target = hooks
        .entry("Stop".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(groups) = target.as_array_mut()
        && !groups.contains(&group)
    {
        groups.push(group);
    }
}

/// Recognise a completion-evidence hook command (#10575).
pub(super) fn is_completion_evidence_hook_command(cmd: &str) -> bool {
    cmd.ends_with(COMPLETION_EVIDENCE_SUFFIX)
}
