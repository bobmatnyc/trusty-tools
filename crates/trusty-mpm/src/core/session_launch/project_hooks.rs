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
//! block, the PM-enforcement guard, the lifecycle triad, and (#6887, when the
//! manifest opts in) the bulk-read diversion groups into one value;
//! [`is_project_managed_hook_command`] recognises a command from ANY of those
//! four sources.
//! Test: `project_hooks_tests.rs`.

use super::divert_hooks::{divert_hook_groups, is_divert_hook_command};
use super::settings::{PM_GUARD_SUFFIX, TRUSTY_MEMORY_HOOKS, pm_guard_hook_value};
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
/// `divert_enabled` is the `[divert] enabled` manifest key (#6887): `true`
/// APPENDS two more `PreToolUse` groups (matcher `Read`, matcher `Bash`) after
/// every existing group, so no other group's bytes change either way; `false`
/// (the default) writes none.
/// Test: `project_managed_hook_additions_combines_all_three_sources`,
/// `project_managed_hook_additions_is_stable_across_calls`,
/// `project_managed_hook_additions_omits_prompt_context_when_disabled`,
/// `project_managed_hook_additions_includes_divert_when_enabled`,
/// `project_managed_hook_additions_omits_divert_when_disabled`.
///
/// #7244: returns the lifecycle triad's resolution error rather than a block
/// built around an unvouched-for command, so the caller writes nothing.
/// `exe_override` pins the binary path (production passes `None` and lets
/// [`mpm_hook_additions_with_exe`] resolve); a test passes an installed-looking
/// path so its assertions do not depend on whether the host running them has
/// `tm` installed.
pub(super) fn project_managed_hook_additions(
    exe_override: Option<&std::path::Path>,
    inject_prompt_context: bool,
    divert_enabled: bool,
) -> Result<serde_json::Value, crate::core::standalone::hooks::StableHookExeError> {
    // #7244: resolved first — a refusal must reach the caller before the
    // project's `.claude/` directory is created or its settings file read.
    let triad = mpm_hook_additions_with_exe(exe_override)?;

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

    // #6887: APPENDED last, and only onto `PreToolUse`, so the PM-guard and
    // lifecycle-triad groups above keep byte-identical positions and
    // content whether the toggle is on or off. Never `insert`
    // `PreToolUse` — that clobbers both (module doc above).
    if divert_enabled && let Some(hooks_obj) = hooks.as_object_mut() {
        let target = hooks_obj
            .entry("PreToolUse".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if let Some(target_arr) = target.as_array_mut() {
            for group in divert_hook_groups() {
                if !target_arr.contains(&group) {
                    target_arr.push(group);
                }
            }
        }
    }

    Ok(serde_json::json!({ "hooks": hooks }))
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
/// What: the event keys of [`project_managed_hook_additions`] with EVERY
/// toggle on — a superset of every toggled variant by construction, so it
/// cannot drift as toggles are added (#6887 adds the second one).
/// Test: `project_managed_hook_events_is_a_superset_of_every_variant`,
/// `write_project_hooks_strips_stale_prompt_context_when_disabled`,
/// `write_project_hooks_strips_stale_divert_when_disabled`.
pub(super) fn project_managed_hook_events() -> Vec<String> {
    // #7244: the key SET is a property of the block's SHAPE and must never
    // depend on whether a binary resolves — an empty answer here would narrow
    // the strip domain and leave stale entries behind. A synthetic
    // installed-looking path makes the resolution unconditionally succeed
    // without a filesystem or PATH lookup; the path itself never reaches a
    // file, because only the keys are read.
    project_managed_hook_additions(Some(EVENT_KEY_PROBE_EXE.as_ref()), true, true)
        .ok()
        .and_then(|additions| {
            additions["hooks"]
                .as_object()
                .map(|events| events.keys().cloned().collect())
        })
        .unwrap_or_default()
}

/// A stable-looking binary path used only to enumerate hook EVENT KEYS.
///
/// Why (#7244): see [`project_managed_hook_events`] — the key set must not
/// vary with the host's installed binaries. Absolute, outside every build
/// tree, and named `tm`, so it satisfies both halves of the
/// `resolve_stable_hook_exe` guard by construction.
/// What: never written anywhere; only the keys of the block built around it
/// are read.
/// Test: `project_managed_hook_events_is_a_superset_of_every_variant`.
const EVENT_KEY_PROBE_EXE: &str = "/usr/local/bin/tm";

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
/// ([`is_mpm_hook_command`]), a `trusty-memory ` command, a PM-guard command
/// (ends with [`PM_GUARD_SUFFIX`]), or a diversion-check command (#6887,
/// [`is_divert_hook_command`]).
/// Test: `is_project_managed_hook_command_recognises_all_three_sources`,
/// `is_project_managed_hook_command_recognises_divert_check`.
pub(super) fn is_project_managed_hook_command(cmd: &str) -> bool {
    is_mpm_hook_command(cmd)
        || cmd.starts_with("trusty-memory ")
        || cmd.ends_with(PM_GUARD_SUFFIX)
        // #6887: without this arm the strip never removes a stale divert group.
        || is_divert_hook_command(cmd)
}

#[cfg(test)]
#[path = "project_hooks_tests.rs"]
mod tests;
