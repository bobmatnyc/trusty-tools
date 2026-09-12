//! Tests for the `claudeMdExcludes` reader and writer (#7673).
//!
//! Split out with `#[path]` so `claude_md_excludes.rs` stays under the 500-SLOC
//! production cap.

use super::*;
use tempfile::TempDir;

fn write_settings(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn the_settings_key_is_frozen() {
    assert_eq!(EXCLUDES_KEY, "claudeMdExcludes");
}

#[test]
fn the_layer_list_covers_project_user_and_managed() {
    let project = Path::new("/srv/acme");
    let home = Path::new("/Users/ada");
    let managed = Path::new("/Users/ada/.trusty-tools/claude-config");

    let layers = settings_layers(project, Some(home), Some(managed));

    assert!(layers.contains(&project.join(".claude").join("settings.json")));
    assert!(layers.contains(&project.join(".claude").join("settings.local.json")));
    assert!(layers.contains(&home.join(".claude").join("settings.json")));
    assert!(layers.contains(&managed.join("settings.json")));
}

#[test]
fn merged_excludes_unions_every_layer() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a/.claude/settings.json");
    let b = tmp.path().join("b/.claude/settings.local.json");
    write_settings(&a, r#"{"claudeMdExcludes": ["/one/CLAUDE.md"]}"#);
    write_settings(&b, r#"{"claudeMdExcludes": ["/two/CLAUDE.md"]}"#);

    let merged = merged_excludes(&[a, b]);

    assert!(merged.contains("/one/CLAUDE.md"));
    assert!(merged.contains("/two/CLAUDE.md"));
}

#[test]
fn an_unreadable_layer_contributes_nothing() {
    let tmp = TempDir::new().unwrap();
    let bad = tmp.path().join(".claude/settings.json");
    write_settings(&bad, "{ not json");

    assert!(merged_excludes(&[bad, tmp.path().join("absent.json")]).is_empty());
}

#[test]
fn an_exact_path_entry_excludes_the_file() {
    let excludes: BTreeSet<String> = ["/Users/ada/CLAUDE.md".to_string()].into_iter().collect();
    assert!(is_excluded(Path::new("/Users/ada/CLAUDE.md"), &excludes));
}

#[test]
fn a_glob_entry_excludes_the_file() {
    let excludes: BTreeSet<String> = ["/Users/**/CLAUDE.md".to_string()].into_iter().collect();
    assert!(is_excluded(
        Path::new("/Users/ada/work/CLAUDE.md"),
        &excludes
    ));
}

#[test]
fn an_unrelated_entry_does_not_match() {
    let excludes: BTreeSet<String> = ["/opt/other/CLAUDE.md".to_string()].into_iter().collect();
    assert!(!is_excluded(Path::new("/Users/ada/CLAUDE.md"), &excludes));
}

/// FAILS BEFORE THIS CHANGE: there was no writer for this key at all.
#[test]
fn adding_an_exclude_seeds_the_array() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join(".claude/settings.local.json");
    let target = Path::new("/Users/ada/CLAUDE.md");

    assert_eq!(add_exclude(&settings, target), ExcludeWrite::Added);

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        value[EXCLUDES_KEY],
        serde_json::json!(["/Users/ada/CLAUDE.md"])
    );
}

#[test]
fn adding_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join(".claude/settings.local.json");
    let target = Path::new("/Users/ada/CLAUDE.md");

    assert_eq!(add_exclude(&settings, target), ExcludeWrite::Added);
    assert_eq!(add_exclude(&settings, target), ExcludeWrite::AlreadyListed);

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(value[EXCLUDES_KEY].as_array().unwrap().len(), 1);
}

#[test]
fn other_keys_survive_the_write() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join(".claude/settings.local.json");
    write_settings(
        &settings,
        r#"{"permissions": {"allow": ["Bash(ls:*)"]}, "claudeMdExcludes": ["/x/CLAUDE.md"]}"#,
    );

    assert_eq!(
        add_exclude(&settings, Path::new("/Users/ada/CLAUDE.md")),
        ExcludeWrite::Added
    );

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        value["permissions"]["allow"],
        serde_json::json!(["Bash(ls:*)"])
    );
    assert_eq!(value[EXCLUDES_KEY].as_array().unwrap().len(), 2);
}

#[test]
fn an_unparseable_settings_file_is_refused() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join(".claude/settings.local.json");
    write_settings(&settings, "{ not json");

    let outcome = add_exclude(&settings, Path::new("/Users/ada/CLAUDE.md"));

    assert!(matches!(outcome, ExcludeWrite::Refused(_)), "{outcome:?}");
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        "{ not json",
        "a file this cannot parse is never rewritten"
    );
}
