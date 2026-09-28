//! A failed `git worktree remove` that deleted content is never reported as
//! kept (#8782).
//!
//! Why: `remove_registered_worktree` read every non-zero git exit through the
//! #4732 protection classifier, which answers "declined, must be preserved"
//! whenever git still lists the tree — including after git already deleted
//! part of it.
//! What: a stub stands in for git: it deletes some of a real registered
//! worktree's files and exits 128. No real git failure is needed.
//! Test: this file is the test module.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;

use super::{WorktreeRemoval, remove_registered_worktree};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_removal_integrity::content_count;
use crate::session_manager::worktree_safety::DirtyWorktreePolicy;

/// Fails on `e3503272d`'s rule, where the same run returns
/// `Kept("… must be preserved …")`.
#[test]
fn a_git_failure_after_a_partial_delete_is_reported_as_partially_removed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("partial-8782");
    for name in ["a.rs", "b.rs", "c.rs"] {
        std::fs::write(wt.join(name), "// work\n").expect("write");
    }
    let before = content_count(&wt);
    let stub = |_root: &Path| {
        // What git does before it reports some failures: delete, then stop.
        std::fs::remove_file(wt.join("a.rs")).expect("stub delete a.rs");
        std::fs::remove_file(wt.join("b.rs")).expect("stub delete b.rs");
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(128 << 8),
            stdout: Vec::new(),
            stderr: b"error: failed to delete 'c.rs': Permission denied".to_vec(),
        })
    };

    let outcome = remove_registered_worktree(&wt, DirtyWorktreePolicy::Skip, before, &stub);

    let WorktreeRemoval::PartiallyRemoved(report) = &outcome else {
        panic!("a partial delete was reported as {outcome:?}");
    };
    assert!(report.contains("partially removed"), "{report}");
    assert!(
        report.contains("failed to delete 'c.rs'"),
        "git's error: {report}"
    );
    for word in ["kept", "preserved", "declined"] {
        assert!(!report.contains(word), "reads as kept ({word}): {report}");
    }
    assert!(
        wt.join("c.rs").exists(),
        "nothing more is deleted after git fails"
    );
}
