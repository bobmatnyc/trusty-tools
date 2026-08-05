//! Integration tests for the startup-hygiene data-loss fixes (#2177, #4961).
//!
//! Why: `run_hygiene_for_base` runs on every daemon start against the managed
//! checkout a user may have open in an editor, so every claim it makes about
//! not losing work has to be proven against real git, not a mock. #2177 proved
//! the committed-but-unpushed and dirty-tree cases. #4961 adds the case those
//! missed: `git status --porcelain` does not report gitignored paths, so
//! gitignored content reads as clean and BOTH `git reset --hard` and `git merge
//! --ff-only` silently overwrite it when the target commit tracks the same
//! path. Only a real git fixture can demonstrate that, because the bug lives
//! entirely in what git chooses to report and overwrite.
//! What: a small git-repo-pair harness (`origin` + `base` clone, both real
//! on-disk git repos under a `tempfile::TempDir`) plus one test per safety
//! scenario, plus the preserved fast-forward case so the sweep's actual
//! purpose is not silently killed.
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
/// `gitignore` is written into the initial commit when non-empty, which is how
/// the #4961 fixture makes a path invisible to `git status --porcelain`.
/// Returns `(origin_path, base_path)`. The base clone has `origin/HEAD` set up
/// (via `git clone`) so `get_default_branch` resolves `"main"`.
fn init_repo_pair_with_ignore(root: &Path, gitignore: &str) -> (PathBuf, PathBuf) {
    let origin = root.join("origin");
    let base = root.join("base");
    std::fs::create_dir_all(&origin).expect("mkdir origin");

    git(&origin, &["init", "-q", "-b", "main"]);
    git(&origin, &["config", "user.email", "test@example.com"]);
    git(&origin, &["config", "user.name", "Test"]);
    std::fs::write(origin.join("file.txt"), "v1\n").expect("write file");
    if !gitignore.is_empty() {
        std::fs::write(origin.join(".gitignore"), gitignore).expect("write gitignore");
    }
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

/// The common no-gitignore fixture.
fn init_repo_pair(root: &Path) -> (PathBuf, PathBuf) {
    init_repo_pair_with_ignore(root, "")
}

/// Advance origin's `main` by one commit that rewrites `file.txt`.
fn advance_origin(origin: &Path) -> String {
    std::fs::write(origin.join("file.txt"), "v2 from origin\n").expect("write file");
    git(origin, &["add", "."]);
    git(origin, &["commit", "-q", "-m", "origin moved forward"]);
    git(origin, &["rev-parse", "HEAD"])
}

/// THE #4961 REGRESSION TEST: gitignored working-tree content must survive.
///
/// The exact reproduced scenario. `notes.md` is gitignored, so it holds real
/// user content while `git status --porcelain` reports the tree clean and the
/// branch is not ahead — every pre-#4961 gate passes. Origin then starts
/// tracking that same path. `git reset --hard origin/main` (pre-fix) and `git
/// merge --ff-only origin/main` (the naive fix) BOTH overwrite the file
/// silently. The update must refuse instead, and the content must survive.
#[test]
fn hygiene_gitignored_file_is_not_clobbered() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (origin, base) = init_repo_pair_with_ignore(tmp.path(), "notes.md\n");

    // The user's real, never-committed content, in a gitignored path.
    std::fs::write(base.join("notes.md"), "MY PRECIOUS NOTES\n").expect("write notes");

    // The premise of the bug: this content is invisible to the dirty check.
    let porcelain = git(&base, &["status", "--porcelain"]);
    assert!(
        porcelain.is_empty(),
        "premise: gitignored content must read as clean, got: {porcelain:?}"
    );

    // Origin starts tracking the same path, with different content.
    std::fs::write(origin.join("notes.md"), "origin version\n").expect("write notes");
    git(&origin, &["add", "-f", "notes.md"]);
    git(
        &origin,
        &["commit", "-q", "-m", "origin now tracks notes.md"],
    );

    let head_before = git(&base, &["rev-parse", "HEAD"]);
    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    let contents = std::fs::read_to_string(base.join("notes.md")).expect("read notes");
    assert_eq!(
        contents, "MY PRECIOUS NOTES\n",
        "gitignored user content must survive the hygiene sweep"
    );
    assert_eq!(
        head_before,
        git(&base, &["rev-parse", "HEAD"]),
        "the update must be skipped, not forced through"
    );
}

/// A base clone that is AHEAD of origin (an unpushed local commit) must NOT be
/// updated — the extra commit must survive `run_hygiene_for_base` (#2177).
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
        "an ahead branch must not be updated — HEAD must be unchanged"
    );
    assert!(
        base.join("unpushed.txt").exists(),
        "the unpushed commit's file must survive the hygiene sweep"
    );
}

/// A base clone with a DIRTY working tree (uncommitted changes) must NOT be
/// updated — the uncommitted file must survive `run_hygiene_for_base` (#2177).
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

/// A base clone that is clean and NOT ahead of origin must still advance to
/// origin — the sweep's original purpose is preserved (#2177, #4961).
///
/// This is the test that would catch a "fix" that made hygiene a permanent
/// no-op, which is what adding `--ignored` to the dirty check would do.
#[test]
fn hygiene_clean_branch_is_fast_forwarded() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (origin, base) = init_repo_pair(tmp.path());

    // Origin moves forward; base has NOT fetched or committed anything new,
    // so base is behind (clean, ahead_count == 0 relative to a post-fetch origin).
    let origin_head = advance_origin(&origin);

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
        "the fast-forward must land origin's content"
    );
}

/// A base clone whose gitignored content does NOT collide with the incoming
/// commit must still fast-forward (#4961).
///
/// The distinction that makes the collision check a real fix rather than a
/// feature-kill: a built checkout always has a gitignored `target/`, and that
/// must not block hygiene forever.
#[test]
fn hygiene_non_colliding_ignored_content_still_fast_forwards() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (origin, base) = init_repo_pair_with_ignore(tmp.path(), "target/\n");

    // The analogue of a built checkout: gitignored, present, never tracked.
    std::fs::create_dir(base.join("target")).expect("mkdir target");
    std::fs::write(base.join("target/artifact.bin"), "build output\n").expect("write artifact");

    let origin_head = advance_origin(&origin);

    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    assert_eq!(
        git(&base, &["rev-parse", "HEAD"]),
        origin_head,
        "non-colliding gitignored content must not block the fast-forward"
    );
    assert!(
        base.join("target/artifact.bin").exists(),
        "and it must still be there afterwards"
    );
}

/// Off the default branch, the update must refuse rather than move the local
/// branch ref to `origin/<default>` (#4961, second finding).
///
/// The ahead-count is measured as `origin/feature..feature`, so it can read
/// zero while the operation targets `origin/main` — a different ref entirely.
#[test]
fn hygiene_non_default_branch_is_not_updated() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (origin, base) = init_repo_pair(tmp.path());

    // A real `feature` branch on origin, so `origin/feature` exists and the
    // ahead-count resolves to Some(0) — the exact pre-fix passing condition.
    git(&origin, &["branch", "feature"]);
    git(&base, &["fetch", "-q", "origin"]);
    git(
        &base,
        &["checkout", "-q", "-b", "feature", "origin/feature"],
    );
    let feature_head = git(&base, &["rev-parse", "HEAD"]);

    let origin_main_head = advance_origin(&origin);
    assert_ne!(feature_head, origin_main_head, "fixture sanity");

    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    assert_eq!(
        git(&base, &["rev-parse", "HEAD"]),
        feature_head,
        "a non-default branch must never be silently moved to origin/<default>"
    );
}

/// The per-repo opt-out marker skips the sweep for a single checkout (#4961).
#[test]
fn hygiene_opt_out_marker_skips_update() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (origin, base) = init_repo_pair(tmp.path());

    // Exclude the marker locally so it does not itself dirty the tree — the
    // skip must be attributable to the marker, not to a dirty working tree.
    std::fs::write(base.join(".git/info/exclude"), ".trusty-mpm-no-hygiene\n")
        .expect("write exclude");
    std::fs::write(base.join(".trusty-mpm-no-hygiene"), "").expect("write marker");
    assert!(
        git(&base, &["status", "--porcelain"]).is_empty(),
        "fixture sanity: the marker must not make the tree dirty"
    );

    let head_before = git(&base, &["rev-parse", "HEAD"]);
    let origin_head = advance_origin(&origin);
    assert_ne!(head_before, origin_head, "fixture sanity");

    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    assert_eq!(
        git(&base, &["rev-parse", "HEAD"]),
        head_before,
        "an opted-out checkout must be left entirely alone"
    );
}

/// When an update proceeds, a recovery ref pointing at the pre-update HEAD must
/// be written first, as a defense-in-depth breadcrumb (#2177).
#[test]
fn hygiene_recovery_ref_written_before_update() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let (origin, base) = init_repo_pair(tmp.path());
    let pre_update_head = git(&base, &["rev-parse", "HEAD"]);

    advance_origin(&origin);

    let result = run_hygiene_for_base(&base);
    assert!(
        result.is_ok(),
        "run_hygiene_for_base should not error: {result:?}"
    );

    let recovery_sha = git(&base, &["rev-parse", "refs/trusty-mpm/pre-hygiene/main"]);
    assert_eq!(
        recovery_sha, pre_update_head,
        "the recovery ref must point at the HEAD sha from before the update"
    );
}
