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

/// The agent every fixture worktree below is attributed to.
const AGENT: &str = "a-fixture";

/// Build a worktree at the harness shape, attributed to [`AGENT`].
///
/// Why the sentinel is part of the fixture: since #4311's review,
/// [`reap_worktree`] re-reads the target's sentinel and refuses a path whose
/// ownership it cannot confirm on disk. A fixture without one exercises that
/// refusal instead of the gate the test names.
fn harness_worktree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    harness_worktree_for(fx, name, AGENT)
}

/// [`harness_worktree`], attributed to `agent_id`.
///
/// Why the agent is a parameter rather than a later `attribute` call: a test
/// that commits (`commit_all_and_push`) commits the sentinel with everything
/// else, so re-stamping it afterwards leaves a MODIFIED TRACKED file and the
/// dirt gate refuses — the test would then exercise gate 5 instead of the gate
/// it names. Attribute once, before any commit.
fn harness_worktree_for(fx: &GitWorktreeFixture, name: &str, agent_id: &str) -> PathBuf {
    let base = fx.repo.join(".claude").join("worktrees");
    let wt = fx.add_worktree_at(&base, name);
    attribute(&wt, agent_id);
    wt
}

/// Stamp `path`'s ownership sentinel as belonging to `agent_id`.
fn attribute(path: &std::path::Path, agent_id: &str) {
    attribute_to(path, agent_id, crate::core::session::SessionId::new());
}

/// [`attribute`], naming `parent` as the dispatching session.
///
/// Why a separate helper: the `SessionEnd` sweep resolves candidates by
/// `parent_session_id`, so a fixture stamped with a random one is invisible to
/// it and would exercise the "not my agent" arm instead of the gate under test.
fn attribute_to(path: &std::path::Path, agent_id: &str, parent: crate::core::session::SessionId) {
    write_agent_sentinel(
        path,
        crate::session_manager::worktree_ownership::AgentWorktreeOwner {
            agent_id: agent_id.to_string(),
            delegation_id: crate::core::agent::DelegationId(uuid::Uuid::new_v4()),
            parent_session_id: parent,
        },
    )
    .expect("write agent sentinel");
}

/// [`harness_worktree_for`], attributed to `agent_id` under parent `parent`.
fn harness_worktree_under(
    fx: &GitWorktreeFixture,
    name: &str,
    agent_id: &str,
    parent: crate::core::session::SessionId,
) -> PathBuf {
    let base = fx.repo.join(".claude").join("worktrees");
    let wt = fx.add_worktree_at(&base, name);
    attribute_to(&wt, agent_id, parent);
    wt
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

    let outcome = reap_worktree(&wt, AGENT, &[]);

    assert_eq!(outcome, ReapOutcome::Removed, "clean+pushed must be reaped");
    assert!(!wt.exists(), "the directory must be gone: {}", wt.display());
}

/// An uncommitted edit is never destroyed by a reap.
#[test]
fn reap_refuses_a_dirty_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree(&fx, "agent-dirty");
    std::fs::write(wt.join("README.md"), "edited, never committed\n").expect("write edit");

    let outcome = reap_worktree(&wt, AGENT, &[]);

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

    let outcome = reap_worktree(&wt, AGENT, &[]);

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

    let outcome = reap_worktree(&wt, AGENT, std::slice::from_ref(&wt));

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

    let outcome = reap_worktree(&wt, AGENT, &[]);

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

    assert_eq!(reap_worktree(&wt, AGENT, &[]), ReapOutcome::AlreadyGone);
}

/// A directory git does not claim is never removed, and there is no
/// `remove_dir_all` fallback that would remove it anyway.
#[test]
fn reap_refuses_a_directory_no_git_registry_claims() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wt = tmp.path().join(".claude").join("worktrees").join("orphan");
    std::fs::create_dir_all(&wt).expect("create dirs");
    // Attributed, so the refusal under test is git's and not the sentinel gate's.
    attribute(&wt, AGENT);

    let outcome = reap_worktree(&wt, AGENT, &[]);

    let reason = outcome
        .refusal()
        .expect("a non-worktree directory must be refused");
    assert!(reason.contains("no git repository claims"), "{reason}");
    assert!(wt.exists(), "the directory must survive");
}

/// #4311 REGRESSION: a path whose sentinel names ANOTHER agent is never removed.
///
/// Why: `reap_worktree`'s target arrives either from `d.worktree_path` — a
/// registry value the write site could have been tricked into pointing
/// anywhere, since the agent chooses the `cwd` it reports — or from
/// `rebuild_from_disk`. Neither is a claim made BY the directory. Re-reading
/// the sentinel at the target is the one check that cannot have been redirected
/// on the way here, and it holds even if every write-site gate is bypassed.
#[test]
fn reap_refuses_a_path_whose_sentinel_names_another_agent() {
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree_for(&fx, "agent-someone-else", "a-the-real-owner");
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    let outcome = reap_worktree(&wt, "a-the-impostor", &[]);

    let reason = outcome
        .refusal()
        .expect("a sentinel naming another agent must be refused");
    assert!(reason.contains("a-the-impostor"), "{reason}");
    assert!(wt.exists(), "another agent's worktree must survive");
}

/// #4311 REGRESSION: an unattributed path is never removed, however clean.
///
/// Why: this is the shape a corrupted or redirected registry value produces —
/// a directory the agent merely visited, carrying no ownership claim at all.
/// Pre-review it was removed on the registry's word alone.
#[test]
fn reap_refuses_a_path_with_no_sentinel() {
    let fx = GitWorktreeFixture::new();
    let base = fx.repo.join(".claude").join("worktrees");
    let wt = fx.add_worktree_at(&base, "agent-unattributed");
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    let outcome = reap_worktree(&wt, AGENT, &[]);

    let reason = outcome
        .refusal()
        .expect("an unattributed path must be refused");
    assert!(reason.contains("Unknown"), "{reason}");
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

/// The three answers the merged-PR reclaim gate keys on (#5661).
///
/// Why: `Ended` is the only one that permits a deletion, so the boundary
/// between it and the two refusing answers is the whole safety property. In
/// particular an empty registry must read `Unknown` and not `Ended` — that is
/// the post-restart shape, and reading it as `Ended` is what would let the
/// sweep delete a live agent's tree after a daemon restart.
#[tokio::test]
async fn delegation_state_reports_live_ended_and_unknown() {
    use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
    use crate::session_manager::worktree_ownership::AgentDelegationState;

    let (state, _dir, session) = hermetic();
    assert_eq!(
        super::delegation_state_for_agent(&state, "never-seen"),
        AgentDelegationState::Unknown,
        "an empty registry knows nothing; it does not know the agent is gone"
    );

    let mut running = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "work");
    running.agent_id = Some("agent-running".into());
    running.status = DelegationStatus::Running;
    state.upsert_delegation(running);
    assert_eq!(
        super::delegation_state_for_agent(&state, "agent-running"),
        AgentDelegationState::Live
    );

    let mut done = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "work");
    done.agent_id = Some("agent-done".into());
    done.status = DelegationStatus::Completed;
    state.upsert_delegation(done);
    assert_eq!(
        super::delegation_state_for_agent(&state, "agent-done"),
        AgentDelegationState::Ended
    );

    // `Stale` is neither live nor terminal — tracking lost the agent rather than
    // watching it exit — so it must NOT read as `Ended`. Same reading
    // `paths_in_use` gives it, and for the same reason.
    let mut lost = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "work");
    lost.agent_id = Some("agent-stale".into());
    lost.status = DelegationStatus::Stale;
    state.upsert_delegation(lost);
    assert_eq!(
        super::delegation_state_for_agent(&state, "agent-stale"),
        AgentDelegationState::Live
    );
}

/// #5800 REGRESSION: a `SubagentStop` reaps nothing, because a turn boundary is
/// not an exit.
///
/// Why: this is the defect verbatim. On 2026-08-18 an agent that had finished
/// and pushed its work to open PR #5990 had its worktree removed on its stop,
/// and the next `SendMessage` to it failed with "This agent cannot be resumed:
/// its worktree no longer exists". A `SubagentStop` reports that the agent's
/// TURN ended; the harness can still resume it afterwards, so nothing in that
/// event releases the tree.
///
/// The control is what gives this test its teeth. The fixture is a clean,
/// fully-pushed, correctly-attributed tree — it passes every surviving gate —
/// so its survival could otherwise be an artifact of some unrelated refusal.
/// Delivering the `SessionEnd` afterwards proves the same tree IS reapable
/// through the trigger that carries release evidence; only the stop declined.
/// Against the pre-fix commit the first `await_gone`-shaped wait is unnecessary:
/// `spawn_on_stop` removes the directory and the `wt.exists()` assertion below
/// fails.
#[tokio::test]
async fn subagent_stop_does_not_reap_a_resumable_worktree() {
    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree_under(&fx, "agent-resumable", "a5800", session);
    std::fs::write(wt.join("work.txt"), "finished and pushed\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    for event in [HookEvent::SubagentStop, HookEvent::SubagentStopFailure] {
        deliver(
            &state,
            session,
            event,
            serde_json::json!({ "agent_id": "a5800", "cwd": fx.repo.to_string_lossy() }),
        );
    }
    // A removal takes 1-3 s (git subprocesses plus a system-wide `lsof`), so a
    // same-instant assertion would pass against an implementation that reaps.
    // Give a reap every chance to land before asserting it did not happen.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    assert!(
        wt.exists(),
        "a stop is a turn boundary — the agent stays resumable and its tree must survive"
    );

    // The control: the same tree, through the trigger that does carry proof.
    deliver(
        &state,
        session,
        HookEvent::SessionEnd,
        serde_json::json!({ "cwd": fx.repo.to_string_lossy() }),
    );
    await_gone(&wt).await;
}

/// #5800 REGRESSION: a terminal delegation's worktree is still protected.
///
/// Why: `paths_in_use` dropped every terminal delegation from the protection set
/// until #5800, letting `is_terminal()` decide a question it cannot answer.
/// Terminal means trusty-mpm watched THAT delegation finish; the harness can
/// still hold the same tree open to resume the agent. Against the pre-fix body
/// this path is absent from the set and the assertion fails.
#[tokio::test]
async fn paths_in_use_protects_a_terminal_delegations_worktree() {
    use crate::core::agent::{Delegation, DelegationStatus, ModelTier};

    let (state, _dir, session) = hermetic();
    let finished = PathBuf::from("/r/.claude/worktrees/agent-finished");

    let mut done = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "work");
    done.agent_id = Some("a-finished".into());
    done.status = DelegationStatus::Completed;
    done.worktree_path = Some(finished.clone());
    state.upsert_delegation(done);

    let in_use = super::paths_in_use(&state, "a-stopping-agent").await;

    assert!(
        in_use.contains(&finished),
        "a finished delegation's tree can still be one the harness resumes into; got {in_use:?}"
    );
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
    let pid = child.id();
    let outcome = reap_worktree(&wt, AGENT, &[]);
    let _ = child.kill();
    let _ = child.wait();

    let reason = outcome
        .refusal()
        .expect("a worktree a live process stands in must be refused");
    // Name the PID, not just "still occupied": that prefix wraps EVERY liveness
    // refusal, including "could not run lsof", so asserting on it alone passes
    // on a machine where the probe never ran. This is the only test that
    // exercises the real probe positively, so it must prove the probe SAW this
    // process.
    assert!(
        reason.contains(&pid.to_string()),
        "the refusal must name the process it found (pid {pid}): {reason}"
    );
    assert!(wt.exists(), "the directory must survive");
}

/// #4311 REGRESSION: a sibling agent in ANOTHER session still holds its tree.
///
/// Why: `paths_in_use` read only `delegations_for(session)` until this review.
/// A session is not an isolation boundary for "is something running in this
/// directory" — an agent dispatched from a different session is exactly as
/// live. Under the pre-fix scoping this reap approved a path a sibling held.
#[tokio::test]
async fn reap_spares_a_worktree_another_sessions_agent_still_holds() {
    let (state, _dir, _session) = hermetic();
    let other_session = crate::core::session::SessionId(uuid::Uuid::new_v4());
    let held = PathBuf::from("/r/.claude/worktrees/agent-sibling");

    let mut sibling = crate::core::agent::Delegation::observed(
        other_session,
        "rust-engineer",
        "sibling work",
        Some("toolu_sibling".to_string()),
    );
    sibling.agent_id = Some("a-sibling".to_string());
    sibling.worktree_path = Some(held.clone());
    state.upsert_delegation(sibling);

    let in_use = super::paths_in_use(&state, "a-stopping-agent").await;

    assert!(
        in_use.contains(&held),
        "a live agent in another session must still protect its tree; got {in_use:?}"
    );
}

// ── the session-end trigger (#4311 follow-up, sole trigger since #5800) ─────

/// Deliver `event` through the real hook pipeline, as the daemon does.
///
/// Why the pipeline and not `spawn_on_session_end` directly: the defect was that
/// no trigger reached the reap at all, so a test calling the reap by hand would
/// prove the gate stack and skip the wiring that was actually missing.
fn deliver(
    state: &std::sync::Arc<crate::daemon::state::DaemonState>,
    session: crate::core::session::SessionId,
    event: HookEvent,
    payload: serde_json::Value,
) {
    crate::daemon::services::hook_service::HookService::new(std::sync::Arc::clone(state))
        .process(session, event, payload);
}

/// REGRESSION: a `SessionEnd` reaps the worktree of an agent whose
/// `SubagentStop` never arrived.
///
/// Why this test and not a happy-path one: `spawn_on_stop` was the reap's ONLY
/// trigger, so every exit route that emits no `SubagentStop` — the harness
/// session exiting or restarting under an in-flight agent, an interrupt, a
/// `tm hook` POST that lost its 2 s budget — left the tree registered, owned,
/// and unreclaimable. `prune_orphaned_worktrees` skips `SentinelOwner::Agent`
/// by design (#4311), so nothing else would ever pick it up: the leak was
/// permanent and, for this class, entirely unlogged.
///
/// The fixture delivers NO stop for `a801`. Against the pre-fix commit the
/// `SessionEnd` reaches `HookService::process`, which routes it to the overseer,
/// the idle nudge, the delegation tracker and the ring buffer — none of which
/// look at worktrees — and `await_gone` panics with the directory still there.
#[tokio::test]
async fn session_end_reaps_a_worktree_whose_agent_never_stopped() {
    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree_under(&fx, "agent-abandoned", "a801", session);
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    deliver(
        &state,
        session,
        HookEvent::SessionEnd,
        serde_json::json!({ "cwd": fx.repo.to_string_lossy() }),
    );

    await_gone(&wt).await;
}

/// Every agent of the ending session is reclaimed, not just the first.
///
/// Why: the sweep resolves each candidate's own `in_use` set, and each of these
/// trees is registered to a live-looking delegation of the SAME session. An
/// implementation that computed `paths_in_use` once, or that failed to exclude
/// each candidate's own registration, would find every tree "still in use" and
/// reclaim none.
#[tokio::test]
async fn session_end_reaps_every_agent_of_the_ending_session() {
    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let mut trees = Vec::new();
    for (name, agent) in [("agent-one", "a901"), ("agent-two", "a902")] {
        let wt = harness_worktree_under(&fx, name, agent, session);
        std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
        GitWorktreeFixture::commit_all_and_push(&wt, "agent work");
        let mut d = crate::core::agent::Delegation::observed(
            session,
            "rust-engineer",
            "work",
            Some(format!("toolu_{agent}")),
        );
        d.agent_id = Some(agent.to_string());
        d.worktree_path = Some(wt.clone());
        state.upsert_delegation(d);
        trees.push(wt);
    }

    deliver(
        &state,
        session,
        HookEvent::SessionEnd,
        serde_json::json!({ "cwd": fx.repo.to_string_lossy() }),
    );

    for wt in &trees {
        await_gone(wt).await;
    }
}

/// A tree holding unsaved work survives the session's end.
///
/// Why: the new trigger reuses `reap_worktree` unchanged, so the dirt gate must
/// still refuse. A `SessionEnd` proves the agent exited; it proves nothing about
/// whether its work was saved, and an uncommitted edit is the one thing this
/// module must never destroy.
///
/// Delete gate 5 (`dirt_blocks_removal`) from `reap_worktree` and this goes red:
/// the summary reports `removed: 1, kept: 0` and the directory is gone.
#[tokio::test]
async fn session_end_keeps_a_worktree_that_holds_unsaved_work() {
    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree_under(&fx, "agent-dirty-at-exit", "a803", session);
    std::fs::write(wt.join("README.md"), "edited, never committed\n").expect("write edit");

    let swept = settle(super::spawn_on_session_end(
        &state,
        session,
        HookEvent::SessionEnd,
        &serde_json::json!({ "cwd": fx.repo.to_string_lossy() }),
    ))
    .await;

    assert_eq!(
        swept,
        Some(super::SweepSummary {
            removed: 0,
            already_gone: 0,
            kept: 1,
        }),
        "the dirt gate must keep this tree, and the summary must count it as kept"
    );
    assert!(wt.exists(), "an uncommitted edit must survive its session");
}

/// Another session's agent is untouched by this session's end.
///
/// Why: the sweep's key is the sentinel's `parent_session_id`. Two PMs run
/// against one project store on this machine, so a sweep keyed on the store
/// alone would reclaim a live peer's tree the moment either session ended.
///
/// The control tree carries its weight: without it the ending session owns no
/// candidate, the sweep never starts, and "the peer survived" would be true of
/// an implementation that does nothing at all. With it the sweep provably runs
/// and provably removes something, so the peer's survival is the filter doing
/// its job. Delete `owner.parent_session_id == session` from
/// `agent_worktrees_of` and this goes red: `removed` is 2 and the peer is gone.
#[tokio::test]
async fn session_end_spares_another_sessions_agent() {
    let (state, _dir, session) = hermetic();
    let other = crate::core::session::SessionId(uuid::Uuid::new_v4());
    let fx = GitWorktreeFixture::new();
    let peer = harness_worktree_under(&fx, "agent-peer", "a804", other);
    std::fs::write(peer.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&peer, "agent work");
    let control = harness_worktree_under(&fx, "agent-mine", "a808", session);
    std::fs::write(control.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&control, "agent work");

    let swept = settle(super::spawn_on_session_end(
        &state,
        session,
        HookEvent::SessionEnd,
        &serde_json::json!({ "cwd": fx.repo.to_string_lossy() }),
    ))
    .await;

    assert_eq!(
        swept,
        Some(super::SweepSummary {
            removed: 1,
            already_gone: 0,
            kept: 0,
        }),
        "only the ending session's own tree is a candidate"
    );
    assert!(
        !control.exists(),
        "the control must be gone, or this test proves nothing about the sweep having run"
    );
    assert!(
        peer.exists(),
        "a peer session's agent worktree must survive"
    );
}

/// #5800: the legitimate reap still runs — known agent, terminal, released.
///
/// Why: #5800 tightened two gates and removed a trigger, and the failure mode of
/// such a change is a reap that now never fires. This is the arm that must
/// survive it: the registry KNOWS this agent (a delegation names it), that
/// delegation is TERMINAL, and the parent session ending is the release
/// evidence. All three hold, so the tree is removed.
///
/// It also pins the boundary the other two #5800 tests draw. The same terminal
/// delegation that `paths_in_use` now protects from a stop must NOT protect this
/// tree from its own session's end — `paths_in_use` excludes the candidate's own
/// agent, and if that exclusion were lost this test reports `kept: 1`.
#[tokio::test]
async fn session_end_reaps_a_finished_agents_released_worktree() {
    use crate::core::agent::{Delegation, DelegationStatus, ModelTier};

    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree_under(&fx, "agent-released", "a5801", session);
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    let mut done = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "work");
    done.agent_id = Some("a5801".into());
    done.status = DelegationStatus::Completed;
    done.worktree_path = Some(wt.clone());
    state.upsert_delegation(done);

    let swept = settle(super::spawn_on_session_end(
        &state,
        session,
        HookEvent::SessionEnd,
        &serde_json::json!({ "cwd": fx.repo.to_string_lossy() }),
    ))
    .await;

    assert_eq!(
        swept,
        Some(super::SweepSummary {
            removed: 1,
            already_gone: 0,
            kept: 0,
        }),
        "known + terminal + released is the population this reap exists for"
    );
    assert!(!wt.exists(), "the directory must be gone: {}", wt.display());
}

/// #5800: an agent stamped on TWO directories owns neither, so both survive.
///
/// Why: nothing deletes a stale sentinel and registration re-fires on every
/// `PreToolUse`, so an agent that moved between trees leaves the first one
/// stamped — and this store is measurably in that state
/// (`agent-a23097670a51a0450`'s sentinel recorded `agent_id: a5ed76bc80fd52e41`
/// on 2026-08-18). Which tree such an agent owns is undeterminable, and ADR-0045
/// forbids resolving an undeterminable owner by picking one.
///
/// The control tree carries the same weight it does in
/// `session_end_spares_another_sessions_agent`: without it the sweep could
/// reclaim nothing for an unrelated reason and this test would still pass.
/// Remove the `find_agent_worktree` filter from `agent_worktrees_of` and this
/// goes red — `removed` is 3 and both ambiguous directories are gone.
#[tokio::test]
async fn session_end_keeps_a_worktree_whose_agent_names_two_directories() {
    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let mut ambiguous = Vec::new();
    for name in ["agent-moved-from", "agent-moved-to"] {
        let wt = harness_worktree_under(&fx, name, "a5802", session);
        std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
        GitWorktreeFixture::commit_all_and_push(&wt, "agent work");
        ambiguous.push(wt);
    }
    let control = harness_worktree_under(&fx, "agent-unambiguous", "a5803", session);
    std::fs::write(control.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&control, "agent work");

    let swept = settle(super::spawn_on_session_end(
        &state,
        session,
        HookEvent::SessionEnd,
        &serde_json::json!({ "cwd": fx.repo.to_string_lossy() }),
    ))
    .await;

    assert_eq!(
        swept,
        Some(super::SweepSummary {
            removed: 1,
            already_gone: 0,
            kept: 0,
        }),
        "only the unambiguous tree is a candidate; the ambiguous pair is not swept at all"
    );
    assert!(
        !control.exists(),
        "the control must be gone, or this test proves nothing about the sweep having run"
    );
    for wt in &ambiguous {
        assert!(
            wt.exists(),
            "an undeterminable owner keeps every candidate: {}",
            wt.display()
        );
    }
}

/// A `SessionEnd` with no `cwd` resolves no store and reclaims nothing.
///
/// Delete the `cwd` guard from `store_for` and this goes red at the `None`:
/// a store resolves, so the sweep starts.
#[tokio::test]
async fn session_end_without_a_cwd_reaps_nothing() {
    let (state, _dir, session) = hermetic();
    let fx = GitWorktreeFixture::new();
    let wt = harness_worktree_under(&fx, "agent-no-cwd", "a805", session);
    std::fs::write(wt.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&wt, "agent work");

    let swept = settle(super::spawn_on_session_end(
        &state,
        session,
        HookEvent::SessionEnd,
        &serde_json::json!({}),
    ))
    .await;

    assert_eq!(swept, None, "an unresolvable store must start no sweep");
    assert!(wt.exists(), "an unresolvable store must reclaim nothing");
}

/// The `cwd` guard itself, asserted on the resolution rather than on a survivor.
///
/// Why both this and the test above: delete the guard and the natural fallback
/// is the process's own directory, which resolves to the REAL harness store on
/// this machine. That store holds no sentinel naming a random test session, so
/// the sweep finds no candidate and the survivor test above still passes — while
/// the daemon has in fact read the live worktree store off a payload that named
/// no directory at all. Only the resolution catches that.
#[test]
fn session_end_without_a_cwd_resolves_no_store() {
    assert_eq!(
        super::store_for(&serde_json::json!({})),
        None,
        "a payload with no cwd must resolve no store"
    );
    assert_eq!(
        super::store_for(&serde_json::json!({ "cwd": 7 })),
        None,
        "a non-string cwd is not a path"
    );
}

/// Await the sweep itself, so a negative assertion runs AFTER the decision.
///
/// Why this replaced a fixed 750 ms sleep: that window was unreachable. Gate 6
/// (`process_holding`) shells out to `lsof`, measured at 0.56-0.67 s on an idle
/// machine, and it runs after a `git rev-parse` and the dirt gate's several git
/// subprocesses and before the `git worktree remove` itself. A real removal
/// lands at roughly 1-3 s idle and later under load, so every "this tree must
/// survive" assertion sampled the filesystem while the removal was still in
/// flight — and passed with the gate it existed to protect deleted. Awaiting the
/// sweep's own handle removes the window instead of widening it.
///
/// `None` is not a failure: it is [`super::spawn_on_session_end`] reporting that
/// no sweep ran at all, which for two of the tests below is the whole point.
async fn settle(
    handle: Option<tokio::task::JoinHandle<super::SweepSummary>>,
) -> Option<super::SweepSummary> {
    match handle {
        Some(h) => Some(h.await.expect("the sweep task must not panic")),
        None => None,
    }
}

/// Poll until `path` is gone, or fail with what is still there.
///
/// Why: the two tests that still use this go through `HookService::process`,
/// which drops the sweep's handle — that is the point, since the defect was that
/// the pipeline reached no reap at all. With no handle to await, a POSITIVE
/// assertion has to poll the condition. The negative tests do have a handle and
/// await it instead; polling cannot prove a removal that must never happen.
///
/// The budget is 60 s and not the 10 s it started at. One reap shells out to
/// `git status`, `git worktree remove` and a system-wide `lsof` that costs about
/// a second on an idle machine; inside a 5000-test parallel run competing with
/// other builds it costs far more. At 10 s these tests passed alone and failed
/// in the full suite — a timing artifact, not a reap that declined. A correct
/// implementation still finishes in well under a second, so a wider budget
/// costs a passing run nothing and only changes how long a genuine failure
/// takes to report.
async fn await_gone(path: &std::path::Path) {
    for _ in 0..600 {
        if !path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{} was never removed", path.display());
}
