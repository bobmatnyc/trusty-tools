//! Unit tests for [`super`] — launch-time foreign-harness detection.
//!
//! Why: the warning itself is a `warn!` a test cannot assert on without a
//! tracing subscriber, so every case here drives the pure detection functions
//! the logger calls.
//! What: a foreign-hooked directory, a clean one, a malformed settings file,
//! and the launch/workspace de-duplication.
//! Test: this module IS the test suite for `super`.

use super::*;

/// Write a `.claude/settings.json` under `project` and return the project dir.
fn write_settings(project: &Path, contents: &serde_json::Value) -> PathBuf {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("create .claude");
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(contents).expect("serialize fixture"),
    )
    .expect("write settings.json");
    project.to_path_buf()
}

/// A settings value wiring claude-mpm's hooks — the shape
/// `foreign_hook_event_names` recognises (mirrors the doctor probe's fixture).
fn claude_mpm_hooks_value() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "claude-mpm hooks fire SessionStart",
                    "timeout": 5
                }]
            }]
        }
    })
}

/// A checkout carrying claude-mpm hooks is reported.
///
/// Why: this is the finding `tm doctor` already produces; the launch path must
/// produce the same one at the moment it matters.
/// Test: this function IS the test.
#[test]
fn detects_foreign_hooks_in_launch_dir() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let project = write_settings(
        &tmp.path().join("foreign-project"),
        &claude_mpm_hooks_value(),
    );

    let found = foreign_harness_files(std::slice::from_ref(&project));

    assert_eq!(
        found,
        vec![project.join(".claude").join("settings.json")],
        "a claude-mpm-hooked settings file must be reported"
    );
}

/// A checkout with no foreign hooks reports nothing.
///
/// Why: a launch-time warning that fires on clean projects is one operators
/// learn to skip past.
/// Test: this function IS the test.
#[test]
fn clean_dir_reports_nothing() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let project = write_settings(
        &tmp.path().join("clean-project"),
        &serde_json::json!({ "outputStyle": "trusty-mpm" }),
    );

    assert!(
        foreign_harness_files(&[project]).is_empty(),
        "a settings file with no foreign hooks must produce no finding"
    );
}

/// A settings file that is not parseable JSON is skipped, never a panic.
///
/// Why: this runs on the launch path. An operator's malformed settings file must
/// not take the spawn down — the doctor probe owns reporting that file.
/// Test: this function IS the test.
#[test]
fn malformed_settings_file_is_skipped_not_panicked() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let project = tmp.path().join("broken-project");
    std::fs::create_dir_all(project.join(".claude")).expect("create .claude");
    std::fs::write(project.join(".claude").join("settings.json"), "{ not json")
        .expect("write malformed settings");

    assert!(
        foreign_harness_files(&[project]).is_empty(),
        "an unparseable settings file contributes no finding"
    );
}

/// A redirected launch scans both trees; an unredirected one scans a single tree.
///
/// Why: the launch directory is where inherited framework state accumulates, so
/// it has to be scanned even though the session runs elsewhere — and scanning it
/// twice when the two paths coincide would double every warning.
/// Test: this function IS the test.
#[test]
fn launch_and_workspace_dirs_are_deduplicated() {
    let launch = Path::new("/tmp/an-unmanaged-clone");
    let managed = Path::new("/tmp/projects/an-owner/a-repo");

    assert_eq!(
        launch_scan_dirs(launch, managed),
        vec![launch.to_path_buf(), managed.to_path_buf()],
        "a redirected launch scans the launch directory AND the managed checkout"
    );
    assert_eq!(
        launch_scan_dirs(managed, managed),
        vec![managed.to_path_buf()],
        "an unredirected launch scans one directory, not the same one twice"
    );
}
