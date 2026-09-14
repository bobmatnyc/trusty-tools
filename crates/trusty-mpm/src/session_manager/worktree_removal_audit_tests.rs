//! Tests for the pre-deletion removal audit line (#7885).
//!
//! Why: the closure condition is that the line names the resolved path and the
//! session/branch, so the assertions are on the RENDERED line — the thing an
//! operator greps — rather than on the struct's fields.

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// 🔴 #7885: the line names the path, the branch, the owning session and the
/// reason.
///
/// Why: two merged worktrees lost their whole `crates/` subtree on 2026-09-14
/// and no log named either path or either session, so the mechanism could not
/// be identified. Fails on `0f2bd5134`, where no such line exists.
#[test]
fn worktree_7885_an_audit_names_the_path_branch_session_and_reason() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-a9826013b");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    let audit = RemovalAudit::resolve(&wt, "merged-PR reclaim, every gate passed");
    let line = audit.line();
    assert!(
        line.contains(
            &wt.canonicalize()
                .expect("canonicalize")
                .display()
                .to_string()
        ),
        "names the resolved path: {line}"
    );
    assert!(
        line.contains("session/agent-a9826013b"),
        "names the branch: {line}"
    );
    assert!(
        line.contains("merged-PR reclaim"),
        "names the reason: {line}"
    );
    assert!(
        !audit.session.is_empty() && audit.session != "(unowned)",
        "names the sentinel's owner: {:?}",
        audit.session
    );
}

/// Capture everything `body` logs, as rendered lines.
fn captured<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
    use tracing_subscriber::layer::SubscriberExt;

    // #4181/#4931: `tracing` short-circuits every macro on a process-global
    // MAX_LEVEL a thread-local dispatcher does not raise.
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let out = tracing::subscriber::with_default(subscriber, body);
    (out, buffer.tail(64))
}

/// 🔴 #7885: the removal path writes the audit line BEFORE it deletes — proven
/// by a removal git REFUSES, which deletes nothing and must still be logged.
///
/// Why: the two worktrees that lost their `crates/` subtree left no log naming
/// either path or session, and a line on the success arm alone would not have
/// caught them either. Fails on `0f2bd5134`, where no audit line exists on any
/// arm.
#[test]
#[serial_test::serial]
fn worktree_7885_a_removal_emits_one_audit_line_before_deleting() {
    let fx = GitWorktreeFixture::new();

    // 1. A removal that SUCCEEDS.
    let wt = fx.add_worktree("agent-a1afc1489");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    let (outcome, lines) = captured(|| {
        crate::session_manager::decommission::remove_session_worktree(
            &wt,
            "merged-PR reclaim, every gate passed",
        )
    });
    assert!(outcome.removed(), "the fixture worktree must be removable");
    assert!(!wt.exists(), "and actually gone");
    let audit = lines
        .iter()
        .find(|l| l.contains("worktree-removal:"))
        .unwrap_or_else(|| panic!("no audit line for a real removal. Captured: {lines:#?}"));
    assert!(audit.contains("agent-a1afc1489"), "names the path: {audit}");
    assert!(
        audit.contains("session/agent-a1afc1489"),
        "names the branch: {audit}"
    );
    assert!(
        audit.contains("merged-PR reclaim"),
        "names the reason: {audit}"
    );
    assert!(audit.contains("INFO"), "at info level: {audit}");

    // 2. A removal git REFUSES. Nothing is deleted, and the line is still
    //    there — which is what makes it a PRE-deletion record rather than a
    //    success-arm one.
    let locked = fx.add_worktree("agent-locked");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&locked);
    fx.lock_worktree(&locked);
    let (outcome, lines) = captured(|| {
        crate::session_manager::decommission::remove_session_worktree(
            &locked,
            "orphan sweep, no live session claims it",
        )
    });
    assert!(!outcome.removed(), "a git-locked worktree must be refused");
    assert!(locked.exists(), "and left on disk");
    assert!(
        lines.iter().any(|l| l.contains("worktree-removal:")
            && l.contains("agent-locked")
            && l.contains("orphan sweep")),
        "a refused removal must still be audited. Captured: {lines:#?}"
    );
}

/// A path git cannot be asked about still produces a line — an audit that
/// refuses is a new failure mode on a path whose job is to finish.
#[test]
fn an_audit_for_an_unreadable_path_still_names_the_path_and_reason() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("gone");

    let audit = RemovalAudit::resolve(&missing, "leftover directory, no repository claims it");
    assert_eq!(audit.branch, "(none)", "no branch is knowable here");
    assert_eq!(audit.session, "(unowned)", "no sentinel is knowable here");
    let line = audit.line();
    assert!(line.contains("gone"), "still names the path: {line}");
    assert!(
        line.contains("leftover directory"),
        "still names the reason: {line}"
    );
}
