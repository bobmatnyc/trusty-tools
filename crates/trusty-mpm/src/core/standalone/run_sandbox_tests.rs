//! #10102: `tm run`'s sandbox step.

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

/// #10102: `tm run` writes the PM policy into the repo and its config dir.
#[test]
fn run_sandbox_applies_and_verifies_the_pm_policy() {
    let (_tmp, repo, home, config) = fixture();
    run_sandbox(&repo, &home, &config, SessionProfile::Pm).expect("applies");
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.join(".claude/settings.json")).expect("settings"),
    )
    .expect("json");
    assert_eq!(settings["sandbox"]["enabled"], true, "{settings}");
    assert!(config.join("settings.json").exists());
}

/// #10102 Fail-Open Check: a repo whose sandbox does not verify refuses.
#[test]
fn run_sandbox_refuses_a_workspace_that_does_not_verify() {
    let (_tmp, repo, home, config) = fixture();
    std::fs::write(
        repo.join(".claude/settings.local.json"),
        r#"{"sandbox":{"enabled":false}}"#,
    )
    .expect("plant");
    let err = run_sandbox(&repo, &home, &config, SessionProfile::Pm).expect_err("refuses");
    assert!(err.to_string().contains("settings.local.json"), "{err}");
}
