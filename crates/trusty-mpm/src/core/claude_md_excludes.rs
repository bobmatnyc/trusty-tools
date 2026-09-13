//! Read and extend Claude Code's `claudeMdExcludes` settings key (#7673).
//!
//! Why: an ancestor `CLAUDE.md` with real content in it is not tm's to delete —
//! it is someone's notes, and a monorepo may want it for its OWN subtrees while
//! this project does not. Claude Code's own answer is `claudeMdExcludes`: an
//! array of paths/globs, merged across settings layers, naming memory files a
//! session must skip. That makes the ⚠️ repair reversible and non-destructive —
//! one array entry, no file touched.
//!
//! What: [`merged_excludes`] reads the key from every layer a session loads and
//! returns the union; [`is_excluded`] answers whether one path is covered by
//! that union (exact match or glob); [`add_exclude`] appends one absolute path
//! to the project's `.claude/settings.local.json`, creating the array when
//! absent and never touching another key.
//!
//! FAIL-CLOSED on an unreadable or non-object settings file: it contributes no
//! excludes to the read, and [`add_exclude`] refuses to write it rather than
//! replacing it with a fresh object.
//! Test: `claude_md_excludes_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use trusty_common::claude_config::write_json_atomic;

/// The Claude Code settings key naming memory files a session must skip.
///
/// What: `claudeMdExcludes`, an array of paths or globs, merged across layers.
/// Test: `the_settings_key_is_frozen`.
pub const EXCLUDES_KEY: &str = "claudeMdExcludes";

/// What one [`add_exclude`] call did to one settings file.
///
/// Why: the doctor `--fix` arm must distinguish "added", "already there" and
/// "declined because the file is corrupt" — a bool cannot carry that, and the
/// corrupt case must never render as a success.
/// Test: `adding_an_exclude_seeds_the_array`, `adding_is_idempotent`,
/// `an_unparseable_settings_file_is_refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcludeWrite {
    /// The path was appended to the array (which was created if absent).
    Added,
    /// The path was already listed; nothing was written.
    AlreadyListed,
    /// The file could not be read as a JSON object; nothing was written.
    Refused(String),
}

/// The settings files whose `claudeMdExcludes` a session in `project_root` merges.
///
/// Why: Claude Code merges the key across layers, so a doctor check that read
/// only one of them would report an already-excluded ancestor as a finding. The
/// managed `$CLAUDE_CONFIG_DIR` tier is included because that is the tier a
/// tm-launched session actually reads (#7423).
/// What: the project's `.claude/settings.json` and `.claude/settings.local.json`,
/// the user's `~/.claude/settings.json`, and the managed config dir's
/// `settings.json`, in that order. Existence is not checked — an absent file
/// contributes nothing.
/// Test: `the_layer_list_covers_project_user_and_managed`.
pub fn settings_layers(
    project_root: &Path,
    home: Option<&Path>,
    managed_config_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut layers = vec![
        project_root.join(".claude").join("settings.json"),
        project_root.join(".claude").join("settings.local.json"),
    ];
    if let Some(home) = home {
        layers.push(home.join(".claude").join("settings.json"));
    }
    if let Some(managed) = managed_config_dir {
        layers.push(managed.join("settings.json"));
    }
    layers
}

/// The union of `claudeMdExcludes` across `layers`.
///
/// What: every string entry of the key in every readable layer. A file that is
/// absent, unreadable, not a JSON object, or whose key is not an array of
/// strings contributes nothing — the read can only UNDER-report, which errs
/// toward showing the operator a finding rather than hiding one.
/// Test: `merged_excludes_unions_every_layer`,
/// `an_unreadable_layer_contributes_nothing`.
pub fn merged_excludes(layers: &[PathBuf]) -> BTreeSet<String> {
    layers.iter().flat_map(|p| excludes_in(p)).collect()
}

/// The `claudeMdExcludes` entries in ONE settings file.
///
/// Test: see [`merged_excludes`].
fn excludes_in(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    value
        .get(EXCLUDES_KEY)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Is `path` covered by `excludes`?
///
/// Why: the key holds paths OR globs, so a string compare alone would report an
/// ancestor the operator has already excluded by pattern (`**/CLAUDE.md`) as an
/// outstanding finding.
/// What: `true` on an exact string match against the path's display form, or
/// when any entry compiles as a glob that matches it. An entry that does not
/// compile as a glob is compared as a literal only.
/// Test: `an_exact_path_entry_excludes_the_file`,
/// `a_glob_entry_excludes_the_file`, `an_unrelated_entry_does_not_match`.
pub fn is_excluded(path: &Path, excludes: &BTreeSet<String>) -> bool {
    let shown = path.display().to_string();
    excludes.iter().any(|entry| {
        entry == &shown
            || globset::Glob::new(entry)
                .map(|g| g.compile_matcher().is_match(path))
                .unwrap_or(false)
    })
}

/// Append one absolute path to `claudeMdExcludes` in `settings_path`.
///
/// Why (#7673): this is the ⚠️ repair. Renaming a file with real content in it
/// is not tm's call; adding one array entry is reversible, leaves the file
/// byte-for-byte alone, and is the mechanism Claude Code documents for exactly
/// this case.
/// What: reads `settings_path` (absent or empty starts from `{}`), appends
/// `absolute`'s display form to the array when it is not already listed, and
/// publishes through [`write_json_atomic`] — which stages and renames, taking
/// `<path>.bak` first. Every other key survives. An unreadable or non-object
/// file is [`ExcludeWrite::Refused`] and left exactly as it is.
/// Test: `adding_an_exclude_seeds_the_array`, `adding_is_idempotent`,
/// `other_keys_survive_the_write`, `an_unparseable_settings_file_is_refused`.
pub fn add_exclude(settings_path: &Path, absolute: &Path) -> ExcludeWrite {
    // Held across the read AND the write: atomicity alone stops corruption but
    // not a lost update, and two sessions on one machine race this file (#4072).
    let _guard = crate::core::claude_json_guard::lock();
    let raw = match std::fs::read_to_string(settings_path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return ExcludeWrite::Refused(err.to_string()),
    };
    let mut settings = match raw.as_deref() {
        None => serde_json::json!({}),
        Some(text) if text.trim().is_empty() => serde_json::json!({}),
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(value) if value.is_object() => value,
            Ok(_) => return ExcludeWrite::Refused("settings root is not an object".to_string()),
            Err(err) => return ExcludeWrite::Refused(err.to_string()),
        },
    };
    let Some(obj) = settings.as_object_mut() else {
        return ExcludeWrite::Refused("settings root is not an object".to_string());
    };

    let wanted = absolute.display().to_string();
    let entry = obj
        .entry(EXCLUDES_KEY.to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(list) = entry.as_array_mut() else {
        return ExcludeWrite::Refused(format!("`{EXCLUDES_KEY}` is not an array"));
    };
    if list.iter().any(|v| v.as_str() == Some(wanted.as_str())) {
        return ExcludeWrite::AlreadyListed;
    }
    list.push(serde_json::Value::String(wanted));

    match write_json_atomic(settings_path, &settings) {
        Ok(()) => ExcludeWrite::Added,
        Err(err) => ExcludeWrite::Refused(err.to_string()),
    }
}

#[cfg(test)]
#[path = "claude_md_excludes_tests.rs"]
mod tests;
