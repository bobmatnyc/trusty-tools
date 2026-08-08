//! Unit tests for startup hygiene's pure decision logic (#2177, #4961).
//!
//! Why: the update gate is the only thing standing between a daemon restart and
//! a user's uncommitted work. Its decision logic is deliberately a pure
//! function so every refusal path can be asserted without a git fixture; the
//! git-shelling half is covered by the integration tests in
//! `crates/trusty-mpm/tests/inproject_hygiene_test.rs`.
//! What: one test per `decide_update` refusal reason, plus the single
//! proceed case, plus the cheap non-git early returns.
//! Test: this file IS the test module.

use super::*;

#[test]
fn get_default_branch_returns_none_for_non_git() {
    // A non-git directory must return None cleanly.
    let tmp = std::env::temp_dir();
    assert!(get_default_branch(&tmp).is_none());
}

#[test]
fn run_hygiene_skips_missing_dir() {
    // A path that does not have .git must return Ok(()) immediately.
    let tmp = std::env::temp_dir();
    let result = run_hygiene_for_base(&tmp);
    assert!(
        result.is_ok(),
        "should skip non-git dir cleanly: {result:?}"
    );
}

#[test]
fn run_hygiene_for_all_bases_skips_missing_root() {
    // A non-existent repos root must complete without panicking.
    let missing = std::path::Path::new("/tmp/trusty-nonexistent-repos-root-hygiene-test");
    run_hygiene_for_all_bases(missing); // must not panic
}

#[test]
fn hygiene_opt_out_marker_detected() {
    // The marker short-circuits the sweep before any git command runs, so a
    // directory that merely LOOKS like a repo is enough to assert the path.
    let tmp = tempfile::TempDir::new().expect("temp dir");
    std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
    std::fs::write(tmp.path().join(HYGIENE_OPT_OUT_MARKER), "").expect("write marker");
    // Without the marker this would shell out to git against a bogus .git dir;
    // with it, the function returns before that happens.
    assert!(run_hygiene_for_base(tmp.path()).is_ok());
}

// ── decide_update: pure decision-logic unit tests (#2177, #4961) ──────────

#[test]
fn decide_update_ahead_skips() {
    // Any unpushed commit must refuse the update, even on a clean tree.
    match decide_update(Some("main"), "main", Some(1), Some(false)) {
        UpdateDecision::Skip(reason) => assert!(reason.contains("ahead")),
        UpdateDecision::Update => panic!("an ahead branch must never be updated"),
    }
}

#[test]
fn decide_update_dirty_skips() {
    // A dirty tree must refuse the update, even when not ahead.
    match decide_update(Some("main"), "main", Some(0), Some(true)) {
        UpdateDecision::Skip(reason) => assert!(reason.contains("uncommitted")),
        UpdateDecision::Update => panic!("a dirty tree must never be updated"),
    }
}

#[test]
fn decide_update_unknown_ahead_skips() {
    // No upstream / rev-list failure (ahead=None) must conservatively refuse,
    // regardless of the dirty state.
    match decide_update(Some("main"), "main", None, Some(false)) {
        UpdateDecision::Skip(_) => {}
        UpdateDecision::Update => panic!("unknown ahead-count must never be updated"),
    }
}

#[test]
fn decide_update_unknown_dirty_skips() {
    // A `git status` failure (dirty=None) must conservatively refuse,
    // regardless of the ahead-count.
    match decide_update(Some("main"), "main", Some(0), None) {
        UpdateDecision::Skip(_) => {}
        UpdateDecision::Update => panic!("unknown dirty-state must never be updated"),
    }
}

#[test]
fn decide_update_detached_head_skips() {
    // Detached HEAD: there is no branch to fast-forward, so refuse outright
    // rather than moving whatever ref happens to be nearby.
    match decide_update(None, "main", Some(0), Some(false)) {
        UpdateDecision::Skip(reason) => assert!(reason.contains("detached")),
        UpdateDecision::Update => panic!("a detached HEAD must never be updated"),
    }
}

#[test]
fn decide_update_non_default_branch_skips() {
    // #4961 second finding: ahead-count is measured against `origin/<branch>`
    // but the update targets `origin/<default>`. Off the default branch the
    // check proves nothing about the ref being moved, so refuse.
    match decide_update(Some("feature"), "main", Some(0), Some(false)) {
        UpdateDecision::Skip(reason) => {
            assert!(
                reason.contains("feature"),
                "reason names the branch: {reason}"
            );
            assert!(
                reason.contains("main"),
                "reason names the default: {reason}"
            );
        }
        UpdateDecision::Update => panic!("a non-default branch must never be moved to origin/main"),
    }
}

#[test]
fn decide_update_clean_and_even_updates() {
    // The only case that may proceed: on the default branch, zero ahead,
    // confirmed clean.
    assert_eq!(
        decide_update(Some("main"), "main", Some(0), Some(false)),
        UpdateDecision::Update
    );
}
