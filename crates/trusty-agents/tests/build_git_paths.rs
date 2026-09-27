//! The build script's git-dir resolution for `rerun-if-changed` paths (#8787).
//!
//! Why: `crates/trusty-agents/build.rs` emitted
//! `cargo:rerun-if-changed=.git/HEAD` and `.git/index`, resolved relative to
//! the crate directory — a path that never exists there, since `.git` lives
//! at the repository root. Cargo treats a nonexistent watched path as always
//! stale and reruns the build script (recompiling the whole crate) on every
//! invocation. A build script's own `#[cfg(test)]` module is never compiled
//! by `cargo test`, so this target `include!`s the same source `build.rs`
//! does and tests it directly.
//! What: one test per resolution outcome — a plain (non-worktree) `.git`
//! directory found at an ancestor, a worktree's `gitdir:` pointer file
//! followed to its private per-worktree dir, walking up past the crate's own
//! directory to find `.git`, and the `None` fallback with no `.git` anywhere
//! (the crates.io tarball case).
//! Test: this file.

include!("../build_git_paths.rs");

/// A plain repository's `.git` directory is used directly, even when the
/// search starts several levels below it (mirrors a crate under `crates/`).
#[test]
fn finds_a_plain_git_dir() {
    let repo = tempfile::tempdir().expect("tempdir");
    let git_dir = repo.path().join(".git");
    std::fs::create_dir_all(&git_dir).expect("mkdir .git");
    let crate_dir = repo.path().join("crates").join("some-crate");
    std::fs::create_dir_all(&crate_dir).expect("mkdir crate dir");

    let (head, index) =
        resolve_git_watch_paths(&crate_dir).expect("a .git dir up the tree must resolve");
    assert_eq!(head, git_dir.join("HEAD"));
    assert_eq!(index, git_dir.join("index"));
}

/// A worktree's `.git` FILE is parsed for its `gitdir:` line, and the
/// returned paths point at the worktree's own private git dir — not the
/// pointer file's directory — because that private dir owns the `HEAD` and
/// `index` describing THIS worktree's commit and staged index.
#[test]
fn resolves_a_worktree_gitdir_pointer() {
    let main_repo = tempfile::tempdir().expect("tempdir");
    let worktree_git_dir = main_repo.path().join(".git").join("worktrees").join("wt1");
    std::fs::create_dir_all(&worktree_git_dir).expect("mkdir worktree gitdir");
    std::fs::write(worktree_git_dir.join("HEAD"), "ref: refs/heads/feature\n").expect("write HEAD");
    std::fs::write(worktree_git_dir.join("index"), b"fake-index").expect("write index");

    let worktree_checkout = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        worktree_checkout.path().join(".git"),
        format!("gitdir: {}\n", worktree_git_dir.display()),
    )
    .expect("write .git pointer file");
    let crate_dir = worktree_checkout
        .path()
        .join("crates")
        .join("trusty-agents");
    std::fs::create_dir_all(&crate_dir).expect("mkdir crate dir");

    let (head, index) =
        resolve_git_watch_paths(&crate_dir).expect("a worktree pointer file must resolve");
    let expected_git_dir = worktree_git_dir.canonicalize().expect("canonicalize");
    assert_eq!(head, expected_git_dir.join("HEAD"));
    assert_eq!(index, expected_git_dir.join("index"));
}

/// The walk climbs past the crate's own directory (and any intermediate
/// directories) rather than stopping at the first ancestor.
#[test]
fn walks_up_past_the_crate_dir() {
    let repo = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(repo.path().join(".git")).expect("mkdir .git");
    let deep_dir = repo
        .path()
        .join("crates")
        .join("trusty-agents")
        .join("src")
        .join("nested");
    std::fs::create_dir_all(&deep_dir).expect("mkdir deep dir");

    assert!(resolve_git_watch_paths(&deep_dir).is_some());
}

/// No `.git` anywhere up to the filesystem root (a crates.io source tarball
/// build) resolves to `None` rather than panicking or fabricating a path.
#[test]
fn returns_none_with_no_git_dir_anywhere() {
    let tarball_extract = tempfile::tempdir().expect("tempdir");
    let crate_dir = tarball_extract.path().join("some-crate-1.0.0");
    std::fs::create_dir_all(&crate_dir).expect("mkdir crate dir");

    assert_eq!(resolve_git_watch_paths(&crate_dir), None);
}
