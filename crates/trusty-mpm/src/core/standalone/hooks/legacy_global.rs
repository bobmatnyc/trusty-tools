//! tm's old hook entries left in the global `~/.claude` settings files
//! (#12077, #12127 row 12).
//!
//! Why: before #2945 tm wrote its lifecycle hooks into `~/.claude/settings.json`,
//! and every launch stripped them again. #12077: tm does not write the generic
//! Claude settings files, so the launch no longer strips them. tm's own hooks
//! now live in its managed `CLAUDE_CONFIG_DIR`, which carries the same entries
//! on purpose, so there is no tm-owned target left for this strip. The launch
//! reports what it finds instead, so the operator can remove it.
//! What: [`legacy_global_mpm_hooks_at`] reads the two files and counts the
//! handler entries that equal, field for field, one tm wrote: a tm hook
//! command, in a `matcher: "*"` group, with the type, timeout and `async` flag
//! [`super::mpm_hook_block`] gives that event. It never writes.
//! Test: `tm_exact_old_global_hook_is_reported_12077`,
//! `a_user_hook_that_differs_from_tm_old_hook_is_not_reported_12077`,
//! `project_settings_below_home_are_not_read_12077`,
//! `an_unreadable_global_file_is_reported_12077`.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// One global settings file that holds tm's old entries, or cannot be read.
#[derive(Debug, PartialEq, Eq)]
pub struct LegacyGlobalHooks {
    /// The file.
    pub path: PathBuf,
    /// How many of tm's exact old handler entries it holds, or why it could
    /// not be read.
    pub found: Result<usize, String>,
}

impl LegacyGlobalHooks {
    /// The stderr warning `tm launch` prints for this file.
    ///
    /// Test: `the_legacy_global_hook_warning_names_each_entry_and_the_removal_12077`.
    pub fn warning(&self) -> String {
        match &self.found {
            Ok(n) => format!(
                "warning: {} still carries {n} of tm's old hook entries; tm no longer \
                 edits this file (#12077) — remove them by hand",
                self.path.display()
            ),
            Err(e) => format!(
                "warning: could not read {} to check for tm's old hook entries: {e}",
                self.path.display()
            ),
        }
    }
}

/// The two GLOBAL Claude settings files under `home`.
///
/// Why (#5875): name them; never walk the home tree.
fn global_settings_files(home: &Path) -> [PathBuf; 2] {
    let claude = home.join(".claude");
    [
        claude.join("settings.json"),
        claude.join("settings.local.json"),
    ]
}

/// Every global settings file under `home` that holds tm's old hook entries or
/// cannot be read; a missing, empty or clean file is left out.
///
/// What: read-only. A file that exists and cannot be read is reported with the
/// error (#12077 fail-open), as is one that is not a JSON object.
/// Test: see the module doc.
pub fn legacy_global_mpm_hooks_at(home: &Path) -> Vec<LegacyGlobalHooks> {
    global_settings_files(home)
        .into_iter()
        .filter_map(|path| {
            let found = match std::fs::read_to_string(&path) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
                Err(err) => Err(err.to_string()),
                Ok(text) if text.trim().is_empty() => return None,
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(value) if value.is_object() => Ok(count_tm_old_entries(&value)),
                    Ok(_) => Err("not a JSON object".to_string()),
                    Err(err) => Err(err.to_string()),
                },
            };
            (found != Ok(0)).then_some(LegacyGlobalHooks { path, found })
        })
        .collect()
}

/// How many handler entries in `settings` equal tm's own, field for field.
fn count_tm_old_entries(settings: &Value) -> usize {
    let Some(events) = settings.get("hooks").and_then(Value::as_object) else {
        return 0;
    };
    events
        .iter()
        .filter_map(|(event, groups)| Some((event, groups.as_array()?)))
        .flat_map(|(event, groups)| groups.iter().map(move |group| (event, group)))
        .filter(|(_, group)| group.get("matcher").and_then(Value::as_str) == Some("*"))
        .flat_map(|(event, group)| {
            let handlers = group.get("hooks").and_then(Value::as_array);
            handlers.into_iter().flatten().map(move |h| (event, h))
        })
        .filter(|(event, handler)| is_tm_old_handler(event, handler))
        .count()
}

/// Whether `handler` under `event` is exactly the entry tm wrote there.
fn is_tm_old_handler(event: &str, handler: &Value) -> bool {
    let Some(cmd) = handler.get("command").and_then(Value::as_str) else {
        return false;
    };
    if !super::is_mpm_hook_command(cmd) {
        return false;
    }
    super::mpm_hook_block(cmd)["hooks"][event][0]["hooks"][0] == *handler
}
