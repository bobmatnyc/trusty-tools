//! Unit tests for [`super`] (the #7262 build-tree repoint repair).
//!
//! Why: split out of `repoint.rs` to keep the production file focused, mirroring
//! the `{build_tree,cleanup}.rs` / `*_tests.rs` split already used here.
//! What: one fixture per corruption shape the issue enumerates — a build-tree
//! hook command, a build-tree `--pm-guard` command, a build-tree
//! `statusLine.command` — each asserted detect → repair → clean, plus the
//! idempotence pass, the unparseable-JSON refusal, and the refusal to repoint at
//! an ephemeral binary.
//! Test: this module IS the test suite for `super`.

use std::fs;
use std::path::{Path, PathBuf};

use super::super::build_tree::tests::{INCIDENT_EXE, incident_settings};
use super::super::cleanup::{build_tree_hook_commands, build_tree_statusline_command};
use super::*;

/// An installed binary path the repoint may write.
///
/// Why: [`repoint_settings_file`] refuses a relative or ephemeral target, so
/// every positive test needs one absolute path that no build-tree rule claims.
/// Resolving the real `tm` would make the assertions depend on the host.
/// What: a fixed absolute path outside any Cargo layout.
const INSTALLED: &str = "/usr/local/bin/tm";

/// Write `incident_settings()` into a fresh temp dir and hand back the path.
fn seed_incident() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    fs::write(&path, incident_settings().to_string()).expect("seed");
    (tmp, path)
}

/// The parsed contents of `path`.
fn read(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read")).expect("parse")
}

/// How many snapshot siblings of `path` exist.
fn snapshot_count(path: &Path) -> usize {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("a named file");
    fs::read_dir(path.parent().expect("a parent"))
        .expect("read_dir")
        .flatten()
        .filter(|e| {
            let entry = e.file_name();
            let entry = entry.to_string_lossy();
            entry != name && entry.starts_with(name) && entry.ends_with(".bak")
        })
        .count()
}

#[test]
fn repoint_settings_file_applies_and_snapshots() {
    // The three corruption shapes at once: six lifecycle ` hook` commands, one
    // ` hook --pm-guard`, and the `statusLine.command`.
    let (_tmp, path) = seed_incident();
    let before = read(&path);
    assert_eq!(build_tree_hook_commands(&before).len(), 2, "two distinct");
    assert!(build_tree_statusline_command(&before).is_some());

    let outcome = repoint_settings_file(&path, Path::new(INSTALLED), true)
        .expect("repoint")
        .expect("the incident file carries build-tree commands");

    // Seven hook entries carry the damage (six lifecycle + the guard).
    assert_eq!(outcome.hooks.len(), 7, "{:?}", outcome.hooks);
    assert_eq!(outcome.command_count(), 8, "plus the statusLine");
    assert!(
        outcome
            .statusline
            .as_ref()
            .is_some_and(|(_, new)| new == "/usr/local/bin/tm statusline"),
        "{:?}",
        outcome.statusline
    );
    let backup = outcome.backup_path.expect("apply takes a snapshot");
    assert_eq!(
        fs::read_to_string(&backup).expect("backup readable"),
        before.to_string(),
        "the snapshot must be the file as it was"
    );

    // Detect-then-repair-then-clean: nothing left for the classifiers to name.
    let after = read(&path);
    assert!(build_tree_hook_commands(&after).is_empty(), "{after}");
    assert!(build_tree_statusline_command(&after).is_none(), "{after}");
    assert_eq!(
        after["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
        serde_json::json!("/usr/local/bin/tm hook --pm-guard"),
        "the guard keeps its argv and gains a working path"
    );
    // Every other key survives untouched.
    assert_eq!(after["outputStyle"], serde_json::json!("trusty-mpm"));
    assert_eq!(
        after["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        serde_json::json!("trusty-memory prompt-context"),
    );
}

#[test]
fn repoint_settings_file_dry_run_changes_nothing() {
    let (_tmp, path) = seed_incident();
    let raw = fs::read_to_string(&path).expect("read");

    let outcome = repoint_settings_file(&path, Path::new(INSTALLED), false)
        .expect("repoint")
        .expect("the damage is reported in dry run too");

    assert_eq!(outcome.command_count(), 8);
    assert!(outcome.backup_path.is_none(), "dry run snapshots nothing");
    assert_eq!(fs::read_to_string(&path).expect("read"), raw);
    assert_eq!(snapshot_count(&path), 0);
}

#[test]
fn repoint_settings_file_is_idempotent() {
    // #7262: the second pass must be a no-op — no rewrite, no snapshot, and a
    // byte-identical file.
    let (_tmp, path) = seed_incident();
    repoint_settings_file(&path, Path::new(INSTALLED), true).expect("first pass");
    let after_first = fs::read_to_string(&path).expect("read");
    let snapshots_after_first = snapshot_count(&path);

    let second = repoint_settings_file(&path, Path::new(INSTALLED), true).expect("second pass");

    assert!(second.is_none(), "nothing left to repoint: {second:?}");
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        after_first,
        "the second pass must not rewrite the file"
    );
    assert_eq!(
        snapshot_count(&path),
        snapshots_after_first,
        "the second pass must not take a snapshot either"
    );
}

#[test]
fn repoint_settings_file_refuses_unparseable_json() {
    // Fail-closed: report, back up nothing, rewrite nothing.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let raw = "{ \"hooks\": { \"PreToolUse\": [ ,,, ";
    fs::write(&path, raw).expect("seed");

    let err = repoint_settings_file(&path, Path::new(INSTALLED), true)
        .expect_err("unparseable JSON must be an error, never a silent skip");

    assert!(
        err.to_string().contains("is not valid JSON"),
        "the message must name the reason: {err}"
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), raw);
    assert_eq!(
        snapshot_count(&path),
        0,
        "no backup of a file we cannot fix"
    );
}

#[test]
fn repoint_settings_file_refuses_a_non_object_document() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    fs::write(&path, "[1, 2, 3]").expect("seed");

    let err = repoint_settings_file(&path, Path::new(INSTALLED), true)
        .expect_err("a non-object settings document is not repairable");

    assert!(err.to_string().contains("not an object"), "{err}");
    assert_eq!(fs::read_to_string(&path).expect("read"), "[1, 2, 3]");
}

#[test]
fn repoint_settings_file_refuses_an_ephemeral_installed_binary() {
    // The repair must never write the class of path it exists to remove.
    let (_tmp, path) = seed_incident();
    let raw = fs::read_to_string(&path).expect("read");

    let err = repoint_settings_file(&path, Path::new(INCIDENT_EXE), true)
        .expect_err("an ephemeral target is refused");

    assert!(err.to_string().contains("not an installed"), "{err}");
    assert_eq!(fs::read_to_string(&path).expect("read"), raw);
}

#[test]
fn repoint_settings_file_refuses_a_relative_installed_binary() {
    let (_tmp, path) = seed_incident();
    let err = repoint_settings_file(&path, Path::new("tm"), true)
        .expect_err("a bare name is not an absolute installed path");
    assert!(err.to_string().contains("not an installed"), "{err}");
}

#[test]
fn repoint_settings_file_missing_file_is_noop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("settings.json");
    assert!(
        repoint_settings_file(&missing, Path::new(INSTALLED), true)
            .expect("a missing file is not an error")
            .is_none()
    );
}

#[test]
fn repoint_settings_file_leaves_foreign_entries_alone() {
    // A build-tree path alone is not enough — the argv must be one tm writes.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let body = serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [
                    { "type": "command", "command": format!("{INCIDENT_EXE} lint --fix") },
                    { "type": "command", "command": "claude-hook pre" },
                ]
            }]
        }
    })
    .to_string();
    fs::write(&path, &body).expect("seed");

    assert!(
        repoint_settings_file(&path, Path::new(INSTALLED), true)
            .expect("repoint")
            .is_none()
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), body);
}

#[test]
fn repoint_settings_file_ignores_an_installed_statusline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let body = serde_json::json!({
        "statusLine": { "type": "command", "command": "/usr/local/bin/tm statusline" }
    })
    .to_string();
    fs::write(&path, &body).expect("seed");

    assert!(
        repoint_settings_file(&path, Path::new(INSTALLED), true)
            .expect("repoint")
            .is_none()
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), body);
}

#[test]
fn build_tree_commands_in_lists_the_incident_commands() {
    let (_tmp, path) = seed_incident();
    let found = build_tree_commands_in(&path);
    assert_eq!(found.len(), 3, "two distinct hooks plus the statusLine");
    assert!(
        found.iter().all(|c| c.starts_with(INCIDENT_EXE)),
        "{found:?}"
    );
}

#[test]
fn build_tree_commands_in_is_empty_for_an_unparseable_file() {
    // The probe answers "should I speak up", and an unparseable file is the
    // repair's error to report, not the probe's.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    fs::write(&path, "not json").expect("seed");
    assert!(build_tree_commands_in(&path).is_empty());
    assert!(build_tree_commands_in(&tmp.path().join("absent.json")).is_empty());
}
