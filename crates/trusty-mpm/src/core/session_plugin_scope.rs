//! Default-deny Claude Code plugin scoping for a tm-managed project (#7422).
//!
//! Why: a plugin installed once into the tm-managed `CLAUDE_CONFIG_DIR` loads
//! its whole skill catalog into every session on the host. The `aws-core` and
//! `aws-agents` pair alone contributed 32 skills and roughly 5.9k tokens to
//! sessions that never touch AWS. Claude Code has no per-invocation flag for
//! this — `enabledPlugins` is settings-only — so the lever is the project's own
//! `.claude/settings.json`, which outranks the user tier.
//!
//! What: [`plugin_scope`] enumerates every plugin the managed config dir knows
//! about and maps each to `true` when the project's committed config opts it in
//! and `false` otherwise. [`merge_enabled_plugins`] folds that map into an
//! existing `enabledPlugins` object, owning only the keys it enumerated and
//! preserving any other key an operator put there.
//!
//! A name in `[session] plugins` matches either the full `<plugin>@<market>`
//! key or the bare `<plugin>` half, because that is how the operator refers to
//! it (`claude plugin install aws-core`).
//!
//! Test: `session_plugin_scope_tests.rs`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

/// Path, under the managed config dir, of Claude Code's installed-plugin index.
///
/// Why: one literal shared by the enumeration and its tests.
/// What: `plugins/installed_plugins.json`.
/// Test: `plugin_scope_reads_the_installed_index`.
pub const INSTALLED_PLUGINS_PATH: &[&str] = &["plugins", "installed_plugins.json"];

/// The settings key Claude Code reads a plugin allowlist from.
///
/// Why: written by tm into the PROJECT tier and read by tm from the managed
/// USER tier, so both spellings come from here.
/// What: `enabledPlugins`.
/// Test: `merge_enabled_plugins_preserves_foreign_keys`.
pub const ENABLED_PLUGINS_KEY: &str = "enabledPlugins";

/// Every plugin name the managed config dir knows about.
///
/// Why: the project tier can only turn OFF a plugin it names, so the write has
/// to enumerate the full installed set rather than only the enabled one — a
/// plugin installed but not yet enabled would otherwise become enabled the
/// moment an operator flipped the user tier.
/// What: the union of `plugins/installed_plugins.json`'s `plugins` keys and
/// `settings.json`'s `enabledPlugins` keys, sorted and deduped. An absent,
/// unreadable, or malformed file contributes nothing and is not an error.
/// Test: `plugin_scope_reads_the_installed_index`,
/// `plugin_scope_unions_the_enabled_map`.
pub fn known_plugins(config_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();

    let mut index = config_dir.to_path_buf();
    for segment in INSTALLED_PLUGINS_PATH {
        index.push(segment);
    }
    if let Some(map) = read_object(&index, "plugins") {
        names.extend(map.keys().cloned());
    }
    if let Some(map) = read_object(&config_dir.join("settings.json"), ENABLED_PLUGINS_KEY) {
        names.extend(map.keys().cloned());
    }

    names.sort();
    names.dedup();
    names
}

/// Read one top-level object out of a JSON file, tolerating every failure.
///
/// Why: these files are operator state read on the launch path; a missing or
/// hand-broken one must cost the launch nothing.
/// What: `Some(map)` when the file parses and `key` maps to an object; `None`
/// otherwise.
/// Test: `plugin_scope_tolerates_a_malformed_index`.
fn read_object(path: &Path, key: &str) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    parsed.get(key)?.as_object().cloned()
}

/// Whether an opt-in list names a plugin key.
///
/// Why: operators name a plugin `aws-core`, while the settings key is
/// `aws-core@claude-plugins-official`. Accepting both means the committed
/// config does not have to encode a marketplace id that can change.
/// What: `true` when `opt_in` contains the full key or the segment before the
/// first `@`.
/// Test: `opt_in_matches_a_bare_plugin_name`.
fn is_opted_in(key: &str, opt_in: &[String]) -> bool {
    let bare = key.split('@').next().unwrap_or(key);
    opt_in.iter().any(|n| n == key || n == bare)
}

/// Decide each known plugin's enabled state for one project.
///
/// Why: this is the default-deny decision for plugins, kept separate from the
/// settings write so `tm doctor` can report it without touching a file.
/// What: every name from [`known_plugins`] mapped to `true` when `opt_in` names
/// it and `false` otherwise. Sorted, because it is written into a committed-ish
/// settings file and a stable order keeps the diff readable.
/// Test: `plugin_scope_denies_by_default`, `plugin_scope_enables_an_opt_in`.
pub fn plugin_scope(config_dir: &Path, opt_in: &[String]) -> BTreeMap<String, bool> {
    known_plugins(config_dir)
        .into_iter()
        .map(|key| {
            let enabled = is_opted_in(&key, opt_in);
            (key, enabled)
        })
        .collect()
}

/// The plugin keys this project's sessions will NOT load.
///
/// Why: `tm doctor` and `tm session instructions` report the excluded set, and
/// deriving it from [`plugin_scope`] means the report and the write can never
/// disagree.
/// What: the sorted keys whose scope decision is `false`.
/// Test: `excluded_plugins_lists_only_the_denied`.
pub fn excluded_plugins(config_dir: &Path, opt_in: &[String]) -> Vec<String> {
    plugin_scope(config_dir, opt_in)
        .into_iter()
        .filter_map(|(key, enabled)| (!enabled).then_some(key))
        .collect()
}

/// Fold a scope decision into an existing `enabledPlugins` object.
///
/// Why: tm owns only the keys it enumerated. A key an operator added by hand
/// for a plugin tm cannot see — a `--plugin-dir` install, a marketplace tm's
/// managed dir does not index — is theirs, and survives.
/// What: returns the merged object and whether it differs from `existing`.
/// Every key in `scope` is set to its decided value; every other key in
/// `existing` is carried through unchanged.
/// Test: `merge_enabled_plugins_preserves_foreign_keys`,
/// `merge_enabled_plugins_reports_no_change_when_identical`.
pub fn merge_enabled_plugins(
    existing: Option<&Map<String, Value>>,
    scope: &BTreeMap<String, bool>,
) -> (Map<String, Value>, bool) {
    let mut merged = existing.cloned().unwrap_or_default();
    for (key, enabled) in scope {
        merged.insert(key.clone(), Value::Bool(*enabled));
    }
    let changed = existing != Some(&merged);
    (merged, changed)
}

#[cfg(test)]
#[path = "session_plugin_scope_tests.rs"]
mod tests;
