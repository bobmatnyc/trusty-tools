//! tm's hooks on the launch-time `--settings` tier, and the strip that removes
//! the copies older launches wrote into the project (#12077 part 4c).
//!
//! Why: owner ruling (Bob, 1.14.16) "Move the hooks". Every launch builds tm's
//! hook groups into the one `--settings` body
//! ([`crate::core::launch_policy::carrier`]), so no project file needs them and
//! the PM cannot edit the guard out of a file in its own checkout. The copies
//! earlier launches wrote into `<ws>/.claude/settings.json` and
//! `settings.local.json` are removed on each launch (Architect 14:38Z Q1).
//! What: [`launch_hook_additions`] builds the groups a launch of `profile`
//! carries, through the same builder the project writer used;
//! [`strip_tm_project_hooks`] removes tm's entries from both project files,
//! under the settings lock, writing only a file that changed;
//! [`is_stale_tm_hook_command`] is the identity predicate the strip and
//! `tm doctor`'s `hooks_contamination` row share.
//! Test: `launch_hooks_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::project_hooks::is_project_managed_hook_command;
use crate::core::paths::FrameworkPaths;
use crate::core::session_profile::SessionProfile;
use crate::core::standalone::hooks::StableHookExeError;
use crate::core::standalone::hooks::cleanup::event_names_matching;

/// The project settings files the strip reads, relative to `<ws>/.claude`.
pub const PROJECT_SETTINGS_FILES: [&str; 2] = ["settings.json", "settings.local.json"];

/// The `trusty-memory` commands tm's project writer added (`TRUSTY_MEMORY_HOOKS`).
///
/// Why: `trusty-memory setup` writes the same two commands into a project on
/// its own, so a bare `trusty-memory ` prefix would claim the operator's hook.
/// These are stripped only from a file that also carries a tm identity entry,
/// because only tm's project writer put both there.
/// Test: `a_memory_hook_alone_is_the_operators_and_stays_12077`.
pub const TM_MEMORY_HOOK_COMMANDS: [&str; 2] =
    ["trusty-memory prompt-context", "trusty-memory inbox-check"];

/// The hook groups one launch carries on its `--settings` body.
///
/// Why: the body replaces the project file as the carrier, so it must hold
/// what the project writer wrote: the same builder, the same toggles.
/// What: `[hooks] prompt_context`, `[pm_guard] enabled` and the #7688 host flag
/// from `fw.root`'s config; `[divert] enabled` from the re-resolved plan; the
/// Architect groups (exec-form pm-guard with `onFailure: block`, standing
/// rules) by `profile`. `exe` names every tm command.
///
/// # Errors
///
/// The resolver's refusal of `exe`; the caller refuses the launch (Q2).
///
/// Test: `the_body_carries_the_pm_guard_and_egress_groups_12077`,
/// `an_architect_body_carries_the_exec_form_guard_12077`.
pub fn launch_hook_additions(
    fw: &FrameworkPaths,
    project_dir: &Path,
    profile: SessionProfile,
    exe: &Path,
) -> Result<Value, StableHookExeError> {
    let config = crate::core::config::MpmConfig::load(&fw.root);
    let plan = crate::core::mcp_session_env::resolve_plan(fw, project_dir);
    super::project_hooks::project_managed_hook_additions_for_profile(
        Some(exe),
        config.hooks.prompt_context,
        plan.divert_enabled,
        crate::core::prompt_self_improvement::enabled_for_with_host(
            project_dir,
            config.pm.prompt_self_improvement,
        ),
        config.pm_guard.enabled,
        super::architect_hooks::ArchitectHooks::for_profile(profile, &config),
    )
}

/// Whether `cmd` is an entry tm's own project writer put in a project file.
///
/// Why (Q1): an exact match on the current exe is too narrow — the managed
/// file's `Notification` hook names a mise shim, the rest `~/.cargo/bin/tm` —
/// so the strip uses tm's identity predicates, which match any tm binary path.
/// What: the project writer's own strip predicate
/// ([`is_project_managed_hook_command`]: lifecycle `<tm> hook`, pm-guard,
/// divert, prompt feedback, egress guard, standing rules, turn task,
/// completion evidence, shadow guard) minus its bare `trusty-memory ` prefix.
/// Test: `the_strip_predicate_claims_every_tm_shape_and_no_memory_prefix_12077`.
pub fn is_stale_tm_hook_command(cmd: &str) -> bool {
    !cmd.starts_with("trusty-memory ") && is_project_managed_hook_command(cmd)
}

/// The event keys of `val` that carry an entry the strip would remove.
///
/// Why: `tm doctor`'s `hooks_contamination` row and the strip must agree.
/// Test: `stale_event_names_match_what_the_strip_removes_12077`.
pub fn stale_tm_hook_event_names(val: &Value) -> Vec<String> {
    let marked = !event_names_matching(val, is_stale_tm_hook_command).is_empty();
    event_names_matching(val, |cmd| is_stripped(cmd, marked))
}

/// The strip's predicate for one file: `marked` when the file carries a tm
/// identity entry, which is what makes its memory commands tm's.
fn is_stripped(cmd: &str, marked: bool) -> bool {
    is_stale_tm_hook_command(cmd) || (marked && TM_MEMORY_HOOK_COMMANDS.contains(&cmd))
}

/// Remove tm's entries from one settings value; `true` when anything changed.
///
/// What: entry granularity, every event; a group or event left empty goes,
/// and so does an emptied `hooks` key. Foreign entries keep their place.
/// Test: `a_launch_removes_only_tms_entries_12077`.
pub fn strip_tm_entries(val: &mut Value) -> bool {
    let marked = !event_names_matching(val, is_stale_tm_hook_command).is_empty();
    crate::core::standalone::hooks::strip_hook_entries_matching_for_events(val, None, |cmd| {
        is_stripped(cmd, marked)
    })
}

/// A project settings file the strip could not read or write.
#[derive(Debug, thiserror::Error)]
#[error("#12077 could not remove tm's old hooks from {path}: {source}")]
pub struct StripError {
    /// The settings file.
    pub path: PathBuf,
    /// The read, lock or write error.
    #[source]
    pub source: std::io::Error,
}

/// Remove tm's old hook entries from `<project_dir>/.claude/settings{,.local}.json`.
///
/// Why: see the module doc. Claude Code concatenates the project tiers with
/// the flag tier, so a stale copy naming another exe path would fire beside
/// the body's.
/// What: per file, under the settings lock: absent → nothing; not a JSON
/// object → left alone, logged (row 11's rule); otherwise [`strip_tm_entries`],
/// published only when it changed. Returns the files rewritten.
///
/// # Errors
///
/// [`StripError`] when a file exists and cannot be read, locked or written;
/// the launch refuses rather than run beside a copy it could not remove.
///
/// Test: `a_default_launch_leaves_a_hookless_project_file_byte_identical_12077`,
/// `a_launch_removes_only_tms_entries_12077`,
/// `an_unreadable_project_file_refuses_the_strip_12077`,
/// `an_unwritable_project_file_refuses_the_strip_12077`.
pub fn strip_tm_project_hooks(project_dir: &Path) -> Result<Vec<PathBuf>, StripError> {
    let mut changed = Vec::new();
    for name in PROJECT_SETTINGS_FILES {
        let path = project_dir.join(".claude").join(name);
        if strip_file(&path)? {
            changed.push(path);
        }
    }
    Ok(changed)
}

/// [`strip_tm_project_hooks`] for one file; `true` when it was rewritten.
fn strip_file(path: &Path) -> Result<bool, StripError> {
    let err = |source| StripError {
        path: path.to_path_buf(),
        source,
    };
    // A missing file is the common case; no lock file is created for it.
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(err(e)),
        Ok(_) => {}
    }
    crate::core::settings_lock::with_settings_lock(path, || strip_file_locked(path))
        .map_err(err)?
        .map_err(err)
}

/// The read / strip / publish body of [`strip_file`], under the lock.
fn strip_file_locked(path: &Path) -> std::io::Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut value = match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => value,
        _ => {
            tracing::warn!(
                path = %path.display(),
                "#12077: not a JSON object; the launch-time hook strip leaves it alone"
            );
            return Ok(false);
        }
    };
    if !strip_tm_entries(&mut value) {
        return Ok(false);
    }
    crate::core::settings_lock::publish(path, &value)?;
    Ok(true)
}

#[cfg(test)]
#[path = "launch_hooks_tests.rs"]
mod tests;
