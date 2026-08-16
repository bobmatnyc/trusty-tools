//! Reap-gate tests for [`super`] (#4311).
//!
//! Why: this module deletes directories, so the arms that REFUSE are the ones
//! that earn coverage. Every one runs against a throwaway git repository built
//! by [`GitWorktreeFixture`] — never against a real worktree on the machine.
//! Test: this file.

use std::path::PathBuf;

use super::{ReapOutcome, is_harness_agent_worktree, reap_worktree};
use crate::core::hook::HookEvent;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::write_agent_sentinel;

/// Build a worktree at the harness shape: `<repo>/.claude/worktrees/<name>`.
fn harness_worktree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let base = fx.repo.join(".claude").join("worktrees");
    fx.add_worktree_at(&base, name)
}

/// #4311 REGRESSION: a finished agent's clean, fully-pushed worktree is removed.
///
/// Why: this is the population the reap exists for. The harness reclaims a
/// granted worktree only while it is UNCHANGED, so everything that survives it
/// has work in it — and the one class this module may safely remove is work
/// that is committed and pushed. That is the merged-PR shape the July 2026
/// sweep reclaimed 1.1 TiB of by hand.
#[test]
fn reap_removes_a_clean_pushed_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree(&fx, "agent-alpha");
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    let outcome = reap_worktree(&wt, &[]);

    assert_eq!(outcome, ReapOutcome::Removed, "clean+pushed must be reaped");
    assert!(!wt.exists(), "the directory must be gone: {}", wt.display());
}

/// An uncommitted edit is never destroyed by a reap.
#[test]
fn reap_refuses_a_dirty_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree(&fx, "agent-dirty");
    std::fs::write(wt.join("README.md"), "edited, never committed\n").expect("write edit");

    let outcome = reap_worktree(&wt, &[]);

    let reason = outcome.refusal().expect("a dirty worktree must be refused");
    assert!(reason.contains("unsaved work"), "{reason}");
    assert!(wt.exists(), "the directory must survive");
}

/// A commit that reached no remote is unsaved work, even though it is committed.
#[test]
fn reap_refuses_an_unpushed_commit() {
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree(&fx, "agent-unpushed");
    GitWorktreeFixture::commit_unpushed(&wt);

    let outcome = reap_worktree(&wt, &[]);

    let reason = outcome
        .refusal()
        .expect("an unpushed commit must be refused");
    assert!(reason.contains("unsaved work"), "{reason}");
    assert!(wt.exists(), "the directory must survive");
}

/// A path something live still holds survives, however clean it reads.
///
/// Why: the caller passes every managed session's `workspace_path` and every
/// non-terminal sibling delegation's registered tree. A worktree whose owning
/// session is still running is the case that must never be reaped by an agent's
/// exit — the agent ended, the session did not.
#[test]
fn reap_refuses_a_path_a_live_session_holds() {
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree(&fx, "agent-shared");
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    let outcome = reap_worktree(&wt, std::slice::from_ref(&wt));

    let reason = outcome.refusal().expect("an in-use path must be refused");
    assert!(reason.contains("still in use"), "{reason}");
    assert!(wt.exists(), "the directory must survive");
}

/// A tree that escaped `.claude/worktrees/` is recorded but never removed.
///
/// Why: the 2026-07-29 count found worktrees under `/private/tmp`,
/// `/private/var/folders`, and two outside the project directory entirely.
/// Recording those is what makes them visible; deleting a directory whose
/// provenance this module cannot establish is not on the table.
#[test]
fn reap_refuses_a_worktree_outside_the_harness_base() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree_at(&fx.repo.join("scratch"), "agent-escaped");
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    let outcome = reap_worktree(&wt, &[]);

    let reason = outcome
        .refusal()
        .expect("a non-harness path must be refused");
    assert!(reason.contains(".claude/worktrees"), "{reason}");
    assert!(wt.exists(), "the directory must survive");
}

/// The harness reclaimed it first — the ordinary outcome for an unchanged tree.
#[test]
fn reap_reports_already_gone_when_the_harness_reclaimed_it() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.repo.join(".claude").join("worktrees").join("never-made");

    assert_eq!(reap_worktree(&wt, &[]), ReapOutcome::AlreadyGone);
}

/// A directory git does not claim is never removed, and there is no
/// `remove_dir_all` fallback that would remove it anyway.
#[test]
fn reap_refuses_a_directory_no_git_registry_claims() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wt = tmp.path().join(".claude").join("worktrees").join("orphan");
    std::fs::create_dir_all(&wt).expect("create dirs");

    let outcome = reap_worktree(&wt, &[]);

    assert!(
        outcome.refusal().is_some(),
        "a non-worktree directory must be refused, got {outcome:?}"
    );
    assert!(wt.exists(), "the directory must survive");
}

#[test]
fn harness_shape_is_the_strict_leaf_form() {
    assert!(is_harness_agent_worktree(std::path::Path::new(
        "/r/.claude/worktrees/agent-x"
    )));
    // One level deeper is not a leaf of the store, and neither is the store
    // itself — the looser "somewhere under .claude/worktrees" test that
    // `worktree_reconcile::categorize` uses is report text, not a delete gate.
    assert!(!is_harness_agent_worktree(std::path::Path::new(
        "/r/.claude/worktrees/agent-x/nested"
    )));
    assert!(!is_harness_agent_worktree(std::path::Path::new(
        "/r/.claude/worktrees"
    )));
    assert!(!is_harness_agent_worktree(std::path::Path::new(
        "/r/.worktrees/session-x"
    )));
}

/// A hermetic daemon state plus one session id.
fn hermetic() -> (
    std::sync::Arc<crate::daemon::state::DaemonState>,
    tempfile::TempDir,
    crate::core::session::SessionId,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = crate::core::paths::FrameworkPaths::under(dir.path());
    let state = std::sync::Arc::new(crate::daemon::state::DaemonState::with_paths(&paths));
    (
        state,
        dir,
        crate::core::session::SessionId(uuid::Uuid::new_v4()),
    )
}

/// `spawn_on_stop` is a no-op for anything that is not an agent exit.
///
/// Why: it runs on the hook pipeline's hot path, in front of every event in
/// every managed session, so an event that is not a stop must return before it
/// reaches the delegation lookup at all.
#[tokio::test]
async fn spawn_on_stop_ignores_a_non_stop_event() {
    let (state, _dir, session) = hermetic();
    super::spawn_on_stop(
        &state,
        session,
        HookEvent::PreToolUse,
        &serde_json::json!({ "agent_id": "a1", "cwd": "/nowhere" }),
    );
    assert!(state.delegations_for(session).is_empty());
}

/// A stop with no `agent_id` reaps nothing.
///
/// Why: `agent_id` is the only exact correlation key a stop carries, and
/// `delegation_tracker`'s own note forbids a "most recent" guess — under
/// concurrency it closes the wrong agent. Reaping on such a guess would delete
/// the wrong directory, so the absence of the key must end the path.
#[tokio::test]
async fn spawn_on_stop_ignores_a_payload_without_an_agent_id() {
    let (state, _dir, session) = hermetic();
    super::spawn_on_stop(
        &state,
        session,
        HookEvent::SubagentStop,
        &serde_json::json!({ "cwd": "/nowhere" }),
    );
    // An empty-string id is equally indeterminate, not a match.
    super::spawn_on_stop(
        &state,
        session,
        HookEvent::SubagentStop,
        &serde_json::json!({ "agent_id": "", "cwd": "/nowhere" }),
    );
    assert!(state.delegations_for(session).is_empty());
}

// ── the liveness gate (#4311) ───────────────────────────────────────────────

/// #4311 REGRESSION: a process standing in the tree stops the removal, even
/// though every registry says the tree is free.
///
/// Why: gates 3 and 5 read registries — what trusty-mpm recorded, and what git
/// recorded. On 2026-08-15 a `trusty-memory serve --foreground` started by hand
/// inside an agent worktree ran for a day in neither. This test passes `&[]` for
/// `in_use` and builds a clean, fully-pushed tree, so every earlier gate
/// approves; only an OS-level check can refuse. It fails against the pre-#4311
/// chain, which removed the directory out from under the process.
#[test]
fn reap_refuses_a_worktree_a_live_process_is_standing_in() {
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree(&fx, "agent-occupied");
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    // A real child process whose cwd is the worktree, held open for the probe.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .current_dir(&wt)
        .spawn()
        .expect("spawn a process inside the worktree");
    let outcome = reap_worktree(&wt, &[]);
    let _ = child.kill();
    let _ = child.wait();

    let reason = outcome
        .refusal()
        .expect("a worktree a live process stands in must be refused");
    assert!(reason.contains("still occupied"), "{reason}");
    assert!(wt.exists(), "the directory must survive");
}

// ── ownership rebuilt from disk (#4311, ADR-0023 point 4) ───────────────────

/// #4311 REGRESSION: a stop reaps a tree the in-memory registry never knew.
///
/// Why: `DaemonState::delegations` is a `DashMap` built empty at every boot
/// with no load path, so a daemon restart drops every registration — and a
/// `SubagentStop` arrives once, so the reap was lost permanently for any agent
/// alive across the restart. This test registers NOTHING in the daemon state
/// (which is exactly what a post-restart daemon holds) and asserts the reap
/// still fires from the on-disk sentinel alone. It fails against the pre-#4311
/// body, whose only lookup was the DashMap.
#[tokio::test]
async fn stop_reaps_a_worktree_the_registry_lost_to_a_restart() {
    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree(&fx, "agent-restarted");
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");
    write_agent_sentinel(
        &wt,
        crate::session_manager::worktree_ownership::AgentWorktreeOwner {
            agent_id: "a601".to_string(),
            delegation_id: crate::core::agent::DelegationId(uuid::Uuid::new_v4()),
            parent_session_id: session,
        },
    )
    .expect("write agent sentinel");
    assert!(
        state.delegations_for(session).is_empty(),
        "this fixture must hold no registration — that is what a restarted daemon looks like"
    );

    super::spawn_on_stop(
        &state,
        session,
        HookEvent::SubagentStop,
        &serde_json::json!({ "agent_id": "a601", "cwd": fx.repo.to_string_lossy() }),
    );
    await_gone(&wt).await;

    assert!(
        !wt.exists(),
        "the sentinel is the whole ownership record here; the reap must act on it"
    );
}

/// A sentinel naming a DIFFERENT agent is not this agent's tree.
///
/// Why: the disk lookup is the one place a "close enough" match would delete
/// a directory belonging to an agent that is still running. It must be as
/// exact as `on_subagent_stop`'s own `agent_id` resolution.
#[tokio::test]
async fn stop_ignores_a_sentinel_naming_a_different_agent() {
    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree(&fx, "agent-other");
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");
    write_agent_sentinel(
        &wt,
        crate::session_manager::worktree_ownership::AgentWorktreeOwner {
            agent_id: "a701".to_string(),
            delegation_id: crate::core::agent::DelegationId(uuid::Uuid::new_v4()),
            parent_session_id: session,
        },
    )
    .expect("write agent sentinel");

    super::spawn_on_stop(
        &state,
        session,
        HookEvent::SubagentStop,
        &serde_json::json!({ "agent_id": "a702", "cwd": fx.repo.to_string_lossy() }),
    );
    // Give a wrong implementation the same window the correct one gets.
    tokio::time::sleep(std::time::Duration::from_millis(750)).await;

    assert!(
        wt.exists(),
        "another agent's worktree must never be reaped by this stop"
    );
}

/// Poll until `path` is gone, or fail with what is still there.
///
/// Why: `spawn_on_stop` detaches the removal onto a task, so the assertion has
/// to wait on the condition rather than on a fixed sleep.
async fn await_gone(path: &std::path::Path) {
    for _ in 0..100 {
        if !path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{} was never removed", path.display());
}
