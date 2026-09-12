//! What a live project's settings still owe the session-scope write (#7678).
//!
//! Why: `prepare_session` writes the project-tier `enabledPlugins` default-deny
//! map ONCE, at launch. A session that was paused before that write existed —
//! or resumed across the upgrade that added it — keeps running against a
//! `.claude/settings.json` with no such key, so every user-tier plugin still
//! loads. The `session_scope` doctor check reported the DECISION ("plugins NOT
//! loaded: aws-agents, aws-core") while those two plugins were in fact loading
//! at roughly 2,600 tokens per turn, because nothing compared the decision
//! against the file. This module is that comparison.
//!
//! What: [`plan_enabled_plugins`] composes the exact settings object
//! `crate::core::session_launch::write_enabled_plugins` would write and names
//! every key the file on disk is missing or disagrees with. The writer is built
//! on it, the doctor check reads it, and `tm doctor --fix` repairs from it — so
//! the report and the write cannot disagree.
//! Test: the `tests` module below.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::core::session_plugin_scope::{ENABLED_PLUGINS_KEY, merge_enabled_plugins, plugin_scope};

/// One `enabledPlugins` key whose on-disk value is not what a launch would write.
///
/// Why: "missing" and "set to the opposite value" are different facts and the
/// operator needs both spelled out — the first is a project that never took the
/// write, the second is a hand edit or a stale decision.
/// What: the settings key, the value a launch would write, and the value found
/// on disk (`None` for absent, or for a non-boolean an operator wrote there).
/// Test: `drift_names_an_absent_key`, `drift_names_a_divergent_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginKeyDrift {
    /// The `enabledPlugins` key, e.g. `aws-core@claude-plugins-official`.
    pub key: String,
    /// What `prepare_session` would write for it.
    pub want: bool,
    /// What the project's settings file holds today, if anything readable.
    pub found: Option<bool>,
}

impl PluginKeyDrift {
    /// One clause naming this key and both values.
    ///
    /// Why: the doctor hint and the `--fix` preview must describe a key the
    /// same way, so the operator can match one to the other.
    /// What: `` `key` absent (would be written false) `` or
    /// `` `key` is true, would be written false ``.
    /// Test: `drift_names_an_absent_key`, `drift_names_a_divergent_key`.
    pub fn describe(&self) -> String {
        match self.found {
            None => format!("`{}` absent (would be written {})", self.key, self.want),
            Some(found) => format!("`{}` is {found} (would be written {})", self.key, self.want),
        }
    }
}

/// The project-tier settings write a launch would perform, and what it changes.
///
/// Why: one value carries both halves the two consumers need — the merged
/// object the writer serialises, and the per-key drift the check and the
/// preview render — so neither has to re-derive the other's half.
/// What: the settings file's path, the WHOLE settings object with the merged
/// `enabledPlugins` key set (every other key preserved), and the drift list.
/// An empty `drift` means the file already matches and no write is owed.
/// Test: `plan_is_empty_when_the_file_already_matches`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSettingsPlan {
    /// `<project>/.claude/settings.json`.
    pub settings_path: PathBuf,
    /// The full settings object to write, `enabledPlugins` merged in.
    pub merged: Value,
    /// Every key the file is missing or disagrees with, in scope order.
    pub drift: Vec<PluginKeyDrift>,
}

/// Plan the `enabledPlugins` write for one project, resolving its trust bit.
///
/// Why: the launch path, the doctor check and the repair all want the same
/// answer for a REAL project, and the trust lookup reads the operator's own
/// `~/.trusty-tools/trusty-mpm/` store, so it belongs at one call site.
/// What: [`plan_enabled_plugins_with_trust`] against
/// [`crate::core::project_trust::is_project_trusted`]. `None` when the managed
/// config dir knows about no plugins at all — there is then nothing to enumerate
/// and nothing to write, which is NOT the same as "nothing to change".
/// Test: covered through the trust-injected variant below.
pub fn plan_enabled_plugins(project_dir: &Path, config_dir: &Path) -> Option<PluginSettingsPlan> {
    plan_enabled_plugins_with_trust(
        project_dir,
        config_dir,
        crate::core::project_trust::is_project_trusted(project_dir),
    )
}

/// [`plan_enabled_plugins`] against an explicit trust decision.
///
/// Why: the hermetic seam, mirroring
/// [`crate::core::session_mcp_scope::granted_plugins_with_trust`] — the trust
/// bit lives under the operator's `$HOME`, so a test of the plan itself would
/// otherwise have to redirect it.
/// What: composes [`plugin_scope`] from the granted opt-in list, reads the
/// project's `.claude/settings.json` (tolerating an absent, unreadable or
/// non-object file as an empty object, exactly as the writer does), and reports
/// both the merged object and the per-key drift. A key holding a non-boolean
/// counts as divergent, because the merge replaces it with a boolean.
/// Test: `drift_names_an_absent_key`, `drift_names_a_divergent_key`,
/// `plan_is_empty_when_the_file_already_matches`,
/// `plan_preserves_foreign_keys`, `plan_is_none_without_known_plugins`,
/// `drift_agrees_with_the_merge_changed_flag`.
pub fn plan_enabled_plugins_with_trust(
    project_dir: &Path,
    config_dir: &Path,
    trusted: bool,
) -> Option<PluginSettingsPlan> {
    let opt_in = crate::core::session_mcp_scope::granted_plugins_with_trust(project_dir, trusted);
    let scope = plugin_scope(config_dir, &opt_in);
    if scope.is_empty() {
        return None;
    }

    let settings_path = project_dir.join(".claude").join("settings.json");
    let mut settings = read_settings_object(&settings_path);
    let existing = settings
        .get(ENABLED_PLUGINS_KEY)
        .and_then(Value::as_object)
        .cloned();

    // #7678: one clause per key, so the hint names what is actually wrong
    // rather than only that something is.
    let drift: Vec<PluginKeyDrift> = scope
        .iter()
        .filter_map(|(key, want)| {
            let found = existing
                .as_ref()
                .and_then(|map| map.get(key))
                .and_then(Value::as_bool);
            (found != Some(*want)).then(|| PluginKeyDrift {
                key: key.clone(),
                want: *want,
                found,
            })
        })
        .collect();

    let (merged, _changed) = merge_enabled_plugins(existing.as_ref(), &scope);
    settings[ENABLED_PLUGINS_KEY] = Value::Object(merged);

    Some(PluginSettingsPlan {
        settings_path,
        merged: settings,
        drift,
    })
}

/// Read a project settings file as a JSON object, tolerating every failure.
///
/// Why: this must match the writer's tolerance byte for byte, or the plan and
/// the write disagree about what "existing" means.
/// What: the parsed object, or an empty object for an absent, unreadable,
/// malformed, or non-object file.
/// Test: `plan_tolerates_a_malformed_settings_file`.
fn read_settings_object(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()))
}

#[cfg(test)]
#[path = "session_scope_drift_tests.rs"]
mod tests;
