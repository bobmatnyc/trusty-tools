//! Tests for the per-directory repository resolution (#7057).
//!
//! Why: the bug this module exists to close is invisible in behaviour — a
//! lookup aimed at the wrong repository answers "no pull request" exactly as a
//! correct lookup against a branch with no pull request does. Only the resolved
//! slug distinguishes them, so every test here asserts on the slug.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
fn git_ok(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("fixture: `git {}` could not be run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "fixture: `git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A checkout at `<tmp>/<name>` with one commit and `origin` set to `origin`.
///
/// The remote is never contacted — `git config remote.origin.url` is a local
/// read — so a URL naming a repository that does not exist is fine here and is
/// what keeps these tests hermetic.
fn checkout_with_origin(tmp: &Path, name: &str, origin: &str) -> PathBuf {
    let repo = tmp.join(name);
    std::fs::create_dir_all(&repo).expect("fixture: create repo dir");
    git_ok(&repo, &["init", "--initial-branch=main"]);
    git_ok(&repo, &["config", "user.email", "ci@test.invalid"]);
    git_ok(&repo, &["config", "user.name", "CI"]);
    git_ok(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README.md"), "base\n").expect("fixture: write README");
    git_ok(&repo, &["add", "README.md"]);
    git_ok(&repo, &["commit", "-m", "base"]);
    git_ok(&repo, &["remote", "add", "origin", origin]);
    repo
}

#[test]
fn https_remote_yields_its_owner_and_repo() {
    for url in [
        "https://github.com/1m-consulting/adaptive-crm.git",
        "https://github.com/1m-consulting/adaptive-crm",
        "https://user@github.com/1m-consulting/adaptive-crm.git/",
        "http://github.example.invalid/1m-consulting/adaptive-crm.git",
    ] {
        assert_eq!(
            parse_repo_slug(url).as_deref(),
            Some("1m-consulting/adaptive-crm"),
            "{url}"
        );
    }
}

#[test]
fn ssh_remote_yields_its_owner_and_repo() {
    for url in [
        "ssh://git@github.com/hotstats/hotstats-product-poc.git",
        "ssh://git@github.com:22/hotstats/hotstats-product-poc",
        "git://github.com/hotstats/hotstats-product-poc.git",
    ] {
        assert_eq!(
            parse_repo_slug(url).as_deref(),
            Some("hotstats/hotstats-product-poc"),
            "{url}"
        );
    }
}

#[test]
fn scp_like_remote_yields_its_owner_and_repo() {
    assert_eq!(
        parse_repo_slug("git@github.com:bobmatnyc/trusty-tools.git").as_deref(),
        Some("bobmatnyc/trusty-tools")
    );
}

/// 🔴 The fail-closed case that matters most: a filesystem remote's tail LOOKS
/// like `owner/repo`, and accepting it would aim a live `gh` call at whatever
/// repository happened to share those two names.
#[test]
fn a_local_path_remote_names_no_repository() {
    for url in [
        "/tmp/fixtures/remote.git",
        "../sibling/remote.git",
        "C:\\repos\\remote.git",
        "",
        "github.com",
    ] {
        assert_eq!(parse_repo_slug(url), None, "{url}");
    }
}

#[test]
fn a_file_url_remote_names_no_repository() {
    // `file://` reaches the scp-like branch, whose path is absolute — the guard
    // that keeps a local clone from resolving to a GitHub slug.
    assert_eq!(parse_repo_slug("file:///tmp/fixtures/remote.git"), None);
}

/// 🔴 #7057: the whole point. Two directories on ONE machine, two origins, two
/// answers — never one repository standing in for the other.
#[test]
fn two_worktrees_with_different_origins_resolve_to_different_repos() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = checkout_with_origin(
        tmp.path(),
        "adaptive-crm",
        "https://github.com/1m-consulting/adaptive-crm.git",
    );
    let b = checkout_with_origin(
        tmp.path(),
        "hotstats-product-poc",
        "git@github.com:hotstats/hotstats-product-poc.git",
    );
    assert_eq!(
        repo_slug_for(&a).expect("a resolves"),
        "1m-consulting/adaptive-crm"
    );
    assert_eq!(
        repo_slug_for(&b).expect("b resolves"),
        "hotstats/hotstats-product-poc"
    );
}

/// A linked worktree shares its checkout's config, so the fallback is normally
/// unreachable — this pins that it EXISTS, and that it reaches the owning
/// checkout rather than any other registered project.
#[test]
fn a_directory_with_no_origin_falls_back_to_its_owning_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().into());
    let repo = checkout_with_origin(
        &root,
        "adaptive-crm",
        "https://github.com/1m-consulting/adaptive-crm.git",
    );
    let wt = repo.join(".worktrees").join("fallback-7057");
    git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feat/fallback-7057",
            wt.to_str().expect("utf8 worktree path"),
        ],
    );
    // A worktree-local override with no value is how git spells "this worktree
    // has no origin" without disturbing the shared config.
    git_ok(&wt, &["config", "extensions.worktreeConfig", "true"]);
    git_ok(&wt, &["config", "--worktree", "--unset-all", "remote.origin.url"]);
    assert!(
        origin_url(&wt).is_none(),
        "fixture must leave the worktree without an origin"
    );
    assert_eq!(
        repo_slug_for(&wt).expect("the owning checkout answers"),
        "1m-consulting/adaptive-crm"
    );
}

/// 🔴 Fail-closed: a directory git does not root a repository at refuses, and
/// the refusal names the directory so the operator can see WHERE it looked.
#[test]
fn a_non_repository_directory_resolves_to_no_repository() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let reason = repo_slug_for(tmp.path()).expect_err("a non-repository must refuse");
    assert!(reason.contains("cannot be established"), "{reason}");
    assert!(reason.contains(&tmp.path().display().to_string()), "{reason}");
}

/// A repository whose `origin` is a local clone path refuses, and says what it
/// read — the shape every `GitWorktreeFixture`-backed test now takes.
#[test]
fn an_unparseable_origin_refuses_and_quotes_the_url() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = checkout_with_origin(tmp.path(), "local-remote", "/tmp/fixtures/remote.git");
    let reason = repo_slug_for(&repo).expect_err("a local-path origin must refuse");
    assert!(reason.contains("names no GitHub"), "{reason}");
    assert!(reason.contains("/tmp/fixtures/remote.git"), "{reason}");
}
