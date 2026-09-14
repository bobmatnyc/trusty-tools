//! Diff a project's `.claude/settings.json` against the hook groups this
//! project's CURRENT config asks for (#7849).
//!
//! Why: every toggle-driven hook group — `[hooks] prompt_context` (#5034),
//! `[divert] enabled` (#6887), `prompt_self_improvement` (#7688) — was written
//! by [`super::settings::write_project_hooks_with`] and by nothing that ever
//! re-read it. `tm validate --repair` asked only whether the `hooks` key was
//! present and non-empty, so a project whose flag flipped on AFTER its settings
//! file was written reported "no gaps found" and the file was left alone: the
//! capture registered at the next fresh session and not before. This module is
//! the missing comparison — it recomputes what the writer WOULD write and names
//! every group the file does not carry, plus every tm-owned group it carries
//! that the config no longer asks for.
//!
//! What: [`project_hook_additions_for`] resolves the three toggles and builds
//! the additions block (the ONE builder the launch path and the resume merge
//! also use, so the comparison can never expect a shape no writer produces);
//! [`project_hook_group_gaps`] diffs that block against a settings value, and
//! reports an unresolvable hook binary as an `Err` rather than as an empty diff
//! — an UNKNOWN answer must never read as a complete one.
//! Read-only — the repair is a call to
//! [`super::resume_hooks::ensure_project_hooks_with`], which merges through the
//! same writer.
//! Test: `hook_group_diff_tests.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use super::project_hooks::{
    is_project_managed_hook_command, is_project_tier_marker_command, project_managed_hook_events,
};
use crate::core::paths::FrameworkPaths;
use crate::core::standalone::hooks::StableHookExeError;
use crate::core::standalone::hooks::cleanup::event_names_matching;

/// Build the project-tier hook additions this project's config asks for.
///
/// Why (#7849): the validator and the resume merge each needed the same three
/// answers — `[hooks] prompt_context`, `[divert] enabled`, and
/// `prompt_self_improvement` — and a second resolution of any one of them would
/// let the diff expect a group no writer writes (or miss one every writer
/// does). One function resolves all three, and both callers go through it.
/// What: `[hooks] prompt_context` from [`crate::core::config::MpmConfig`] at
/// `fw.root`, `[divert] enabled` from the re-resolved plan, and the #7688 flag
/// from [`crate::core::prompt_self_improvement::enabled_for`], fed to
/// [`super::project_hooks::project_managed_hook_additions_with_prompt_feedback`].
/// `exe_override` pins the hook binary; production passes `None` and lets
/// [`crate::core::standalone::hooks::resolve_stable_hook_exe`] (#7670) find the
/// installed one, which refuses a build-tree artifact (#7244).
/// Test: `expected_additions_carry_the_capture_when_the_flag_is_on`,
/// `expected_additions_omit_the_capture_when_the_flag_is_off`.
pub(crate) fn project_hook_additions_for(
    fw: &FrameworkPaths,
    project_dir: &Path,
    exe_override: Option<&Path>,
) -> Result<Value, StableHookExeError> {
    let config = crate::core::config::MpmConfig::load(&fw.root);
    let plan = crate::core::mcp_session_env::resolve_plan(fw, project_dir);
    super::project_hooks::project_managed_hook_additions_with_prompt_feedback(
        exe_override,
        config.hooks.prompt_context,
        plan.divert_enabled,
        // #7849: the toggle the resume merge used to hard-code `false`, which
        // stripped on every resume the capture the launch had just written.
        crate::core::prompt_self_improvement::enabled_for(project_dir),
    )
}

/// The two ways a settings file can disagree with the expected hook set.
///
/// Why: the operator has to see WHICH group and on which event — a
/// file-counting verdict cannot express "the `Stop` capture is unwired", which
/// is the whole #7849 incident. Both directions are reported because both are
/// real: a group the config asks for and the file lacks, and a group the file
/// carries that the config turned off.
/// What: `(event, command)` pairs, deduplicated and in a stable order.
/// Test: `a_flipped_on_flag_is_a_missing_group`,
/// `a_flipped_off_flag_is_a_stale_group`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProjectHookGroupGaps {
    /// Groups the current config asks for that the file does not carry.
    pub missing: Vec<(String, String)>,
    /// tm-owned groups the file carries that the current config does not ask
    /// for — a toggle that flipped back off, or a command naming a binary path
    /// that no longer resolves.
    pub stale: Vec<(String, String)>,
}

impl ProjectHookGroupGaps {
    /// Whether the file matches what the writer would produce.
    pub(crate) fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.stale.is_empty()
    }
}

/// Diff `settings` against the hook groups `project_dir`'s config asks for.
///
/// Why: see the module doc. This is the read-only predicate
/// `core::deploy_validate` and `tm doctor`'s `hooks_missing_tm_group` share, so
/// the two surfaces can never disagree about what a complete hook set is.
/// What: `Ok` with no gaps for a file tm's PROJECT tier never provisioned (the
/// #7490 rule, restated: a foreign project owes tm no hook group, and the user
/// tier owes it no project-tier one — see [`is_project_tier_marker_command`]).
/// `Err` when no stable binary resolves, because the expected COMMANDS cannot
/// then be known and the answer is UNKNOWN rather than "complete" — swallowing
/// that into an empty diff reproduced this very issue one probe along, gated on
/// exe resolution instead of on the toggle. Otherwise `Ok` with every expected
/// group the file does not carry, and every tm-owned command on an owned event
/// that no expected group names.
/// Test: `a_flipped_on_flag_is_a_missing_group`,
/// `a_flipped_off_flag_is_a_stale_group`,
/// `a_file_tm_never_provisioned_has_no_gaps`,
/// `a_user_tier_file_has_no_project_tier_gaps`,
/// `an_unresolved_hook_binary_is_an_error_never_an_empty_diff`.
pub(crate) fn project_hook_group_gaps(
    fw: &FrameworkPaths,
    project_dir: &Path,
    settings: &Value,
    exe_override: Option<&Path>,
) -> Result<ProjectHookGroupGaps, StableHookExeError> {
    project_hook_group_gaps_with(settings, || {
        project_hook_additions_for(fw, project_dir, exe_override)
    })
}

/// [`project_hook_group_gaps`] with the expected additions supplied by the
/// caller.
///
/// Why (#7849): `resolve_stable_hook_exe` falls back to any `tm` on `$PATH` or
/// in the well-known daemon directories, so a refused `exe_override` is rescued
/// on every host that has the binary installed — which is every developer
/// machine. The refusal arm therefore cannot be reached from outside, exactly
/// as [`super::settings::write_project_hooks_with`] found for the writer.
/// Taking the already-computed `Result` lets a test hand the refusal in
/// directly, on any host.
/// What: the tier gate, then `expected()?`, then the comparison. `expected` is
/// not called at all for a file tm's project tier never provisioned, so a
/// foreign project never pays for a resolution it does not need.
/// Test: `an_unresolved_hook_binary_is_an_error_never_an_empty_diff`,
/// `a_file_tm_never_provisioned_has_no_gaps`.
pub(crate) fn project_hook_group_gaps_with(
    settings: &Value,
    expected: impl FnOnce() -> Result<Value, StableHookExeError>,
) -> Result<ProjectHookGroupGaps, StableHookExeError> {
    if event_names_matching(settings, is_project_tier_marker_command).is_empty() {
        return Ok(ProjectHookGroupGaps::default());
    }
    Ok(diff_hook_groups(&expected()?, settings))
}

/// Compare an expected `{"hooks": {...}}` block against a settings value.
///
/// Why: split from [`project_hook_group_gaps`] so the comparison — the part
/// with a real contract — is testable without resolving a config, a manifest or
/// a binary.
/// What: compares (event, [`argv_tail`]) pairs in both directions — `missing`
/// is every expected pair the file carries no tm-owned command for, `stale`
/// every tm-owned pair on an owned event the expected set does not name.
/// Command identity rather than group deep-equality, because the command is
/// what the strip in [`super::settings::write_project_hooks_with`] matches on.
///
/// The ARGV TAIL, never the whole command: the command embeds an absolute
/// binary path, and this crate installs two binaries (`tm` and `trusty-mpm`)
/// that resolve to different paths for the same hook. Comparing the whole
/// string would report every group of a `tm`-written file stale to the daemon
/// running as `trusty-mpm`, and each would rewrite what the other wrote — a
/// repair ping-pong, on every launch. Repointing a command at the installed
/// binary is `hooks_build_tree_binary`'s job (#7262), not this probe's.
/// Test: `diff_reports_nothing_for_an_exact_match`,
/// `diff_reports_a_group_the_file_lacks`,
/// `diff_reports_a_tm_owned_command_the_config_no_longer_asks_for`,
/// `diff_ignores_a_different_binary_path_for_the_same_hook`.
fn diff_hook_groups(expected: &Value, actual: &Value) -> ProjectHookGroupGaps {
    let mut gaps = ProjectHookGroupGaps::default();
    let Some(expected_events) = expected.get("hooks").and_then(Value::as_object) else {
        return gaps;
    };

    let mut expected_tails: BTreeSet<(String, String)> = BTreeSet::new();
    let mut reported: BTreeSet<(String, String)> = BTreeSet::new();
    for event in expected_events.keys() {
        let present = event_commands(actual, event);
        for command in event_commands(expected, event) {
            let tail = argv_tail(&command);
            expected_tails.insert((event.clone(), tail.clone()));
            let wired = present
                .iter()
                .any(|found| is_project_managed_hook_command(found) && argv_tail(found) == tail);
            if !wired && reported.insert((event.clone(), tail)) {
                gaps.missing.push((event.clone(), command));
            }
        }
    }

    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for event in project_managed_hook_events() {
        for command in event_commands(actual, &event) {
            let tail = argv_tail(&command);
            if is_project_managed_hook_command(&command)
                && !expected_tails.contains(&(event.clone(), tail.clone()))
                && seen.insert((event.clone(), tail))
            {
                gaps.stale.push((event.clone(), command));
            }
        }
    }

    gaps
}

/// A hook command with its leading executable path removed.
///
/// Why (#7849): see [`diff_hook_groups`] — the identity of a hook is its
/// arguments, not the absolute path of whichever of this crate's two binaries
/// happened to write it.
/// What: everything from the first space onward (the space included), or the
/// whole string when there is no argument at all.
/// Test: `diff_ignores_a_different_binary_path_for_the_same_hook`.
fn argv_tail(command: &str) -> String {
    match command.find(' ') {
        Some(idx) => command[idx..].to_string(),
        None => command.to_string(),
    }
}

/// Every `hooks.<event>[*].hooks[*].command` string in a settings-shaped value.
fn event_commands(val: &Value, event: &str) -> Vec<String> {
    val.get("hooks")
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                .flatten()
                .filter_map(|entry| entry.get("command").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "hook_group_diff_tests.rs"]
mod tests;
