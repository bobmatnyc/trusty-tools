//! Unit tests for the default-deny plugin scope (#7422).
//!
//! Why: the decision is a map of booleans, so the tests that matter are the
//! ones asserting a `false` appears for a plugin nobody opted into, and that a
//! key tm did not write survives the merge.
//! What: enumeration, the deny/allow decision, and the merge discipline.
//! Test: this file.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::*;

/// Write a managed config dir carrying an installed-plugin index.
fn installed_index(dir: &Path, keys: &[&str]) {
    let mut plugins = Map::new();
    for key in keys {
        plugins.insert((*key).to_string(), json!([{"scope": "user"}]));
    }
    let path = dir.join("plugins");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("installed_plugins.json"),
        serde_json::to_string_pretty(&json!({"version": 2, "plugins": plugins})).unwrap(),
    )
    .unwrap();
}

#[test]
fn plugin_scope_reads_the_installed_index() {
    let tmp = TempDir::new().unwrap();
    installed_index(tmp.path(), &["aws-core@claude-plugins-official"]);

    assert_eq!(
        known_plugins(tmp.path()),
        vec!["aws-core@claude-plugins-official".to_owned()]
    );
}

#[test]
fn plugin_scope_unions_the_enabled_map() {
    let tmp = TempDir::new().unwrap();
    installed_index(tmp.path(), &["aws-core@m"]);
    std::fs::write(
        tmp.path().join("settings.json"),
        serde_json::to_string_pretty(&json!({"enabledPlugins": {"vercel@m": false}})).unwrap(),
    )
    .unwrap();

    assert_eq!(
        known_plugins(tmp.path()),
        vec!["aws-core@m".to_owned(), "vercel@m".to_owned()],
        "a plugin enabled but absent from the index must still be scopeable"
    );
}

#[test]
fn plugin_scope_tolerates_a_malformed_index() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("plugins")).unwrap();
    std::fs::write(
        tmp.path().join("plugins").join("installed_plugins.json"),
        "{ not json",
    )
    .unwrap();

    assert!(known_plugins(tmp.path()).is_empty());
}

#[test]
fn plugin_scope_denies_by_default() {
    let tmp = TempDir::new().unwrap();
    installed_index(tmp.path(), &["aws-core@m", "vercel@m"]);

    let scope = plugin_scope(tmp.path(), &[]);

    assert_eq!(scope.get("aws-core@m"), Some(&false));
    assert_eq!(scope.get("vercel@m"), Some(&false));
}

#[test]
fn plugin_scope_enables_an_opt_in() {
    let tmp = TempDir::new().unwrap();
    installed_index(tmp.path(), &["aws-core@m", "vercel@m"]);

    let scope = plugin_scope(tmp.path(), &["aws-core@m".to_owned()]);

    assert_eq!(scope.get("aws-core@m"), Some(&true));
    assert_eq!(scope.get("vercel@m"), Some(&false));
}

#[test]
fn opt_in_matches_a_bare_plugin_name() {
    let tmp = TempDir::new().unwrap();
    installed_index(tmp.path(), &["aws-core@claude-plugins-official"]);

    let scope = plugin_scope(tmp.path(), &["aws-core".to_owned()]);

    assert_eq!(
        scope.get("aws-core@claude-plugins-official"),
        Some(&true),
        "operators name a plugin without its marketplace id"
    );
}

#[test]
fn excluded_plugins_lists_only_the_denied() {
    let tmp = TempDir::new().unwrap();
    installed_index(tmp.path(), &["aws-core@m", "vercel@m"]);

    assert_eq!(
        excluded_plugins(tmp.path(), &["aws-core".to_owned()]),
        vec!["vercel@m".to_owned()]
    );
}

#[test]
fn merge_enabled_plugins_preserves_foreign_keys() {
    let mut existing = Map::new();
    existing.insert("hand-added@local".to_owned(), Value::Bool(true));
    existing.insert("aws-core@m".to_owned(), Value::Bool(true));
    let mut scope = BTreeMap::new();
    scope.insert("aws-core@m".to_owned(), false);

    let (merged, changed) = merge_enabled_plugins(Some(&existing), &scope);

    assert!(changed);
    assert_eq!(
        merged.get("hand-added@local"),
        Some(&Value::Bool(true)),
        "a key tm never enumerated is the operator's and must survive"
    );
    assert_eq!(merged.get("aws-core@m"), Some(&Value::Bool(false)));
}

#[test]
fn merge_enabled_plugins_reports_no_change_when_identical() {
    let mut existing = Map::new();
    existing.insert("aws-core@m".to_owned(), Value::Bool(false));
    let mut scope = BTreeMap::new();
    scope.insert("aws-core@m".to_owned(), false);

    let (_, changed) = merge_enabled_plugins(Some(&existing), &scope);

    assert!(
        !changed,
        "an unchanged project must take no write per launch"
    );
}
