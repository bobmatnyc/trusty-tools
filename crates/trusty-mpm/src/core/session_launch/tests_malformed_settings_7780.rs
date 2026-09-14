//! Call-site coverage for #7780: every launch writer that replaces a malformed
//! project `settings.json` preserves it first.
//!
//! Why a separate file rather than asserts inside `malformed_backup_tests.rs`:
//! the unit tests prove the loader; these prove it is WIRED — into all three
//! writers, and into `prepare_session` once rather than once per writer. The
//! reported incident was a file holding `{ broken` coming back as valid JSON
//! starting `{ "attribution": …`, so the end-to-end arm asserts the original
//! bytes, the copy, and the rewritten file together.
//! What:
//! - `merge_settings_backs_up_a_malformed_file_before_rewriting_it` — the
//!   output-style / auto-memory writer.
//! - `merge_settings_refuses_when_the_copy_cannot_be_written` — the fail-open
//!   check: a read-only `.claude/` still allows `fs::write` onto the EXISTING
//!   file, which is exactly how the pre-fix writer destroyed it; the copy is
//!   what cannot be made, and the rewrite must not happen without it.
//! - `write_project_hooks_backs_up_a_malformed_file_and_installs_the_pm_guard` —
//!   the #1977 guard is installed over damage rather than skipped.
//! - `write_enabled_plugins_backs_up_a_malformed_file_before_rewriting_it` —
//!   the third writer on the same path.
//! - `prepare_session_backs_up_a_malformed_settings_file_exactly_once` — the
//!   real path: one launch, one copy, every writer after the first seeing a
//!   repaired file.
//!
//! Test: this is the test module.

use super::tests::EnvVarGuard;
use super::*;
use tempfile::TempDir;

/// The pinned stable hook binary. `resolve_stable_hook_exe` accepts an absolute,
/// non-ephemeral path whose stem is one tm ships; it never stats the file, so a
/// host without `tm` installed still exercises the write.
const TEST_EXE: &str = "/usr/local/bin/tm";

/// The bytes from the reported incident.
const BROKEN: &[u8] = b"{ broken";

/// Seed `<project>/.claude/settings.json` with `body` and return its path.
fn seed_settings(project: &std::path::Path, body: &[u8]) -> std::path::PathBuf {
    let claude = project.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let path = claude.join("settings.json");
    std::fs::write(&path, body).unwrap();
    path
}

/// Every preserved copy in `<project>/.claude`, name-sorted.
fn copies(project: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(project.join(".claude"))
        .expect("readable .claude")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("settings.json.malformed-"))
        .collect();
    names.sort();
    names
}

/// Read `<project>/.claude/settings.json` as JSON, failing the test when it is
/// not an object.
fn read_settings(project: &std::path::Path) -> serde_json::Value {
    let text = std::fs::read_to_string(project.join(".claude").join("settings.json"))
        .expect("settings.json must exist after a write");
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("the rewritten file must be valid JSON");
    assert!(
        value.is_object(),
        "rewritten file must be an object: {text}"
    );
    value
}

/// Every `PreToolUse` command in a settings value.
fn pre_tool_use_commands(value: &serde_json::Value) -> Vec<String> {
    value["hooks"]["PreToolUse"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .filter_map(|g| g["hooks"][0]["command"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn merge_settings_backs_up_a_malformed_file_before_rewriting_it() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    seed_settings(project, BROKEN);

    settings::write_output_style(project, None).expect("the rewrite proceeds after the copy");

    assert_eq!(copies(project).len(), 1, "{:?}", copies(project));
    assert_eq!(
        std::fs::read(project.join(".claude").join(&copies(project)[0])).unwrap(),
        BROKEN,
        "the copy must hold the ORIGINAL bytes"
    );
    assert!(
        read_settings(project)["outputStyle"].is_string(),
        "the rewrite must still happen"
    );
}

/// The Fail-Open Check. A read-only `.claude/` denies creating the copy while
/// still allowing `fs::write` onto the existing `settings.json` — the exact
/// shape in which the pre-#7780 writer destroyed it. Unix-only: the directory
/// permission bit is the portable way to express that.
///
/// #7762: the settings lock's sidecar is pre-created before the mode change. A
/// non-writable directory refuses a new entry, so without it the writer would
/// fail at the lock rather than at the copy this test is about; opening an
/// EXISTING file for write needs permission on the file, not its directory.
#[cfg(unix)]
#[test]
fn merge_settings_refuses_when_the_copy_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let path = seed_settings(project, BROKEN);
    let claude = project.join(".claude");

    std::fs::write(claude.join("settings.json.lock"), b"").expect("pre-create the lock sidecar");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = settings::write_output_style(project, None);
    // Restore before asserting, so a failed assertion still leaves a removable
    // temp dir behind.
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();

    let err = result.expect_err("a rewrite whose copy failed must not proceed");
    assert!(
        matches!(err, PrepError::SettingsBackup { .. }),
        "unexpected error: {err}"
    );
    assert!(!err.is_fatal(), "the session must still launch");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        BROKEN,
        "the original must be left exactly as it was"
    );
}

#[test]
fn write_project_hooks_backs_up_a_malformed_file_and_installs_the_pm_guard() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    seed_settings(project, BROKEN);

    settings::write_project_hooks(project, Some(std::path::Path::new(TEST_EXE)), true, false)
        .expect("the hooks write proceeds after the copy");

    assert_eq!(copies(project).len(), 1, "{:?}", copies(project));
    assert_eq!(
        std::fs::read(project.join(".claude").join(&copies(project)[0])).unwrap(),
        BROKEN
    );
    let commands = pre_tool_use_commands(&read_settings(project));
    assert!(
        commands.iter().any(|c| c.ends_with(" hook --pm-guard")),
        "the PM guard must be installed over a malformed file: {commands:?}"
    );
}

#[test]
fn write_enabled_plugins_backs_up_a_malformed_file_before_rewriting_it() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("repo");
    let config = tmp.path().join("cfg");
    std::fs::create_dir_all(config.join("plugins")).unwrap();
    std::fs::write(
        config.join("plugins").join("installed_plugins.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "plugins": { "aws-core@market": [] }
        }))
        .unwrap(),
    )
    .unwrap();
    seed_settings(&project, BROKEN);

    settings::write_enabled_plugins_with_trust(&project, Some(&config), false)
        .expect("the plugin write proceeds after the copy");

    assert_eq!(copies(&project).len(), 1, "{:?}", copies(&project));
    assert_eq!(
        std::fs::read(project.join(".claude").join(&copies(&project)[0])).unwrap(),
        BROKEN
    );
    assert_eq!(
        read_settings(&project)["enabledPlugins"]["aws-core@market"],
        serde_json::json!(false)
    );
}

/// The real path. One `prepare_session` over a malformed file must produce
/// exactly ONE copy — the later writers see a repaired file, so the guarantee is
/// per launch, not per writer — and must still install the PM guard.
#[test]
#[serial_test::serial]
fn prepare_session_backs_up_a_malformed_settings_file_exactly_once() {
    let tmp_home = TempDir::new().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    seed_settings(project, BROKEN);

    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;
    std::fs::create_dir_all(&fw.agents).unwrap();
    std::fs::write(
        fw.agents.join("base-engineer.md"),
        "---\nname: base-engineer\nrole: base-engineer\n---\n\n# Base Eng\n\nBASE.\n",
    )
    .unwrap();

    prepare_session_with_repo_url_and_exe(&fw, project, None, Some(std::path::Path::new(TEST_EXE)))
        .expect("preparation succeeds over a malformed settings file");

    let names = copies(project);
    assert_eq!(names.len(), 1, "one launch takes one copy: {names:?}");
    assert_eq!(
        std::fs::read(project.join(".claude").join(&names[0])).unwrap(),
        BROKEN,
        "the copy must hold the bytes the launch found"
    );

    let after = read_settings(project);
    assert!(
        after["outputStyle"].is_string(),
        "the launch must still write its own keys: {after}"
    );
    let commands = pre_tool_use_commands(&after);
    assert!(
        commands.iter().any(|c| c.ends_with(" hook --pm-guard")),
        "the PM guard must survive a malformed file: {commands:?}"
    );
}
