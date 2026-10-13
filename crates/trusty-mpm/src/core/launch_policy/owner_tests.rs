//! Tests for owner discovery.

use super::*;
use std::process::Command;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
}

fn root() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    (tmp, base)
}

#[test]
fn a_checkout_under_the_repos_root_belongs_to_its_first_segment() {
    let (_tmp, base) = root();
    let repos = base.join("repos");
    let project = repos.join("acme").join("widget");
    init(&project);
    assert_eq!(owner_for(&project, &repos).as_deref(), Some("acme"));
    let sub = project.join("src");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(owner_for(&sub, &repos).as_deref(), Some("acme"));
}

#[test]
fn a_linked_worktree_belongs_to_its_main_checkouts_owner() {
    let (_tmp, base) = root();
    let repos = base.join("repos");
    let project = repos.join("acme").join("widget");
    init(&project);
    git(
        &project,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    let wt = base.join("elsewhere").join("agent-x");
    git(&project, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert_eq!(owner_for(&wt, &repos).as_deref(), Some("acme"));
}

#[test]
fn a_non_git_directory_under_the_tree_uses_its_own_path() {
    let (_tmp, base) = root();
    let repos = base.join("repos");
    let dir = repos.join("acme").join("notes");
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(owner_for(&dir, &repos).as_deref(), Some("acme"));
}

#[test]
fn a_depth_one_directory_has_no_owner() {
    let (_tmp, base) = root();
    let repos = base.join("repos");
    let architect = repos.join("architect");
    init(&architect);
    assert_eq!(owner_for(&architect, &repos), None);
}

#[test]
fn a_checkout_outside_the_tree_belongs_to_its_origin_owner() {
    let (_tmp, base) = root();
    let repos = base.join("repos");
    std::fs::create_dir_all(repos.join("BobMatNyc")).unwrap();
    let project = base.join("outside").join("tool");
    init(&project);
    git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:bobmatnyc/tool.git",
        ],
    );
    let owner = owner_for(&project, &repos).expect("an origin owner");
    assert!(
        owner.eq_ignore_ascii_case("bobmatnyc"),
        "the origin owner, matched to the on-disk spelling: {owner}"
    );
    assert_eq!(owner, "BobMatNyc");
}

#[test]
fn a_directory_outside_the_tree_with_no_origin_has_no_owner() {
    let (_tmp, base) = root();
    let repos = base.join("repos");
    let project = base.join("outside").join("tool");
    init(&project);
    assert_eq!(owner_for(&project, &repos), None);
}
