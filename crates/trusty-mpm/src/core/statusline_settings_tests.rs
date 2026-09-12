//! Tests for [`super::ensure_statusline_entry_in`] (#7617).
//!
//! Why: this is the one writer every settings tier goes through, so its
//! seed / repair / keep / refuse quadrants are what guarantee the `💸` segment
//! is wired on a fresh install and stays wired after an upgrade.
//! Test: this file IS the test module.

use super::*;

/// A settings file holding `value` under `statusLine`, in a fresh tempdir.
fn settings_with(value: serde_json::Value) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({ "statusLine": value })).unwrap(),
    )
    .expect("seed settings");
    (dir, path)
}

/// Read `statusLine.command` back off disk.
fn command_in(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["statusLine"]["command"].as_str().map(str::to_owned)
}

/// Why (#7617 closure 1): a fresh install must come away with the entry, which
/// is the whole "core setup, guaranteed by the framework" ruling.
/// Test: itself.
#[test]
fn a_fresh_file_is_seeded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("settings.json");

    let outcome = ensure_statusline_entry_in(&path);

    assert_eq!(outcome, StatuslineWrite::Seeded);
    assert!(outcome.wrote());
    assert!(
        command_in(&path).is_some_and(|cmd| cmd.ends_with(" statusline")),
        "the seeded command must invoke the statusline subcommand"
    );
}

/// Why (#2229, #7262): the disappearance class this rule exists for is a command
/// pointing at a binary that no longer exists — a Cargo build tree that was
/// cleaned, or an install that moved.
/// Test: itself.
#[test]
fn a_stale_entry_is_repaired() {
    let (_dir, path) = settings_with(serde_json::json!({
        "type": "command",
        "command": "/definitely/not/here/tm statusline",
        "padding": 0
    }));

    let outcome = ensure_statusline_entry_in(&path);

    assert_eq!(outcome, StatuslineWrite::Repaired);
    assert_ne!(
        command_in(&path).as_deref(),
        Some("/definitely/not/here/tm statusline"),
        "a command whose binary is gone must be repointed"
    );
}

/// Why: the rule must never cost an operator their own status bar. A command
/// pointing at a binary that EXISTS is theirs, whatever it runs.
/// Test: itself.
#[test]
fn a_customized_entry_is_kept() {
    let mine = format!("{} --custom", std::env::current_exe().unwrap().display());
    let (_dir, path) = settings_with(serde_json::json!({
        "type": "command",
        "command": mine.clone(),
        "padding": 0
    }));

    let outcome = ensure_statusline_entry_in(&path);

    assert_eq!(outcome, StatuslineWrite::Unchanged);
    assert!(!outcome.wrote());
    assert_eq!(command_in(&path).as_deref(), Some(mine.as_str()));
}

/// Why (Fail-Open Check): a settings file that cannot be parsed is not an empty
/// one. Starting from `{}` would silently delete every other key the operator
/// has — hooks, permissions, MCP servers — to fix a status bar.
/// Test: itself.
#[test]
fn an_unparseable_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{ this is not json").expect("seed");

    let outcome = ensure_statusline_entry_in(&path);

    assert!(
        matches!(outcome, StatuslineWrite::Refused(_)),
        "got {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{ this is not json",
        "a refused file must be left byte-for-byte alone"
    );
}

/// Why: this runs on every launch and every resume. A second call must not
/// rewrite the file, or every session bumps the mtime of the operator's
/// settings for nothing.
/// Test: itself.
#[test]
fn seeding_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");

    assert_eq!(ensure_statusline_entry_in(&path), StatuslineWrite::Seeded);
    assert_eq!(
        ensure_statusline_entry_in(&path),
        StatuslineWrite::Unchanged
    );
}

/// Why: the seed writes the whole file back, so every other key has to survive
/// the round trip — this is the assertion that a repair is not a reset.
/// Test: itself.
#[test]
fn other_keys_survive_the_seed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "outputStyle": "trusty-mpm",
            "permissions": { "allow": ["Bash(ls:*)"] }
        }))
        .unwrap(),
    )
    .expect("seed");

    assert_eq!(ensure_statusline_entry_in(&path), StatuslineWrite::Seeded);

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["outputStyle"], "trusty-mpm");
    assert_eq!(value["permissions"]["allow"][0], "Bash(ls:*)");
}
