//! Combine the project-tier hook sources [`super::settings::write_project_hooks`]
//! writes into one merge-ready `{"hooks": {...}}` value (issue #2003).
//!
//! Why: the daemon's managed-launch path spawns `claude --setting-sources
//! project,local` (see [`crate::core::model_inject::SETTING_SOURCES_FLAG`]),
//! which excludes the `user` tier where
//! [`crate::core::standalone::hooks::ensure_managed_hooks`] provisions the
//! trusty-mpm lifecycle triad (circuit breaker / audit log / dashboard
//! observability, via [`crate::core::standalone::hooks::mpm_hook_additions_with_exe`]).
//! Managed sessions therefore never fired those six events at all — only the
//! `trusty-memory` + PM-guard entries the OLD `write_project_hooks` wrote
//! (and it wrote them by REPLACING the entire `hooks` key, clobbering any
//! pre-existing foreign hooks too). This module folds the SAME triad
//! definition the user-tier writer uses into the project-tier write, so a
//! single source of truth backs both writers and they cannot drift apart
//! again, and exposes the matching identity predicate so the caller's
//! replace-by-identity strip (entry-level, per issue #2948) recognises every
//! source this module combines.
//! What: [`project_managed_hook_additions`] deep-merges the `trusty-memory`
//! block, the PM-enforcement guard, and the lifecycle triad into one value;
//! [`is_project_managed_hook_command`] recognises a command from ANY of those
//! three sources.
//! Test: `project_hooks_tests.rs`.

use super::settings::{TRUSTY_MEMORY_HOOKS, pm_guard_hook_value};
use crate::core::standalone::hooks::{is_mpm_hook_command, mpm_hook_additions_with_exe};

/// Build the full set of trusty-mpm-owned hook additions for the project tier.
///
/// Why: see the module doc — this is the single place the three sources
/// combine, so [`super::settings::write_project_hooks`] stays a thin
/// read-strip-merge-write shell.
/// What: starts from [`TRUSTY_MEMORY_HOOKS`] (parsed; `UserPromptSubmit` +
/// `SessionStart`), inserts the [`pm_guard_hook_value`] `PreToolUse` block,
/// then appends every group from
/// [`mpm_hook_additions_with_exe`]`(None)`'s six-event lifecycle triad onto
/// the corresponding event array (deduping by deep equality, mirroring
/// [`trusty_common::claude_config::merge_hook_entries`]'s own semantics).
/// `PreToolUse` and `SessionStart` — sources for more than one of the three —
/// end up with multiple handler groups (PM-guard + tm-hook, or trusty-memory +
/// tm-hook) rather than one clobbering the other.
/// `inject_prompt_context` is the `[hooks] prompt_context` config key (#5034):
/// `true` (the default) keeps the `UserPromptSubmit` entry; `false` drops that
/// event key entirely and leaves every other source untouched.
/// Test: `project_managed_hook_additions_combines_all_three_sources`,
/// `project_managed_hook_additions_is_stable_across_calls`,
/// `project_managed_hook_additions_omits_prompt_context_when_disabled`.
pub(super) fn project_managed_hook_additions(inject_prompt_context: bool) -> serde_json::Value {
    let mut hooks: serde_json::Value =
        serde_json::from_str(TRUSTY_MEMORY_HOOKS).expect("bundled hook block is valid JSON");
    if let Some(obj) = hooks.as_object_mut() {
        obj.insert("PreToolUse".to_string(), pm_guard_hook_value());
        // #5034: the opt-out drops only this one event. `SessionStart` (also a
        // TRUSTY_MEMORY_HOOKS key) and every other source stay as they are.
        if !inject_prompt_context {
            obj.remove("UserPromptSubmit");
        }
    }

    let triad = mpm_hook_additions_with_exe(None);
    if let (Some(hooks_obj), Some(triad_hooks)) = (
        hooks.as_object_mut(),
        triad.get("hooks").and_then(serde_json::Value::as_object),
    ) {
        for (event, groups) in triad_hooks {
            let Some(new_groups) = groups.as_array() else {
                continue;
            };
            let target = hooks_obj
                .entry(event.clone())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if let Some(target_arr) = target.as_array_mut() {
                for group in new_groups {
                    if !target_arr.contains(group) {
                        target_arr.push(group.clone());
                    }
                }
            }
        }
    }

    serde_json::json!({ "hooks": hooks })
}

/// Every hook event key trusty-mpm owns at the project tier, regardless of
/// which ones the current config asks to write.
///
/// Why (#5034): [`super::settings::write_project_hooks`] strips its own prior
/// entries only for the events it is about to write. Deriving that strip list
/// from the toggled-down additions would make the opt-out a no-op on any
/// project launched at least once with the hook enabled — the stale
/// `UserPromptSubmit` entry would sit in `.claude/settings.json` forever,
/// still firing, because the strip no longer covered that key. The strip
/// domain must therefore be the full owned set, not the write set.
/// What: the event keys of [`project_managed_hook_additions`]`(true)` — a
/// superset of every toggled variant by construction, so it cannot drift as
/// toggles are added.
/// Test: `project_managed_hook_events_is_a_superset_of_every_variant`,
/// `write_project_hooks_strips_stale_prompt_context_when_disabled`.
pub(super) fn project_managed_hook_events() -> Vec<String> {
    project_managed_hook_additions(true)["hooks"]
        .as_object()
        .map(|events| events.keys().cloned().collect())
        .unwrap_or_default()
}

/// Recognise a hook command belonging to ANY of the three project-tier
/// sources [`project_managed_hook_additions`] combines.
///
/// Why: [`super::settings::write_project_hooks`]'s replace-by-identity strip
/// (via [`crate::core::standalone::hooks::strip_hook_entries_matching_for_events`])
/// needs a predicate broader than
/// [`crate::core::standalone::hooks::is_mpm_hook_command`] alone — that
/// predicate only recognises the lifecycle-triad `<exe> hook` shape, not the
/// `trusty-memory` or PM-guard commands this module also writes. Without the
/// broader predicate, re-running `write_project_hooks` would duplicate the
/// `trusty-memory`/PM-guard groups on every launch instead of replacing them.
/// What: returns `true` for a lifecycle-triad command
/// ([`is_mpm_hook_command`]), a `trusty-memory ` command, or a PM-guard
/// command (ends with ` hook --pm-guard`).
/// Test: `is_project_managed_hook_command_recognises_all_three_sources`.
pub(super) fn is_project_managed_hook_command(cmd: &str) -> bool {
    is_mpm_hook_command(cmd)
        || cmd.starts_with("trusty-memory ")
        || cmd.ends_with(" hook --pm-guard")
}

#[cfg(test)]
#[path = "project_hooks_tests.rs"]
mod tests;
