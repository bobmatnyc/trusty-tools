//! tm's old hook entries left in the global `~/.claude` settings files
//! (#12077, #12127 row 12).
//!
//! Why: before #2945 tm wrote its lifecycle hooks into `~/.claude/settings.json`,
//! and every launch stripped them again. #12077: tm does not write the generic
//! Claude settings files, so the launch no longer strips them. tm's own hooks
//! now live in its managed `CLAUDE_CONFIG_DIR`, which carries the same entries
//! on purpose, so there is no tm-owned target left for this strip. The launch
//! reports what it finds instead, so the operator can remove it.
//! What: [`legacy_global_mpm_hooks_at`] reads the two files and lists the
//! handler entries that equal, field for field, one tm wrote: a tm hook
//! command, in a `matcher: "*"` group, with the type, timeout and `async` flag
//! [`super::mpm_hook_block`] gives that event. It never writes.
//! [`LegacyGlobalHooks::warning`] is the launch's text for one file.
//! Test: `tm_exact_old_global_hook_is_reported_12077`,
//! `a_user_hook_that_differs_from_tm_old_hook_is_not_reported_12077`,
//! `project_settings_below_home_are_not_read_12077`,
//! `an_unreadable_global_file_is_reported_12077`.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// One of tm's old handler entries, by hook event and command.
#[derive(Debug, PartialEq, Eq)]
pub struct LegacyHookEntry {
    /// The hook event the entry sits under, e.g. `PreToolUse`.
    pub event: String,
    /// The entry's `command`.
    pub command: String,
}

/// One global settings file that holds tm's old entries, or cannot be read.
#[derive(Debug, PartialEq, Eq)]
pub struct LegacyGlobalHooks {
    /// The file.
    pub path: PathBuf,
    /// tm's exact old handler entries it holds, in file order, or why it
    /// could not be read.
    pub found: Result<Vec<LegacyHookEntry>, String>,
}

impl LegacyGlobalHooks {
    /// The stderr warning `tm launch` prints for this file (#12077 row 12).
    ///
    /// What: names each entry (file, event, command); gives `tm hooks clean
    /// --path ~`, first without `--force` as a dry run, then with it, and says
    /// that it backs up first and matches any plain `tm hook` entry, wider than
    /// the list; and gives the manual alternative. For an unreadable file, the
    /// error.
    /// Test: `the_legacy_global_hook_warning_names_each_entry_and_the_removal_12077`.
    pub fn warning(&self) -> String {
        let file = self.path.display();
        let entries = match &self.found {
            Ok(entries) => entries,
            Err(e) => {
                return format!(
                    "warning: could not read {file} to check for tm's old hook entries: {e}"
                );
            }
        };
        let mut text = format!(
            "warning: {file} still holds {} of tm's old hook entries. tm no longer edits \
             this file (#12077), so it does not remove them:",
            entries.len()
        );
        for entry in entries {
            text.push_str(&format!(
                "\n  - {file}: {} runs `{}`",
                entry.event, entry.command
            ));
        }
        text.push_str(
            "\n  To remove them, run `tm hooks clean --path ~` first without `--force`, as a \
             dry run that lists what it would remove, then run `tm hooks clean --path ~ \
             --force`, which backs up each file before it writes. `tm hooks clean` matches \
             more widely than the entries above: it removes any plain `tm hook` entry in \
             ~/.claude/settings.json and ~/.claude/settings.local.json.\
             \n  Or delete those handlers under `hooks` in ~/.claude/settings.json / \
             ~/.claude/settings.local.json yourself.",
        );
        text
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
                    Ok(value) if value.is_object() => Ok(tm_old_entries(&value)),
                    Ok(_) => Err("not a JSON object".to_string()),
                    Err(err) => Err(err.to_string()),
                },
            };
            let clean = found.as_ref().is_ok_and(Vec::is_empty);
            (!clean).then_some(LegacyGlobalHooks { path, found })
        })
        .collect()
}

/// The handler entries in `settings` that equal tm's own, field for field.
fn tm_old_entries(settings: &Value) -> Vec<LegacyHookEntry> {
    let Some(events) = settings.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
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
        .map(|(event, handler)| LegacyHookEntry {
            event: event.clone(),
            command: handler
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect()
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
