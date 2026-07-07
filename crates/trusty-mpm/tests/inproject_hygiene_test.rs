//! Integration tests for the startup-hygiene data-loss fix (#2177).
//!
//! Why: `run_hygiene_for_base` used to run `git reset --hard origin/<branch>`
//! unconditionally on every daemon startup, discarding committed-but-unpushed
//! work and uncommitted changes in real base clones. These tests drive the
//! real function against real temporary git repositories (an `origin` repo and
//! a `base` clone of it) to prove the safety gate: a base clone ahead of
//! origin, or with a dirty working tree, must survive `run_hygiene_for_base`
//! completely untouched, while a clean/behind clone is still fast-forwarded
//! to origin as before, and a recovery ref is written before any reset that
//! does proceed.
//! What: a small git-repo-pair harness (`origin` + `base` clone, both real
//! on-disk git repos under a `tempfile::TempDir`) plus one test per safety
//! scenario.
//! Test: this file IS the test suite; run with `cargo test -p trusty-mpm
//! --test inproject_hygiene_test`.

use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_mpm::daemon::managed_routes::inproject_hygiene::run_hygiene_for_base;

/// Run a git command in `dir`, panicking with full output on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?} in {dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {dir:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Create an `origin` repo with one commit on `main`, and a `base` clone of it.
///
/// Returns `(origin_path, base_path)`. The base clone has `origin/HEAD` set up
/// (via `git clone`) so [`get_default_branch`] resolves `"main"`.
fn init_repo_pair(root: &Path) -> (PathBuf, PathBuf) {
    let origin = root.join("origin");
    let base = root.join("base");
    std::fs::create_dir_all(&origin).expect("mkdir origin");

    git(&origin, &["init", "-q", "-b", "main"]);
    git(&origin, &["config", "user.email", "test@example.com"]);
    git(&origin, &["config", "user.name", "Test"]);
    std::fs::write(origin.join("file.txt"), "v1\n").expect("write file");
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-q", "-m", "initial"]);

    // Clone into base — this sets up the `origin` remote and refs/remotes/origin/HEAD.
    let out = Command::new("git")
        .args([
            "clone",
            "-q",
            origin.to_str().expect("utf8 path"),
            base.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("spawn git clone");
    assert!(
        out.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(&base, &["config", "user.email", "test@example.com"]);
    git(&base, &["config", "user.name", "Test"]);

    (origin, base)
}

/// A base clone that is AHEAD of origin (an unpushed local commit) must NOT be
/// reset — the extra commit must survive `run_hygiene_for_base` (#2177).
#[test]
fn hygiene_ahead_branch_is_not_reset() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (_origin, base) = init_repo_pair(tmp.path());

    // Make an unpushed local commit on base's main.
    std::fs::write(base.join("unpushed.txt"), "local work\n").expect("write file");
    git(&base, &["add", "."]);
    git(&base, &["commit", "-q", "-m", "unpushed local work"]);
    let head_before = git(&base, &["rev-parse", "HEAD"]);

    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    let head_after = git(&base, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_before, head_after,
        "an ahead branch must not be reset — HEAD must be unchanged"
    );
    assert!(
        base.join("unpushed.txt").exists(),
        "the unpushed commit's file must survive the hygiene sweep"
    );
}

/// A base clone with a DIRTY working tree (uncommitted changes) must NOT be
/// reset — the uncommitted file must survive `run_hygiene_for_base` (#2177).
#[test]
fn hygiene_dirty_tree_is_not_reset() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (_origin, base) = init_repo_pair(tmp.path());

    // Uncommitted modification — never staged or committed.
    std::fs::write(base.join("file.txt"), "dirty uncommitted change\n").expect("write file");
    let head_before = git(&base, &["rev-parse", "HEAD"]);

    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    let head_after = git(&base, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_before, head_after,
        "HEAD must be unchanged for a dirty tree"
    );

    let contents = std::fs::read_to_string(base.join("file.txt")).expect("read file");
    assert_eq!(
        contents, "dirty uncommitted change\n",
        "uncommitted changes must survive the hygiene sweep"
    );
}

/// A base clone that is clean and NOT ahead of origin must still be reset to
/// origin — the sweep's original fast-forward purpose is preserved (#2177).
#[test]
fn hygiene_clean_branch_is_reset() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (origin, base) = init_repo_pair(tmp.path());

    // Origin moves forward; base has NOT fetched or committed anything new,
    // so base is behind (clean, ahead_count == 0 relative to a post-fetch origin).
    std::fs::write(origin.join("file.txt"), "v2 from origin\n").expect("write file");
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-q", "-m", "origin moved forward"]);
    let origin_head = git(&origin, &["rev-parse", "HEAD"]);

    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    let base_head = git(&base, &["rev-parse", "HEAD"]);
    assert_eq!(
        base_head, origin_head,
        "a clean, non-ahead base clone must be fast-forwarded to origin"
    );
    let contents = std::fs::read_to_string(base.join("file.txt")).expect("read file");
    assert_eq!(
        contents, "v2 from origin\n",
        "the reset must land origin's content"
    );
}

/// When a reset proceeds, a recovery ref pointing at the pre-reset HEAD must be
/// written first, as a defense-in-depth breadcrumb (#2177).
#[test]
fn hygiene_recovery_ref_written_before_reset() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (origin, base) = init_repo_pair(tmp.path());
    let pre_reset_head = git(&base, &["rev-parse", "HEAD"]);

    std::fs::write(origin.join("file.txt"), "v2 from origin\n").expect("write file");
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-q", "-m", "origin moved forward"]);

    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    let recovery_sha = git(&base, &["rev-parse", "refs/trusty-mpm/pre-hygiene/main"]);
    assert_eq!(
        recovery_sha, pre_reset_head,
        "the recovery ref must point at the HEAD sha from before the reset"
    );
}
