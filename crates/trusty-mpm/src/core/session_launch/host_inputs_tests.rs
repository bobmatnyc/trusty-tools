//! A launch redirected to the managed checkout composes from the session's
//! own checkout, never the managed tree.

use std::path::Path;

use super::super::tests::EnvVarGuard;
use super::super::{PrepError, prepare_session_inner};
use super::HostInputs;

/// Copies every regular file under `from` into `to`, skipping `skip`.
fn copy_tree(from: &Path, to: &Path, skip: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read dir").flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry.file_type().expect("file type");
        if src == skip {
            continue;
        } else if kind.is_dir() {
            copy_tree(&src, &dst, skip);
        } else if kind.is_file() {
            std::fs::copy(&src, &dst).expect("copy");
        }
    }
}

/// A trusty-tools checkout at `root` carrying the repository's `content/`,
/// minus `skip` (relative to `content/`); `git_marker` writes `.git`.
fn checkout(root: &Path, skip: &str, git_marker: impl FnOnce(&Path)) {
    let repo = crate::core::content_source::test_support::repo_root();
    let content = repo.join("content");
    copy_tree(&content, &root.join("content"), &content.join(skip));
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    // A real checkout carries its own CLAUDE.md, so prep seeds none.
    std::fs::write(root.join("CLAUDE.md"), "# project\n").expect("CLAUDE.md");
    git_marker(&root.join(".git"));
}

/// A stale main checkout without `owner-plain.md`, and a fresh linked
/// worktree inside it that has the file. A launch whose session directory is
/// the worktree, redirected to run in the main checkout, composes from the
/// worktree; the same launch with no session directory fails on the stale
/// tree and names it.
#[test]
#[serial_test::serial]
fn a_redirected_launch_composes_from_the_session_worktree() {
    let home = tempfile::tempdir().expect("home");
    let _home = EnvVarGuard::set("HOME", home.path());
    let tmp = tempfile::tempdir().expect("tmp");
    let main = tmp.path().join("trusty-tools");
    checkout(&main, "instructions/output-styles/owner-plain.md", |git| {
        std::fs::create_dir_all(git).expect(".git dir");
    });
    let worktree = main.join(".claude/worktrees/fresh");
    checkout(&worktree, "", |git| {
        std::fs::write(git, "gitdir: ../../../.git/worktrees/fresh\n").expect(".git file");
    });
    let fw = crate::core::paths::FrameworkPaths::under(home.path());
    let prep = |content_dir: Option<&Path>| {
        prepare_session_inner(
            &fw,
            &main,
            None,
            true,
            None,
            Some("compose-source"),
            HostInputs {
                content_dir,
                ..HostInputs::with_home(Some(home.path()))
            },
        )
    };

    if let Err(err) = prep(Some(&worktree)) {
        panic!("the launch must compose from the session worktree: {err}");
    }

    let err = prep(None).expect_err("the stale main checkout lacks owner-plain.md");
    assert!(matches!(err, PrepError::Content { .. }), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("owner-plain.md"), "{msg}");
    assert!(msg.contains(&main.display().to_string()), "{msg}");
}
