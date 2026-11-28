//! #10102: `tm run`'s sandbox step; #12077: it writes nothing.

use super::run_sandbox;
use crate::core::session_profile::SessionProfile;

/// A repo, home and tm-owned config dir under one canonical temp dir.
fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let repo = root.join("projects/a/repo");
    std::fs::create_dir_all(repo.join(".claude")).expect("mkdir");
    let home = root.join("home");
    let config = root.join("projects/a/claude-config");
    (tmp, repo, home, config)
}

/// #12077: `tm run` writes no sandbox key into the repo or its config dir.
#[test]
fn run_sandbox_writes_no_sandbox_settings_12077() {
    let (_tmp, repo, home, config) = fixture();
    run_sandbox(&repo, &home, &config, SessionProfile::Pm).expect("not refused");
    assert!(
        !repo.join(".claude/settings.json").exists(),
        "no project write"
    );
    assert!(!config.join("settings.json").exists(), "no user-tier write");
    assert!(!repo.join(".trusty-mpm/sandbox-roles.json").exists());
}

/// #12077: a local override that turns the sandbox off no longer refuses the
/// run, and is left as it was.
#[test]
fn run_sandbox_leaves_a_local_override_untouched_12077() {
    let (_tmp, repo, home, config) = fixture();
    let local = repo.join(".claude/settings.local.json");
    let planted = r#"{"sandbox":{"enabled":false}}"#;
    std::fs::write(&local, planted).expect("plant");
    run_sandbox(&repo, &home, &config, SessionProfile::Pm).expect("not refused");
    assert_eq!(std::fs::read_to_string(&local).expect("read"), planted);
}
