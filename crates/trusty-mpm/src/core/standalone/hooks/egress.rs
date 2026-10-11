//! The `tm hook --egress-guard` settings entry (#8454 G3, slice S2).
//!
//! Why: the S1 guard denies outbound-channel MCP tools only while Claude Code
//! runs it, so its `PreToolUse` entry has to be in every settings tier tm
//! provisions, and has to survive every path that removes tm hook entries.
//! Unlike the pm-guard entry it has no off switch: Architect D3 rules out a
//! kill switch and a config key, so no writer reads `[pm_guard]` for it.
//! Architect D4 puts it in both the project and the user tier.
//! What: [`egress_guard_hook_group`] builds the one group;
//! [`with_egress_guard_group`] and [`push_egress_guard_group`] add it to a
//! writer's additions; [`strip_egress_guard_entries`] is the writers'
//! replace-by-identity strip, so an exe-path change replaces the entry rather
//! than duplicating it. [`egress_guard_state`] and [`disables_all_hooks`] are
//! the read side `tm doctor`'s `egress_guard` row and the trust anchor share.
//! Lives beside `build_tree` rather than in `session_launch`, because both
//! tiers' writers use it and this is the lower of the two layers.
//! Test: `egress_tests.rs`.

use std::path::Path;

use serde_json::{Map, Value};

pub(crate) use super::build_tree::EGRESS_GUARD_SUFFIX;
use super::{StableHookExeError, resolve_stable_hook_exe, strip_hook_entries_matching_for_events};

/// The `timeout`, in seconds, Claude Code is told to allow the egress hook.
///
/// Why: a hook that outlives its timeout is killed, and Claude Code reads a
/// killed hook as non-blocking — an allow. S1's worst case is 7.5 s: the
/// 500 ms gateway probe, the 5 s stdin budget and the 2 s audit POST.
/// What: 10, the value the pm-guard entry uses.
/// Test: `egress_hook_timeout_exceeds_the_guard_budget` (the drift test, in
/// the binary, against S1's own constants).
pub const EGRESS_GUARD_TIMEOUT_SECS: u64 = 10;

/// The `PreToolUse` matchers that fire for every tool.
const ALL_TOOLS_MATCHERS: [&str; 2] = ["", "*"];

/// The `PreToolUse` group that registers `<exe> hook --egress-guard`.
///
/// Why: one composition point for the command, so the identity predicate has
/// one shape to recognise (#2948).
/// What: matcher `""` (every tool), one command entry, timeout
/// [`EGRESS_GUARD_TIMEOUT_SECS`].
/// Test: `egress_group_registers_the_guard_command_with_timeout_10`.
pub fn egress_guard_hook_group(exe: &Path) -> Value {
    serde_json::json!({
        "matcher": ALL_TOOLS_MATCHERS[0],
        "hooks": [{
            "type": "command",
            "command": format!("{}{EGRESS_GUARD_SUFFIX}", exe.display()),
            "timeout": EGRESS_GUARD_TIMEOUT_SECS,
        }]
    })
}

/// Whether `cmd` is an egress-guard hook command, whatever binary it names.
///
/// Test: `egress_is_project_managed_hook_command_claims_the_egress_suffix`.
pub fn is_egress_guard_hook_command(cmd: &str) -> bool {
    cmd.ends_with(EGRESS_GUARD_SUFFIX)
}

/// Append the egress group to a `hooks` object's `PreToolUse` array.
///
/// What: creates the array when absent; skips a group already present. A
/// `PreToolUse` value that is not an array is left alone — the writers'
/// loaders reject such a file before it gets here.
/// Test: `egress_settings_writer_adds_entry_even_when_pm_guard_disabled`.
pub(crate) fn push_egress_guard_group(hooks: &mut Map<String, Value>, exe: &Path) {
    let group = egress_guard_hook_group(exe);
    if let Some(groups) = hooks
        .entry("PreToolUse")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        && !groups.contains(&group)
    {
        groups.push(group);
    }
}

/// `additions` with the egress group appended, the binary resolved by the
/// lifecycle triad's rule.
///
/// Why (#7244): a refusal must reach the caller before anything is written.
/// What: `additions`' error unchanged; then
/// [`resolve_stable_hook_exe`]`(exe_override)`'s error; then the block with
/// [`push_egress_guard_group`] applied to its `hooks` object.
/// Test: `egress_user_tier_writer_adds_the_group_even_when_pm_guard_disabled`.
pub(crate) fn with_egress_guard_group(
    additions: Result<Value, StableHookExeError>,
    exe_override: Option<&Path>,
) -> Result<Value, StableHookExeError> {
    let mut additions = additions?;
    let exe = resolve_stable_hook_exe(exe_override)?;
    if let Some(root) = additions.as_object_mut()
        && let Some(hooks) = root
            .entry("hooks")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
    {
        push_egress_guard_group(hooks, &exe);
    }
    Ok(additions)
}

/// Remove every egress-guard entry from a settings value, on every event.
///
/// Why: replace-by-identity — the writers call it only when they merge a
/// fresh egress group in the same pass, so it never removes the entry.
/// Test: `egress_user_tier_writer_is_idempotent_across_an_exe_path_change`.
pub(crate) fn strip_egress_guard_entries(settings: &mut Value) -> bool {
    strip_hook_entries_matching_for_events(settings, None, is_egress_guard_hook_command)
}

/// What a settings value carries of the egress guard.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EgressGuardState {
    /// No `hook --egress-guard` entry at all.
    Missing,
    /// An entry exists but cannot do its job; the text says why.
    Weakened(String),
    /// A `PreToolUse` group fires it for every tool with a timeout of at least
    /// [`EGRESS_GUARD_TIMEOUT_SECS`].
    Present,
}

/// Read what `settings` carries of the egress guard.
///
/// Why: the doctor row and the project-settings trust anchor must agree on
/// what "the entry is there" means.
/// What: [`EgressGuardState::Present`] when a `hooks.PreToolUse` group whose
/// matcher is absent, `""` or `"*"` holds a `command` entry naming the guard
/// with no timeout (Claude Code's default is 60 s) or an integer one of at
/// least [`EGRESS_GUARD_TIMEOUT_SECS`]. An entry that misses any of those is
/// [`EgressGuardState::Weakened`]; no entry is
/// [`EgressGuardState::Missing`].
/// Test: `egress_state_reads_present_missing_and_weakened`.
pub fn egress_guard_state(settings: &Value) -> EgressGuardState {
    let groups = settings
        .get("hooks")
        .and_then(|hooks| hooks.get("PreToolUse"))
        .and_then(Value::as_array);
    let mut weakened = None;
    for group in groups.into_iter().flatten() {
        let entries = group.get("hooks").and_then(Value::as_array);
        for entry in entries.into_iter().flatten() {
            let named = entry
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(is_egress_guard_hook_command);
            if !named {
                continue;
            }
            match entry_fault(group, entry) {
                None => return EgressGuardState::Present,
                Some(fault) => {
                    weakened.get_or_insert(fault);
                }
            }
        }
    }
    weakened.map_or(EgressGuardState::Missing, EgressGuardState::Weakened)
}

/// Why an egress entry inside `group` cannot do its job, or `None`.
fn entry_fault(group: &Value, entry: &Value) -> Option<String> {
    match group.get("matcher") {
        None => {}
        Some(Value::String(m)) if ALL_TOOLS_MATCHERS.contains(&m.as_str()) => {}
        Some(other) => return Some(format!("its matcher {other} does not fire for every tool")),
    }
    if entry.get("type").and_then(Value::as_str) != Some("command") {
        return Some("its entry is not a `command` hook".to_owned());
    }
    match entry.get("timeout") {
        None => None,
        Some(t) if t.as_u64().is_some_and(|t| t >= EGRESS_GUARD_TIMEOUT_SECS) => None,
        Some(t) => Some(format!(
            "its timeout {t} is under {EGRESS_GUARD_TIMEOUT_SECS} s, so Claude Code can kill it \
             into an allow"
        )),
    }
}

/// Whether `settings` carries any egress-guard entry, working or not.
pub(crate) fn has_egress_guard_entry(settings: &Value) -> bool {
    egress_guard_state(settings) != EgressGuardState::Missing
}

/// Whether `settings` sets `disableAllHooks` to anything but `false`.
///
/// Why: that one key turns every hook off, the egress guard included.
/// Test: `egress_disable_all_hooks_detects_any_non_false_value`.
pub fn disables_all_hooks(settings: &Value) -> bool {
    settings
        .get("disableAllHooks")
        .is_some_and(|value| *value != Value::Bool(false))
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
