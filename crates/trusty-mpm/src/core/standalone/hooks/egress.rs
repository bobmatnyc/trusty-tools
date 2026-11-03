//! The retired `tm hook --egress-guard` settings entry (#8454, removed by #10735).
//!
//! Why: the owner removed the egress guard (ruling D19). An installed settings
//! file may still carry the entry; it must be recognised so the writers strip
//! it, and the pm-guard trust anchor still reads it while it exists.
//! What: [`is_egress_guard_hook_command`] is the identity predicate,
//! [`strip_egress_guard_entries`] the writers' strip, and
//! [`egress_guard_state`] and [`disables_all_hooks`] the read side the trust
//! anchor shares. Nothing here writes an entry.
//! Test: `egress_tests.rs`.

use serde_json::Value;

pub(crate) use super::build_tree::EGRESS_GUARD_SUFFIX;
use super::strip_hook_entries_matching_for_events;

/// The `timeout`, in seconds, Claude Code is told to allow the egress hook.
///
/// What: 10, the value the pm-guard entry used; only the trust-anchor read of
/// an existing entry still compares against it.
pub const EGRESS_GUARD_TIMEOUT_SECS: u64 = 10;

/// The `PreToolUse` matchers that fire for every tool.
const ALL_TOOLS_MATCHERS: [&str; 2] = ["", "*"];

/// Whether `cmd` is an egress-guard hook command, whatever binary it names.
///
/// Test: `egress_is_project_managed_hook_command_claims_the_egress_suffix`.
pub fn is_egress_guard_hook_command(cmd: &str) -> bool {
    cmd.ends_with(EGRESS_GUARD_SUFFIX)
}

/// Remove every egress-guard entry from a settings value, on every event.
///
/// Why (#10735): the guard is removed, so every writer drops a stale entry.
/// Test: `egress_entry_is_stripped_by_the_user_tier_writers`.
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
