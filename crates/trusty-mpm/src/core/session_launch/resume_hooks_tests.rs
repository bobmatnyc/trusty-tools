//! Tests for the resume/relaunch project-hook re-merge (issue #7490).
//!
//! Why: the incident was a settings file whose `SessionStart` array carried
//! only `trusty-memory inbox-check`, in a project that had been resumed
//! repeatedly. Every test here is written against that exact shape, and the
//! first one fails on the pre-#7490 code because nothing on a resume path
//! called the merge at all.
//! What: the merge (adds the missing group, preserves the foreign entry, and
//! leaves an already-complete file byte-identical) plus the read-only gap
//! predicate the `tm doctor` check and its repair share.
//! Test: this file IS the test suite.

use super::*;
use crate::test_support::STABLE_HOOK_EXE;

/// The pre-#7490 incident file: `SessionStart` wired to the memory hook only,
/// the PM guard present under `PreToolUse`, no lifecycle group anywhere.
fn incident_settings() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "/usr/local/bin/tm hook --pm-guard",
                    "timeout": 5
                }]
            }],
            "SessionStart": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-memory inbox-check",
                    "timeout": 60
                }]
            }]
        }
    })
}

/// Write `value` to `<dir>/.claude/settings.json`.
fn seed_settings(dir: &std::path::Path, value: &serde_json::Value) -> std::path::PathBuf {
    let claude = dir.join(".claude");
    std::fs::create_dir_all(&claude).expect("create .claude");
    let path = claude.join("settings.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(value).expect("serialize settings"),
    )
    .expect("write settings");
    path
}

/// Read `<dir>/.claude/settings.json` back as JSON.
fn read_settings(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read settings"))
        .expect("settings is valid JSON")
}

/// Every `hooks.<event>[*].hooks[*].command` under one event.
fn commands_for(val: &serde_json::Value, event: &str) -> Vec<String> {
    val["hooks"][event]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .filter_map(|g| g.get("hooks").and_then(serde_json::Value::as_array))
                .flatten()
                .filter_map(|e| e.get("command").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// THE #7490 REGRESSION TEST — fails on the pre-fix code.
///
/// Why: before #7490 no resume or relaunch path called any project-hook
/// writer, so this settings file kept its memory-only `SessionStart` forever
/// and `tm hook`'s `SessionStart` arm never fired. Driving the merge against
/// the incident file is the smallest thing that proves the gap is closed.
/// What: seeds the incident shape, runs [`ensure_project_hooks`] with a pinned
/// installed-looking binary, and asserts `SessionStart` now carries a
/// `<exe> hook` command.
/// Test: itself.
#[test]
fn resume_merge_adds_the_sessionstart_group_to_an_existing_file() {
    let project = tempfile::tempdir().expect("tempdir");
    let path = seed_settings(project.path(), &incident_settings());

    ensure_project_hooks(project.path(), Some(std::path::Path::new(STABLE_HOOK_EXE)))
        .expect("the merge must succeed against a pinned installed binary");

    let after = read_settings(&path);
    let commands = commands_for(&after, "SessionStart");
    assert!(
        commands
            .iter()
            .any(|c| c == &format!("{STABLE_HOOK_EXE} hook")),
        "SessionStart must gain the tm-hook lifecycle entry: {commands:?}"
    );
}

/// The merge never displaces the entry the project already had.
///
/// Why: `SessionStart` is shared with `trusty-memory inbox-check`, and a merge
/// that replaced the array rather than appending to it would silently disable
/// the inbox check — trading one missing hook for another.
/// What: as above, then asserts the memory command survives.
/// Test: itself.
#[test]
fn resume_merge_preserves_the_inbox_check_entry() {
    let project = tempfile::tempdir().expect("tempdir");
    let path = seed_settings(project.path(), &incident_settings());

    ensure_project_hooks(project.path(), Some(std::path::Path::new(STABLE_HOOK_EXE)))
        .expect("merge");

    let commands = commands_for(&read_settings(&path), "SessionStart");
    assert!(
        commands.iter().any(|c| c == "trusty-memory inbox-check"),
        "the project's own SessionStart entry must survive the merge: {commands:?}"
    );
}

/// A file that already carries every group is left BYTE-identical (the #7244
/// rule).
///
/// Why: this merge now runs on every spawn, resume and in-place relaunch. If
/// it rewrote the file each time, every session would take a snapshot and the
/// three retained snapshots would all be copies of the current file — the one
/// prior state worth keeping pushed out by an archive of nothing.
/// What: runs the merge twice, comparing the file's raw bytes after each.
/// Test: itself.
#[test]
fn resume_merge_leaves_a_complete_file_byte_identical() {
    let project = tempfile::tempdir().expect("tempdir");
    let path = seed_settings(project.path(), &incident_settings());
    let exe = Some(std::path::Path::new(STABLE_HOOK_EXE));
    // #5040's seam: a temp base, never the process-global `$HOME` — the two
    // calls must read the SAME `[hooks]` / `[divert]` layers, and a
    // concurrently-running test that redirects `$HOME` would change them
    // between the two reads.
    let base = tempfile::tempdir().expect("tempdir");
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace_under(
        base.path(),
        project.path(),
    );

    ensure_project_hooks_with(&fw, project.path(), exe).expect("first merge");
    let first = std::fs::read(&path).expect("read after first merge");

    ensure_project_hooks_with(&fw, project.path(), exe).expect("second merge");
    let second = std::fs::read(&path).expect("read after second merge");

    assert_eq!(
        first, second,
        "a settings file that already carries every group must not be rewritten"
    );
}

/// The gap predicate names the event the incident file is missing.
#[test]
fn missing_events_names_a_sessionstart_gap() {
    let gaps = missing_lifecycle_hook_events(&incident_settings());
    assert!(
        gaps.iter().any(|e| e == "SessionStart"),
        "SessionStart must be reported as missing: {gaps:?}"
    );
    assert!(
        !gaps.is_empty() && gaps.iter().all(|e| e != "UserPromptSubmit"),
        "only the six lifecycle events are in scope: {gaps:?}"
    );
}

/// A file the merge has just produced has no gaps left.
#[test]
fn missing_events_is_empty_for_a_complete_file() {
    let project = tempfile::tempdir().expect("tempdir");
    let path = seed_settings(project.path(), &incident_settings());
    ensure_project_hooks(project.path(), Some(std::path::Path::new(STABLE_HOOK_EXE)))
        .expect("merge");

    let gaps = missing_lifecycle_hook_events(&read_settings(&path));
    assert!(
        gaps.is_empty(),
        "the merged file must have no gaps: {gaps:?}"
    );
}

/// A project tm never provisioned reports nothing.
///
/// Why: the check must never tell a foreign project to adopt tm's hooks, and
/// `--fix` must never write them there — that would undo, on the same run,
/// exactly what `hooks_contamination`'s repair removed.
/// What: a settings value carrying only a claude-mpm hook yields no gaps.
/// Test: itself.
#[test]
fn missing_events_is_empty_for_a_file_tm_never_provisioned() {
    let foreign = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "claude-mpm hook",
                    "timeout": 5
                }]
            }]
        }
    });
    let gaps = missing_lifecycle_hook_events(&foreign);
    assert!(
        gaps.is_empty(),
        "a project tm never provisioned owes no tm hook group: {gaps:?}"
    );
}
