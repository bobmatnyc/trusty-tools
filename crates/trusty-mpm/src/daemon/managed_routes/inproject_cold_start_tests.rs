//! Unit tests for the cold-start entry point (#4990).
//!
//! Why: the two reuse guards are the whole reason this module exists, so both
//! are proven against REAL temp git repositories rather than mocks — the
//! failures they prevent (a checkout on the wrong remote, a dirty tree that
//! cannot be fast-forwarded) live entirely in what git reports about a working
//! directory. The remote canonicalization is pure and gets ordinary table
//! tests.
//! What: `canonical_remote` equivalence and non-equivalence; the
//! remote-mismatch refusal; the dirty-tree refusal; the clean-reuse success;
//! and the truncation of a long dirty-status message.
//! Test: this file IS the test module.

use std::path::Path;
use std::process::Command;

use super::*;

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

/// Create a one-commit `main` repo at `path` and return it.
fn init_origin(path: &Path) -> &Path {
    std::fs::create_dir_all(path).expect("mkdir origin");
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("file.txt"), "v1\n").expect("write file");
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "initial"]);
    path
}

/// Clone `origin` to `dest` so `dest` has a real `origin` remote and `origin/HEAD`.
fn clone_to(origin: &Path, dest: &Path) {
    let out = Command::new("git")
        .args([
            "clone",
            "-q",
            origin.to_str().expect("utf8"),
            dest.to_str().expect("utf8"),
        ])
        .output()
        .expect("spawn git clone");
    assert!(
        out.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(dest, &["config", "user.email", "test@example.com"]);
    git(dest, &["config", "user.name", "Test"]);
}

/// Why: the same repository is spelled three ways in the wild, and a mismatch
/// error for any pair of them would fire on every ordinary GitHub setup.
/// Test: itself.
#[test]
fn equivalent_remote_spellings_match() {
    let want = "github.com/bobmatnyc/trusty-tools";
    for url in [
        "git@github.com:bobmatnyc/trusty-tools.git",
        "git@github.com:bobmatnyc/trusty-tools",
        "https://github.com/bobmatnyc/trusty-tools",
        "https://github.com/bobmatnyc/trusty-tools.git",
        "https://github.com/bobmatnyc/trusty-tools/",
        "ssh://git@github.com/bobmatnyc/trusty-tools.git",
        "HTTPS://GitHub.com/BobMatNYC/Trusty-Tools.git",
    ] {
        assert_eq!(canonical_remote(url), want, "canonicalizing {url}");
    }
}

/// Why: the host is load-bearing here. `parse_github_path` drops it on
/// purpose, so a comparison built on that parser would call two different
/// hosts' `owner/repo` the same repository — exactly the confusion the remote
/// check exists to catch.
/// Test: itself.
#[test]
fn different_hosts_do_not_match() {
    assert_ne!(
        canonical_remote("git@github.com:acme/widget.git"),
        canonical_remote("git@gitlab.com:acme/widget.git")
    );
    assert_ne!(
        canonical_remote("https://github.com/acme/widget"),
        canonical_remote("https://github.com/other/widget")
    );
}

/// DECIDED BEHAVIOR: an existing checkout whose origin names a different
/// repository is an error, not an auto-fix.
///
/// Why: no check exists anywhere today — the standalone driver pulls from
/// whatever `origin` happens to be, so re-pointing an alias at a new URL keeps
/// serving the old remote forever under the new name. Refusing is the only
/// non-destructive answer: re-pointing `origin` under a directory that may hold
/// branches and unpushed commits from the OTHER repository is not a repair.
/// Test: itself.
#[test]
fn existing_checkout_on_a_different_remote_fails_loud() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let origin = init_origin(&tmp.path().join("origin")).to_path_buf();
    let base = tmp.path().join("base");
    clone_to(&origin, &base);

    let err = ensure_managed_checkout_at(&base, "https://github.com/someone/else.git")
        .expect_err("a different remote must be refused");

    assert!(
        matches!(err, ColdStartError::RemoteMismatch { .. }),
        "expected RemoteMismatch, got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains(&base.display().to_string()),
        "names path: {msg}"
    );
    assert!(msg.contains("someone/else"), "names requested repo: {msg}");
    assert!(
        msg.contains("will not re-point"),
        "states the refusal: {msg}"
    );
}

/// FAIL-SAFE: a checkout with no `origin` at all cannot be matched, so it is
/// refused rather than assumed to be the requested repo.
///
/// Test: itself.
#[test]
fn existing_checkout_without_an_origin_fails_loud() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let base = init_origin(&tmp.path().join("base")).to_path_buf();

    let err = ensure_managed_checkout_at(&base, "https://github.com/acme/widget.git")
        .expect_err("a checkout with no origin must be refused");
    assert!(
        matches!(err, ColdStartError::NoOrigin { .. }),
        "expected NoOrigin, got {err:?}"
    );
}

/// DECIDED BEHAVIOR: a dirty existing checkout fails loud.
///
/// Why: this is the `pull_ff_only` hole (`core::standalone::load.rs`) — it
/// warns on a failed refresh and returns `Ok(())`, so the caller cannot tell a
/// refreshed checkout from a stale one. Here the refusal happens BEFORE any
/// write, so the dirty content is never at risk either.
/// Test: itself.
#[test]
fn dirty_existing_checkout_fails_loud() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let origin = init_origin(&tmp.path().join("origin")).to_path_buf();
    let base = tmp.path().join("base");
    clone_to(&origin, &base);

    // The operator's uncommitted work.
    std::fs::write(base.join("file.txt"), "MY UNCOMMITTED EDIT\n").expect("write");

    let url = origin.to_string_lossy().into_owned();
    let err =
        ensure_managed_checkout_at(&base, &url).expect_err("a dirty checkout must be refused");

    assert!(
        matches!(err, ColdStartError::DirtyCheckout { .. }),
        "expected DirtyCheckout, got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains(&base.display().to_string()),
        "names path: {msg}"
    );
    assert!(msg.contains("file.txt"), "names what is dirty: {msg}");

    // And the edit survives — the refusal happens before anything writes.
    assert_eq!(
        std::fs::read_to_string(base.join("file.txt")).expect("read"),
        "MY UNCOMMITTED EDIT\n"
    );
}

/// The success case the two refusals must not swallow: a clean checkout on the
/// requested remote is reused, refreshed, and reported as `reused`.
///
/// Why: a guard that refuses everything is not a guard. This also pins the
/// refresh actually happening — origin moves forward and the reused checkout
/// follows it.
/// Test: itself.
#[test]
fn clean_matching_checkout_is_reused_and_refreshed() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let origin = init_origin(&tmp.path().join("origin")).to_path_buf();
    let base = tmp.path().join("base");
    clone_to(&origin, &base);

    // Origin moves forward after the clone.
    std::fs::write(origin.join("file.txt"), "v2 from origin\n").expect("write");
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-q", "-m", "moved forward"]);

    let url = origin.to_string_lossy().into_owned();
    let checkout = ensure_managed_checkout_at(&base, &url).expect("clean reuse succeeds");

    assert!(checkout.reused, "an existing checkout must report reused");
    assert_eq!(checkout.base_path, base);
    assert_eq!(
        std::fs::read_to_string(base.join("file.txt")).expect("read"),
        "v2 from origin\n",
        "the reused checkout must have been fast-forwarded"
    );
}

/// A fresh clone into an absent path succeeds and reports `reused = false`.
///
/// Why: the cold-start case itself — no `.git`, no cwd, nothing on disk. Uses a
/// local source repo so the test never touches the network.
/// Test: itself.
#[test]
fn absent_path_is_cloned_and_reports_not_reused() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let origin = init_origin(&tmp.path().join("origin")).to_path_buf();
    let base = tmp.path().join("owner").join("repo");

    let url = origin.to_string_lossy().into_owned();
    let checkout = ensure_managed_checkout_at(&base, &url).expect("fresh clone succeeds");

    assert!(!checkout.reused, "a fresh clone must not report reused");
    assert!(base.join(".git").exists(), "clone produced a git checkout");
    assert_eq!(
        std::fs::read_to_string(base.join("file.txt")).expect("read"),
        "v1\n"
    );
}

/// Why: a managed checkout can be thousands of lines dirty, and an
/// untruncated status pushes the remedy off the screen.
/// Test: itself.
#[test]
fn dirty_message_truncates_long_status() {
    let entries: Vec<String> = (0..25).map(|i| format!(" M file{i}.txt")).collect();
    let rendered = summarize_entries(&entries);
    assert!(rendered.contains("file0.txt"));
    assert!(rendered.contains("file9.txt"));
    assert!(!rendered.contains("file10.txt"));
    assert!(rendered.contains("… and 15 more"));

    // Short lists render in full with no summary line.
    let short = summarize_entries(&[" M a.txt".to_string()]);
    assert_eq!(short, " M a.txt");
}
