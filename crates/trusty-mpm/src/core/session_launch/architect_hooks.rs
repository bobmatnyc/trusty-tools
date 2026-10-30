//! The Architect session's PM-guard hook registration (#10459 PR 2, DOC-86 §5).
//!
//! Why: trusty-secrets mints a ticket only for a peer whose argv is exactly
//! `<tm> hook --pm-guard` with a `claude` parent (DOC-86 §2). The shell form
//! every PM session registers runs under `sh -c`, and a failed or timed-out
//! `PreToolUse` hook lets the call through unless `onFailure` says otherwise.
//! What: [`ArchitectHooks`] is the Architect-only switch the project-tier
//! additions take. [`architect_pm_guard_hook_value`] is the exec-form
//! `PreToolUse` group with `onFailure: "block"`; [`push_prompt_hook_groups`]
//! appends the exec-form `UserPromptSubmit` and `UserPromptExpansion` groups
//! when `[architect] direct_commands = true`. PM and agent sessions never
//! reach this module.
//! Test: `architect_hook_settings_golden`, `pm_session_registers_no_ticket_handler`
//! in `architect_hooks_tests.rs`.

use std::path::Path;

use serde_json::{Map, Value, json};

/// The registered argv after `argv[0]`, the absolute `tm` path.
// #10459: must equal trusty-secrets' `HOOK_ARGV_TAIL` (PR 1, `ticket_checks.rs`).
pub(super) const PM_GUARD_ARGS: [&str; 2] = ["hook", "--pm-guard"];

/// The `UserPromptExpansion` matcher: the `/tm-run` command (DOC-86 §3).
pub(super) const TM_RUN_MATCHER: &str = "tm-run";

/// The PM-guard hook timeout, in seconds; the PM group's value.
const PM_GUARD_TIMEOUT_SECS: u64 = 10;

/// What an Architect session adds to the project-tier hook block.
///
/// Why: the one value that separates an Architect launch from a PM launch in
/// [`super::project_hooks::project_managed_hook_additions_for_profile`];
/// `None` there is a PM or agent session, unchanged.
/// What: `direct_commands` is `[architect] direct_commands` from the user
/// config (default `false`).
/// Test: `architect_hook_settings_golden`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArchitectHooks {
    /// Register the `UserPromptSubmit` / `UserPromptExpansion` hooks.
    pub direct_commands: bool,
}

impl ArchitectHooks {
    /// The Architect switch for a session launched in `project_dir`.
    ///
    /// What: `Some` only when [`crate::core::session_profile::resolve`] picks
    /// the Architect profile under `config`; `direct_commands` comes from the
    /// same config's [`crate::core::config::MpmConfig::architect_grant`].
    /// Test: `a_pm_project_resolves_no_architect_hooks`.
    pub(crate) fn for_project(
        project_dir: &Path,
        config: &crate::core::config::MpmConfig,
    ) -> Option<Self> {
        Self::for_profile(
            crate::core::session_profile::resolve(project_dir, config),
            config,
        )
    }

    /// The Architect switch for an already-resolved `profile`: `Some` only for
    /// the Architect, with `direct_commands` from `config`'s grant.
    ///
    /// Test: `the_launch_writes_the_architect_hooks_by_profile`.
    pub(crate) fn for_profile(
        profile: crate::core::session_profile::SessionProfile,
        config: &crate::core::config::MpmConfig,
    ) -> Option<Self> {
        profile.is_architect().then(|| Self {
            direct_commands: config.architect_grant().direct_commands,
        })
    }
}

/// One exec-form `tm hook --pm-guard` entry, with `onFailure` when given.
fn pm_guard_entry(exe: &Path, on_failure: Option<&str>) -> Value {
    let mut entry = json!({
        "type": "command",
        "command": exe.display().to_string(),
        "args": PM_GUARD_ARGS,
        "timeout": PM_GUARD_TIMEOUT_SECS,
    });
    if let (Some(fail), Some(obj)) = (on_failure, entry.as_object_mut()) {
        obj.insert("onFailure".to_owned(), Value::from(fail));
    }
    entry
}

/// The Architect's `PreToolUse` PM-guard group.
///
/// Why (DOC-86 §0, §5): exec form spawns `tm` directly under `claude`, which
/// PR 1's mint check requires; `onFailure: "block"` makes a hook that cannot
/// start, crashes or times out deny the call instead of letting it through.
/// What: `[{matcher: "", hooks: [{type, command: <exe>, args: ["hook",
/// "--pm-guard"], timeout: 10, onFailure: "block"}]}]`, in the position the
/// PM's [`super::settings::pm_guard_hook_value`] takes.
/// Test: `architect_hook_settings_golden`.
pub(super) fn architect_pm_guard_hook_value(exe: &Path) -> Value {
    json!([{ "matcher": "", "hooks": [pm_guard_entry(exe, Some("block"))] }])
}

/// Append the Architect's prompt-hook groups onto `hooks` when
/// `direct_commands` is on.
///
/// Why (DOC-86 §3, §5): the `/tm-run` grant is minted on
/// `UserPromptExpansion` after `UserPromptSubmit` saw the same `prompt_id`;
/// with the flag off neither hook is registered, so nothing can mint a grant.
/// What: `false` changes nothing. `true` appends one exec-form group with
/// matcher `""` onto `UserPromptSubmit` and one with matcher [`TM_RUN_MATCHER`]
/// onto `UserPromptExpansion`, after every existing group and with no
/// `onFailure` (a failure there mints no grant, which is already closed).
/// Test: `architect_hook_settings_golden`.
pub(super) fn push_prompt_hook_groups(hooks: &mut Map<String, Value>, exe: &Path, on: bool) {
    if !on {
        return;
    }
    for (event, matcher) in [
        ("UserPromptSubmit", ""),
        ("UserPromptExpansion", TM_RUN_MATCHER),
    ] {
        let group = json!({ "matcher": matcher, "hooks": [pm_guard_entry(exe, None)] });
        let target = hooks
            .entry(event.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(groups) = target.as_array_mut()
            && !groups.contains(&group)
        {
            groups.push(group);
        }
    }
}

#[cfg(test)]
#[path = "architect_hooks_tests.rs"]
mod tests;
