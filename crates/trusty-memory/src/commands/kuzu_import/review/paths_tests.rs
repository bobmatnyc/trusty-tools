//! Tests for the review export's destination guard (#8624 rule 4).

use std::path::{Path, PathBuf};

use super::ReviewError;
use super::paths::*;

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).expect("canonicalize")
}

fn is_guard(r: Result<PathBuf, ReviewError>) -> bool {
    matches!(r, Err(ReviewError::Guard { .. }))
}

/// Why: a destination that does not exist yet, under a git work tree, is
/// refused by its nearest existing ancestor; a linked worktree's `.git` file
/// counts too.
/// Test: itself.
#[test]
fn guard_refuses_a_destination_inside_a_git_work_tree_before_it_exists() {
    let tmp = tempfile::tempdir().expect("tmp");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir");
    assert!(is_guard(guard_destination(&repo.join("a/b/run"), &[])));
    let linked = tmp.path().join("linked");
    std::fs::create_dir(&linked).expect("mkdir");
    std::fs::write(linked.join(".git"), "gitdir: /elsewhere").expect("write");
    assert!(is_guard(guard_destination(&linked.join("run"), &[])));
}

/// Why: a symlink is followed before the comparison, so a link pointing into
/// the palace data root or a repo is refused.
/// Test: itself.
#[test]
fn guard_refuses_a_symlink_into_a_protected_root() {
    let tmp = tempfile::tempdir().expect("tmp");
    let palace = tmp.path().join("data");
    std::fs::create_dir(&palace).expect("mkdir");
    let link = tmp.path().join("innocent");
    std::os::unix::fs::symlink(&palace, &link).expect("symlink");
    let protected = [palace.clone()];
    assert!(is_guard(guard_destination(
        &link.join("kuzu-review/run"),
        &protected
    )));
    // The same through a protected root named by a symlink.
    let named = [link.clone()];
    assert!(is_guard(guard_destination(&palace.join("run"), &named)));
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir");
    let to_repo = tmp.path().join("to-repo");
    std::os::unix::fs::symlink(&repo, &to_repo).expect("symlink");
    assert!(is_guard(guard_destination(&to_repo.join("run"), &[])));
}

/// Why: `..` through an existing directory is resolved, so it cannot climb
/// into a protected root unseen.
/// Test: itself.
#[test]
fn guard_refuses_dotdot_into_a_protected_root() {
    let tmp = tempfile::tempdir().expect("tmp");
    let palace = tmp.path().join("data");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&palace).expect("mkdir");
    std::fs::create_dir(&outside).expect("mkdir");
    let dest = outside.join("../data/run");
    assert!(is_guard(guard_destination(&dest, &[palace])));
}

/// Why: a `..` inside the part that does not exist cannot be resolved
/// against the disk, so it is refused rather than guessed.
/// Test: itself.
#[test]
fn guard_refuses_dotdot_in_the_missing_tail() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("missing/../run");
    assert!(is_guard(guard_destination(&dest, &[])));
    assert!(is_guard(guard_destination(Path::new("relative/run"), &[])));
}

/// Why: a dangling symlink exists but resolves nowhere; it is refused.
/// Test: itself.
#[test]
fn guard_refuses_a_dangling_symlink() {
    let tmp = tempfile::tempdir().expect("tmp");
    let link = tmp.path().join("dangling");
    std::os::unix::fs::symlink(tmp.path().join("nowhere"), &link).expect("symlink");
    assert!(is_guard(guard_destination(&link.join("run"), &[])));
}

/// Why: an ordinary destination passes and comes back fully resolved, with
/// its missing tail appended.
/// Test: itself.
#[test]
fn guard_admits_a_plain_destination_and_resolves_it() {
    let tmp = tempfile::tempdir().expect("tmp");
    let palace = tmp.path().join("data");
    let dest = tmp.path().join("home/.trusty-tools/kuzu-review/run");
    let got = guard_destination(&dest, &[palace]).expect("admitted");
    assert_eq!(
        got,
        canon(tmp.path()).join("home/.trusty-tools/kuzu-review/run")
    );
}

/// Why (ruling Q4): the root is fixed under the home directory.
/// Test: itself.
#[test]
fn default_root_is_under_home() {
    let home = dirs::home_dir().expect("home");
    assert_eq!(
        default_root().expect("root"),
        home.join(".trusty-tools").join("kuzu-review")
    );
}

/// Why: two runs never share a directory.
/// Test: itself.
#[test]
fn run_names_are_distinct() {
    let (a, b) = (run_name(), run_name());
    assert_ne!(a, b);
    assert_eq!(a.len(), "20261012T000000Z-".len() + 8);
}
