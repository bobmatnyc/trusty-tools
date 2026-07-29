//! Tests for the `push_guard` doctor check (#2867).
//!
//! Why: this check is the ONLY thing that makes a missing guard discoverable
//! on an already-provisioned base clone, so a false `Ok` is worse than no
//! check at all — it converts silence into a positive all-clear.
//! What: real temp git repos (no mocks), one case per `GuardState` arm plus
//! the two not-applicable short-circuits.
//! Test: this file IS the test module.

use super::*;
use crate::core::push_guard::install_pre_push_guard;

/// A real, minimal git repo, or `None` when `git` is unavailable.
fn temp_repo() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = crate::test_support::hermetic_temp_dir();
    let path = dir.path().to_path_buf();
    let ok = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&path)
        .status()
        .ok()?;
    if !ok.success() {
        return None;
    }
    Some((dir, path))
}

#[test]
fn ok_when_no_project_dir() {
    let check = check_push_guard(None);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("not applicable"),
        "{}",
        check.message
    );
}

#[test]
fn ok_outside_a_git_repo() {
    let dir = crate::test_support::hermetic_temp_dir();
    let check = check_push_guard(Some(dir.path()));
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("not a git working tree"),
        "{}",
        check.message
    );
}

#[test]
fn warns_when_missing_and_names_the_retrofit_command() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let check = check_push_guard(Some(&repo));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.message.contains("tm repair push-guard"),
        "the warning must name the exact retrofit command; got: {}",
        check.message
    );
}

#[test]
fn ok_once_installed() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    install_pre_push_guard(&repo).expect("install");
    let check = check_push_guard(Some(&repo));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(check.message.contains("active"), "{}", check.message);
}

#[test]
fn warns_when_an_older_revision_is_installed() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let hooks = crate::core::push_guard::effective_hooks_dir(&repo).expect("hooks dir");
    std::fs::create_dir_all(&hooks).expect("mkdir hooks");
    std::fs::write(
        hooks.join("pre-push"),
        "#!/bin/sh\n# trusty-mpm-push-guard: v0\nexit 0\n",
    )
    .expect("seed old hook");

    let check = check_push_guard(Some(&repo));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.message.contains("older revision"),
        "{}",
        check.message
    );
}

#[test]
fn warns_on_a_foreign_hook_without_claiming_protection() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let hooks = crate::core::push_guard::effective_hooks_dir(&repo).expect("hooks dir");
    std::fs::create_dir_all(&hooks).expect("mkdir hooks");
    std::fs::write(hooks.join("pre-push"), "#!/bin/sh\n# husky\nexit 0\n").expect("seed foreign");

    let check = check_push_guard(Some(&repo));
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "a foreign hook means UNPROTECTED, which must never read as Ok"
    );
    assert!(check.message.contains("NOT installed"), "{}", check.message);
}
