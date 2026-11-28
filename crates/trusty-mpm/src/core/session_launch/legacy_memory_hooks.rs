//! The legacy global `trusty-memory` hook strip (#12077, #12127 row 11).
//!
//! Why: earlier trusty-mpm builds registered `trusty-memory` hooks at the user
//! tier, so they fired for every Claude Code session. They now live in each
//! project's hooks, and the user-tier copies must go. #12077: tm does not write
//! the generic `~/.claude/settings.json`, so the strip runs only in the config
//! dir a tm launch names (`CLAUDE_CONFIG_DIR`, tm's own), and it removes only a
//! group equal to one tm wrote. A group that differs in any field is the
//! operator's and stays.
//! What: [`remove_global_trusty_memory_hooks`] resolves the target;
//! [`clean_global_trusty_memory_hooks`] strips one file.
//! Test: `tests_legacy_hooks_12077.rs`,
//! `remove_global_hooks_removes_trusty_memory_entries`.

use std::path::Path;

use serde_json::Value;

use super::PrepError;

/// Every global `trusty-memory` hook group tm wrote, by event, exactly.
///
/// Why: exact-match removal needs the exact values. The `hooks fire` triad is
/// the pre-#1270 block; `prompt-context` and `inbox-check` are the canonical
/// pair #1270 moved to (`crates/trusty-mpm/src/core/session_launch/settings.rs`,
/// `TRUSTY_MEMORY_HOOKS`, as of that change). A literal, not a parse of the
/// live block, so a later change to the project block cannot widen the strip.
/// Test: `tm_exact_old_groups_are_removed_from_the_config_dir_12077`,
/// `a_user_hook_that_differs_from_tm_old_hook_is_kept_12077`.
const TM_OLD_GLOBAL_GROUPS: &str = r#"{
  "UserPromptSubmit": [
    {"matcher": "", "hooks": [{"type": "command", "command": "trusty-memory prompt-context", "timeout": 60}]},
    {"matcher": "", "hooks": [{"type": "command", "command": "trusty-memory hooks fire claude.user-prompt", "timeout": 60}]}
  ],
  "SessionStart": [
    {"matcher": "", "hooks": [{"type": "command", "command": "trusty-memory inbox-check", "timeout": 60}]}
  ],
  "PostToolUse": [
    {"matcher": "Write|Edit|Bash", "hooks": [{"type": "command", "command": "trusty-memory hooks fire claude.post-tool-use", "timeout": 60}]}
  ],
  "Stop": [
    {"matcher": "", "hooks": [{"type": "command", "command": "trusty-memory hooks fire claude.stop", "timeout": 60}]}
  ]
}"#;

/// [`TM_OLD_GLOBAL_GROUPS`], parsed.
fn tm_old_groups() -> serde_json::Map<String, Value> {
    serde_json::from_str(TM_OLD_GLOBAL_GROUPS).expect("TM_OLD_GLOBAL_GROUPS is a JSON object")
}

/// Strip tm's old global `trusty-memory` hook groups from tm's config dir.
///
/// Why: see the module doc. #5544: `home` is a parameter, never read off
/// `FrameworkPaths`.
/// What: `config_dir` `None` is a no-op — tm never writes
/// `<home>/.claude/settings.json` (#12077). Otherwise the user tier under
/// `config_dir` (declined when not absolute) is cleaned through
/// [`clean_global_trusty_memory_hooks`].
///
/// # Errors
///
/// Whatever [`clean_global_trusty_memory_hooks`] returns.
///
/// Test: `the_strip_never_writes_the_home_settings_file_12077`,
/// `tm_exact_old_groups_are_removed_from_the_config_dir_12077`,
/// `global_hook_cleanup_never_writes_the_real_home_under_an_overridden_root_12077`.
pub(super) fn remove_global_trusty_memory_hooks(
    home: Option<&Path>,
    config_dir: Option<&Path>,
) -> Result<(), PrepError> {
    // #12077: no tm-owned config dir named, so there is nothing tm may write.
    let Some(config_dir) = config_dir else {
        return Ok(());
    };
    let Some(tier) = super::user_tier::resolve_for_write(home, Some(config_dir)) else {
        return Ok(());
    };
    // #8545: fenced in tests.
    crate::core::home_write_fence::check(&tier.dir);
    clean_global_trusty_memory_hooks(&tier.settings_json())
}

/// Remove, from one settings file, every hook group equal to one of tm's old
/// global `trusty-memory` groups.
///
/// What: an absent file is `Ok`. A file that is not a JSON object is left
/// untouched and `Ok` (logged). A group is removed only when it equals an entry
/// of [`TM_OLD_GLOBAL_GROUPS`] under the same event; an event left empty is
/// removed. Writes atomically, and only when a group was removed.
///
/// # Errors
///
/// [`PrepError::Io`] when the file exists and cannot be read, or the write
/// fails.
///
/// Test: `tm_exact_old_groups_are_removed_from_the_config_dir_12077`,
/// `a_user_hook_that_differs_from_tm_old_hook_is_kept_12077`,
/// `an_unreadable_settings_file_is_an_error_12077`,
/// `remove_global_hooks_tolerates_missing_file`.
pub(super) fn clean_global_trusty_memory_hooks(settings_path: &Path) -> Result<(), PrepError> {
    let io = |source| PrepError::Io {
        path: settings_path.to_path_buf(),
        source,
    };
    let text = match std::fs::read_to_string(settings_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        // #12077: an unreadable file is reported, never read as clean.
        Err(err) => return Err(io(err)),
    };
    let mut settings = match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => value,
        _ => {
            tracing::warn!(
                path = %settings_path.display(),
                "#12077: not a JSON object; the legacy trusty-memory hook strip leaves it alone"
            );
            return Ok(());
        }
    };
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let mut changed = false;
    for (event, owned) in tm_old_groups() {
        let owned = owned.as_array().cloned().unwrap_or_default();
        let Some(groups) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        let before = groups.len();
        groups.retain(|group| !owned.contains(group));
        if groups.len() != before {
            changed = true;
            if groups.is_empty() {
                hooks.remove(&event);
            }
        }
    }
    if !changed {
        return Ok(());
    }
    crate::core::settings_lock::publish(settings_path, &settings).map_err(io)
}
