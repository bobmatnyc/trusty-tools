//! Tests for [`super`] — the settings-vs-decision comparison behind #7678.
//!
//! Why: the check, the preview and the write all read one plan, so the plan's
//! own rules (what counts as drift, what is preserved, what is tolerated) are
//! proven once here rather than three times downstream.
//! What: absent / divergent / matching keys, foreign-key preservation, the
//! no-known-plugins case, malformed-file tolerance, and the invariant that the
//! drift list is empty exactly when the merge reports no change.
//! Test: this file.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::*;

/// Write an `installed_plugins.json` naming `keys` under a managed config dir.
fn installed(config_dir: &Path, keys: &[&str]) {
    let mut plugins = serde_json::Map::new();
    for key in keys {
        plugins.insert((*key).to_string(), json!([]));
    }
    std::fs::create_dir_all(config_dir.join("plugins")).unwrap();
    std::fs::write(
        config_dir.join("plugins").join("installed_plugins.json"),
        serde_json::to_string_pretty(&json!({ "plugins": plugins })).unwrap(),
    )
    .unwrap();
}

/// Write a project `.claude/settings.json` holding `body`.
fn project_settings(project: &Path, body: serde_json::Value) {
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(
        project.join(".claude").join("settings.json"),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();
}

#[test]
fn drift_names_an_absent_key() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("repo");
    let config = tmp.path().join("cfg");
    std::fs::create_dir_all(&project).unwrap();
    installed(&config, &["aws-core@market"]);

    let plan = plan_enabled_plugins_with_trust(&project, &config, false).unwrap();

    assert_eq!(plan.drift.len(), 1, "{:?}", plan.drift);
    assert_eq!(plan.drift[0].key, "aws-core@market");
    assert!(!plan.drift[0].want);
    assert_eq!(plan.drift[0].found, None);
    let described = plan.drift[0].describe();
    assert!(described.contains("aws-core@market"), "{described}");
    assert!(described.contains("absent"), "{described}");
}

#[test]
fn drift_names_a_divergent_key() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("repo");
    let config = tmp.path().join("cfg");
    installed(&config, &["aws-core@market"]);
    project_settings(
        &project,
        json!({"enabledPlugins": {"aws-core@market": true}}),
    );

    let plan = plan_enabled_plugins_with_trust(&project, &config, false).unwrap();

    assert_eq!(plan.drift.len(), 1, "{:?}", plan.drift);
    assert_eq!(plan.drift[0].found, Some(true));
    assert!(!plan.drift[0].want);
    let described = plan.drift[0].describe();
    assert!(described.contains("is true"), "{described}");
    assert!(described.contains("would be written false"), "{described}");
}

#[test]
fn plan_is_empty_when_the_file_already_matches() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("repo");
    let config = tmp.path().join("cfg");
    installed(&config, &["aws-core@market"]);
    project_settings(
        &project,
        json!({"enabledPlugins": {"aws-core@market": false}}),
    );

    let plan = plan_enabled_plugins_with_trust(&project, &config, false).unwrap();

    assert!(plan.drift.is_empty(), "{:?}", plan.drift);
}

#[test]
fn plan_preserves_foreign_keys() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("repo");
    let config = tmp.path().join("cfg");
    installed(&config, &["aws-core@market"]);
    project_settings(
        &project,
        json!({
            "outputStyle": "trusty-mpm",
            "enabledPlugins": {"hand-installed@elsewhere": true},
        }),
    );

    let plan = plan_enabled_plugins_with_trust(&project, &config, false).unwrap();

    assert_eq!(plan.merged["outputStyle"], json!("trusty-mpm"));
    assert_eq!(
        plan.merged["enabledPlugins"]["hand-installed@elsewhere"],
        json!(true),
        "a key tm never enumerated is the operator's and survives"
    );
    assert_eq!(
        plan.merged["enabledPlugins"]["aws-core@market"],
        json!(false)
    );
}

#[test]
fn plan_is_none_without_known_plugins() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("repo");
    let config = tmp.path().join("cfg");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&config).unwrap();

    assert!(plan_enabled_plugins_with_trust(&project, &config, false).is_none());
}

#[test]
fn plan_tolerates_a_malformed_settings_file() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("repo");
    let config = tmp.path().join("cfg");
    installed(&config, &["aws-core@market"]);
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(project.join(".claude").join("settings.json"), "{ not json").unwrap();

    let plan = plan_enabled_plugins_with_trust(&project, &config, false).unwrap();

    assert_eq!(plan.drift.len(), 1, "a broken file owes the whole map");
    assert_eq!(
        plan.merged["enabledPlugins"]["aws-core@market"],
        json!(false)
    );
}

#[test]
fn drift_agrees_with_the_merge_changed_flag() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("cfg");
    installed(&config, &["a@m", "b@m"]);

    for (label, body) in [
        ("absent", json!({})),
        ("partial", json!({"enabledPlugins": {"a@m": false}})),
        (
            "wrong-type",
            json!({"enabledPlugins": {"a@m": "false", "b@m": false}}),
        ),
        (
            "matching",
            json!({"enabledPlugins": {"a@m": false, "b@m": false}}),
        ),
    ] {
        let project = tmp.path().join(label);
        project_settings(&project, body);
        let plan = plan_enabled_plugins_with_trust(&project, &config, false).unwrap();

        let scope = crate::core::session_plugin_scope::plugin_scope(&config, &[]);
        let existing = read_settings_object(&plan.settings_path);
        let existing = existing.get("enabledPlugins").and_then(Value::as_object);
        let (_merged, changed) =
            crate::core::session_plugin_scope::merge_enabled_plugins(existing, &scope);

        assert_eq!(
            !plan.drift.is_empty(),
            changed,
            "{label}: the drift list and the merge's changed flag must never disagree"
        );
    }
}
