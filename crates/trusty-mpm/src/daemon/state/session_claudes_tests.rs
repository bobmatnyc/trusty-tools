//! Coverage for the kernel-bound session `claude` registry (#8531).

use super::*;
use crate::core::agent::Delegation;
use crate::core::hook::HookEvent;
use crate::daemon::state::session_claude_liveness::claude_liveness_with;

const CLAUDE: ClaudeProcess = ClaudeProcess {
    pid: 300,
    start_time: 50,
};

/// A process table: 100 (the peer) → 200 (a shell) → 300 (`claude`).
fn table(pid: u32) -> Result<ProcessFacts, String> {
    let (parent, start_time) = match pid {
        100 => (200, 90),
        200 => (300, 60),
        300 => (1, 50),
        _ => return Err(format!("no entry for {pid}")),
    };
    Ok(ProcessFacts {
        parent: Some(parent),
        start_time,
    })
}

/// #8531: first writer wins; a second announcement never rebinds.
#[test]
fn a_session_is_bound_once_first_writer_wins_8531() {
    let state = DaemonState::new();
    let session = SessionId::new();
    assert_eq!(state.session_claudes().get(session), None);
    state.bind_session_claude(session, CLAUDE).expect("vacant");
    let other = ClaudeProcess {
        pid: 999,
        start_time: 1,
    };
    assert!(state.bind_session_claude(session, other).is_err());
    assert_eq!(state.session_claudes().get(session), Some(CLAUDE));
}

/// #8531 Fail-Open Check: an id that already owns records — announced to
/// the daemon by a process nothing identified — is never bound.
#[test]
fn a_session_that_already_owns_records_is_not_bound_8531() {
    let state = DaemonState::new();
    let session = SessionId::new();
    state.upsert_delegation(Delegation::observed(session, "engineer", "task", None));
    let got = state.bind_session_claude(session, CLAUDE);
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("already owns delegation records")),
        "{got:?}"
    );
    assert_eq!(state.session_claudes().get(session), None);
}

/// #8531: the peer's nearest `claude`, with its start time.
#[test]
fn the_peer_claude_is_the_nearest_claude_ancestor_8531() {
    let got = peer_claude_with(100, 1_000, table, |pid| Ok(pid == 300));
    assert_eq!(got, Ok(CLAUDE));
}

/// #8531 MEDIUM, Fail-Open Check: a peer pid now naming a process that
/// started after the request arrived is a reused pid, and is refused before
/// any walk.
#[test]
fn a_peer_started_after_the_request_is_refused_8531() {
    let got = peer_claude_with(100, 89, table, |_| panic!("no walk for a reused pid"));
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("started after the request arrived")),
        "{got:?}"
    );
}

/// #8531 Fail-Open Check: a peer or an ancestor the table cannot read
/// refuses.
#[test]
fn an_unreadable_peer_is_refused_8531() {
    let got = peer_claude_with(7, 1_000, table, |_| Ok(false));
    assert!(
        got.as_ref().is_err_and(|e| e.contains("no entry for 7")),
        "{got:?}"
    );
    let got = peer_claude_with(100, 1_000, table, |_| Err("ps failed".to_string()));
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("ancestry") && e.contains("ps failed")),
        "{got:?}"
    );
}

/// A daemon over `root`, as a restart would build it.
fn daemon_at(root: &std::path::Path) -> DaemonState {
    DaemonState::with_paths(&crate::core::paths::FrameworkPaths::under(root))
}

/// The registry file of `state`.
fn registry_file(state: &DaemonState) -> std::path::PathBuf {
    state.framework_root().join(SESSION_CLAUDES_FILE)
}

/// A valid registry file holding one binding of `session` to [`CLAUDE`], at
/// mode `mode`; returns its path.
fn written_registry(root: &std::path::Path, session: SessionId, mode: u32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let state = daemon_at(root);
    state.bind_session_claude(session, CLAUDE).expect("vacant");
    let path = registry_file(&state);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
}

/// Whether this process would bypass the mode bits a test relies on.
fn running_as_root() -> bool {
    current_uid() == 0
}

/// #8531 HIGH: a binding survives a restart, in an owner-only file, and the
/// restarted daemon still refuses a second binding of the id.
#[test]
fn a_restart_keeps_the_binding_8531() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("tempdir");
    let session = SessionId::new();
    let before = daemon_at(root.path());
    before.bind_session_claude(session, CLAUDE).expect("vacant");
    let path = registry_file(&before);
    let mode = std::fs::metadata(&path)
        .expect("saved")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "owner-only: {mode:o}");
    drop(before);

    let after = daemon_at(root.path());
    assert_eq!(after.session_claudes().sealed(), None);
    assert_eq!(after.session_claudes().get(session), Some(CLAUDE));
    let sibling = ClaudeProcess {
        pid: 999,
        start_time: 1,
    };
    assert!(after.bind_session_claude(session, sibling).is_err());
    assert_eq!(after.session_claudes().get(session), Some(CLAUDE));
}

/// #8531: a daemon state from before the file existed starts empty — no
/// owner is granted — and is not sealed.
#[test]
fn a_missing_file_is_an_empty_registry_8531() {
    let dir = tempfile::tempdir().expect("tempdir");
    let claudes = SessionClaudes::load_as(dir.path().join(SESSION_CLAUDES_FILE), current_uid());
    assert_eq!(claudes.sealed(), None);
    assert!(!claudes.is_settled(SessionId::new()));
}

/// Assert `claudes` is sealed for a reason containing `why`, grants nothing
/// for `session`, and records nothing.
fn assert_sealed(claudes: &SessionClaudes, session: SessionId, why: &str) {
    let sealed = claudes.sealed().expect("sealed");
    assert!(sealed.contains(why), "{sealed}");
    assert_eq!(
        claudes.get(session),
        None,
        "a sealed registry grants nothing"
    );
    assert!(
        claudes.is_settled(session),
        "a sealed registry records nothing"
    );
    let got = claudes.record(SessionId::new(), Announcement::Claude(CLAUDE), || Ok(()));
    assert!(got.is_err_and(|e| e.contains("sealed")));
}

/// #8531 Fail-Open Check: an unparseable file seals the registry and is
/// left as it was.
#[test]
fn a_corrupt_file_seals_the_registry_8531() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = SessionId::new();
    let path = written_registry(root.path(), session, 0o600);
    std::fs::write(&path, b"{\"version\":1,\"sessions\":").expect("corrupt it");
    let claudes = SessionClaudes::load_as(path.clone(), current_uid());
    assert_sealed(&claudes, session, "does not parse");
    assert_eq!(
        std::fs::read(&path).expect("still there"),
        b"{\"version\":1,\"sessions\":"
    );
}

/// #8531 Fail-Open Check: a file another uid owns is not trusted, though
/// it parses and names the session.
#[test]
fn a_foreign_owned_file_seals_the_registry_8531() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = SessionId::new();
    let path = written_registry(root.path(), session, 0o600);
    let claudes = SessionClaudes::load_as(path.clone(), current_uid() + 1);
    assert_sealed(&claudes, session, "owned by uid");
    // A symlink to a valid file is refused, never followed.
    let link = root.path().join("linked.json");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");
    let claudes = SessionClaudes::load_as(link, current_uid());
    assert_sealed(&claudes, session, "could not be opened");
}

/// #8531 Fail-Open Check: a file other users may write is not trusted.
#[test]
fn a_file_open_to_other_users_seals_the_registry_8531() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = SessionId::new();
    let path = written_registry(root.path(), session, 0o620);
    let claudes = SessionClaudes::load_as(path, current_uid());
    assert_sealed(&claudes, session, "only its owner may");
}

/// #8531 Fail-Open Check: a file the daemon cannot read seals the registry.
#[test]
fn an_unreadable_file_seals_the_registry_8531() {
    if running_as_root() {
        eprintln!("running as root bypasses the mode bits; skipping");
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let session = SessionId::new();
    let path = written_registry(root.path(), session, 0o000);
    let claudes = SessionClaudes::load_as(path, current_uid());
    assert_sealed(&claudes, session, "could not be opened");
}

/// #8531 Fail-Open Check: a binding that cannot be saved does not count,
/// so a restart can never forget a binding that granted.
#[test]
fn an_unsaved_binding_does_not_count_8531() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("not-a-dir");
    let claudes = SessionClaudes::load_as(blocker.join(SESSION_CLAUDES_FILE), current_uid());
    assert_eq!(claudes.sealed(), None, "absent: an empty registry");
    std::fs::write(&blocker, b"").expect("a file where the directory goes");
    let session = SessionId::new();
    let got = claudes.record(session, Announcement::Claude(CLAUDE), || Ok(()));
    assert!(got.is_err_and(|e| e.contains("could not be saved")));
    assert_eq!(claudes.get(session), None);
    assert!(!claudes.is_settled(session), "rolled back");
}

/// #8531 MEDIUM, Fail-Open Check: an unproven announcement that cannot be
/// saved still settles the id, so the next socket announcer is not bound.
#[test]
fn an_unsaved_unproven_announcement_still_settles_the_id_8531() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("not-a-dir");
    let claudes = SessionClaudes::load_as(blocker.join(SESSION_CLAUDES_FILE), current_uid());
    std::fs::write(&blocker, b"").expect("a file where the directory goes");
    let session = SessionId::new();
    let got = claudes.record(session, Announcement::Unproven, || Ok(()));
    assert!(got.is_err_and(|e| e.contains("could not be saved")));
    assert!(claudes.is_settled(session), "kept: it can only deny");
    let got = claudes.record(session, Announcement::Claude(CLAUDE), || Ok(()));
    assert_eq!(got, Ok(Some(Announcement::Unproven)));
    assert_eq!(claudes.get(session), None);
}

/// #8531: an id whose first announcement proved no process is never bound,
/// before or after a restart.
#[test]
fn an_unproven_session_is_never_bound_8531() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = SessionId::new();
    let before = daemon_at(root.path());
    before.settle_unproven_session(session).expect("recorded");
    let got = before.bind_session_claude(session, CLAUDE);
    assert!(got.is_err_and(|e| e.contains("without a kernel-verified claude")));
    drop(before);
    let after = daemon_at(root.path());
    assert!(after.bind_session_claude(session, CLAUDE).is_err());
    assert_eq!(after.session_claudes().get(session), None);
    // Settling a bound id leaves its binding.
    let bound = SessionId::new();
    after.bind_session_claude(bound, CLAUDE).expect("vacant");
    after.settle_unproven_session(bound).expect("no-op");
    assert_eq!(after.session_claudes().get(bound), Some(CLAUDE));
}

/// #8531 Fail-Open Check: a peer with no `claude` above it runs in no
/// session.
#[test]
fn a_peer_with_no_claude_above_it_is_refused_8531() {
    let got = peer_claude_with(100, 1_000, table, |_| Ok(false));
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("no claude session process")),
        "{got:?}"
    );
}

/// #8980 HIGH regression: an HTTP `SessionStart` naming a bound owner's id
/// auto-registers a record whose uuid-derived name is never a live session.
/// The reaper must leave that record, and the owner's live records, alone; a
/// daemon-minted session in the same sweep is still reaped.
#[tokio::test]
async fn the_reaper_keeps_an_announced_session_and_its_live_records_8980() {
    use crate::core::session::{ControlModel, Session};
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let owner = SessionId::new();
    // #9010: bound to a claude that still runs, so the reaper keeps it.
    state
        .bind_session_claude(owner, this_process())
        .expect("vacant");
    state.upsert_delegation(Delegation::observed(owner, "version-control", "task", None));
    let forged = crate::daemon::api::HookPost {
        session_id: owner.0.to_string(),
        event: crate::core::hook::HookEvent::SessionStart,
        payload: serde_json::json!({}),
    };
    crate::daemon::rpc::sessions_legacy_ops::ingest_hook(&state, forged)
        .await
        .expect("SessionStart");
    let minted = Session::new(SessionId::new(), "/repo", ControlModel::Tmux, None);
    let minted_id = minted.id;
    state.register_session(minted);

    let result = state.reap_against(&std::collections::HashSet::new());

    assert_eq!(result.reaped, 1, "only the daemon-minted session is reaped");
    assert!(state.session(minted_id).is_none());
    assert!(state.session(owner).is_some(), "the announced record stays");
    assert_eq!(
        state.all_delegations()[0].status,
        crate::core::agent::DelegationStatus::Running
    );
}

/// #8980 Fail-Open Check: while the registry is sealed the reaper skips
/// every session, even a daemon-minted Tmux one with no live tmux name, so
/// no record goes and no delegation is staled.
#[tokio::test]
async fn a_sealed_registry_reaps_nothing_8980() {
    use crate::core::session::{ControlModel, Session};
    let root = tempfile::tempdir().expect("tempdir");
    let file = registry_file(&daemon_at(root.path()));
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, b"{\"version\":1,\"sessions\":").expect("corrupt it");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    assert!(state.session_claudes().sealed().is_some(), "loaded sealed");
    let minted = Session::new(SessionId::new(), "/repo", ControlModel::Tmux, None);
    let minted_id = minted.id;
    state.register_session(minted);
    state.upsert_delegation(Delegation::observed(minted_id, "engineer", "task", None));

    let result = state.reap_against(&std::collections::HashSet::new());

    assert_eq!(result.reaped, 0, "a sealed registry reaps nothing");
    assert!(state.session(minted_id).is_some(), "the record stays");
    assert_eq!(
        state.all_delegations()[0].status,
        crate::core::agent::DelegationStatus::Running
    );
}

/// Ingest one hook `event` for `session` over the shared (HTTP) body.
async fn ingest(state: &std::sync::Arc<DaemonState>, session: SessionId, event: HookEvent) {
    let post = crate::daemon::api::HookPost {
        session_id: session.0.to_string(),
        event,
        payload: serde_json::json!({}),
    };
    crate::daemon::rpc::sessions_legacy_ops::ingest_hook(state, post)
        .await
        .expect("ingested");
}

/// #8984: an id whose `SessionStart` never reached the daemon is settled by
/// its first in-session event, so a sibling's later `SessionStart` — the
/// socket bind `bind_announcing_claude` makes after its walk — binds nothing.
#[tokio::test]
async fn a_forged_session_start_after_another_event_binds_nothing_8984() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let session = SessionId::new();
    ingest(&state, session, HookEvent::PreToolUse).await;

    let forged = state.bind_session_claude(session, CLAUDE);

    assert!(
        forged
            .as_ref()
            .is_err_and(|e| e.contains("without a kernel-verified claude")),
        "{forged:?}"
    );
    assert_eq!(state.session_claudes().get(session), None);
    // The settle is persisted, so a restart does not reopen the id.
    let restarted = daemon_at(root.path());
    assert!(restarted.session_claudes().is_settled(session));
}

/// #8984: a start-up event does not settle the id, so the session's own
/// `SessionStart`, arriving after it, still binds.
#[tokio::test]
async fn a_start_up_event_leaves_the_id_open_to_its_session_start_8984() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let session = SessionId::new();
    ingest(&state, session, HookEvent::InstructionsLoaded).await;
    assert!(!state.session_claudes().is_settled(session));
    state.bind_session_claude(session, CLAUDE).expect("vacant");
    assert_eq!(state.session_claudes().get(session), Some(CLAUDE));
}

/// #8984: a later event that races a legitimate socket `SessionStart`
/// already in flight leaves the id to that bind.
#[tokio::test]
async fn an_in_flight_session_start_still_binds_8984() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let session = SessionId::new();
    let in_flight = state.session_claudes().begin_bind(session);
    let second = state.session_claudes().begin_bind(session);
    drop(second);
    ingest(&state, session, HookEvent::PreToolUse).await;
    assert!(
        !state.session_claudes().is_settled(session),
        "left to the bind"
    );
    state.bind_session_claude(session, CLAUDE).expect("vacant");
    drop(in_flight);
    assert_eq!(state.session_claudes().get(session), Some(CLAUDE));
    // With no bind in flight, the next unannounced id is settled.
    let other = SessionId::new();
    ingest(&state, other, HookEvent::Stop).await;
    assert!(state.session_claudes().is_settled(other));
}

/// #8984 Fail-Open Check: a settle that cannot be saved still settles the id
/// in memory, and a sealed registry records nothing and says why.
#[test]
fn an_unsaved_settle_after_an_event_still_refuses_a_bind_8984() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("not-a-dir");
    let claudes = SessionClaudes::load_as(blocker.join(SESSION_CLAUDES_FILE), current_uid());
    std::fs::write(&blocker, b"").expect("a file where the directory goes");
    let session = SessionId::new();
    let got = claudes.settle_after_event(session);
    assert!(got.is_err_and(|e| e.contains("could not be saved")));
    assert!(claudes.is_settled(session), "kept: it can only deny");
    let got = claudes.record(session, Announcement::Claude(CLAUDE), || Ok(()));
    assert_eq!(got, Ok(Some(Announcement::Unproven)));

    let root = tempfile::tempdir().expect("tempdir");
    let file = root.path().join(SESSION_CLAUDES_FILE);
    std::fs::write(&file, b"{").expect("corrupt it");
    let sealed = SessionClaudes::load_as(file, current_uid());
    let got = sealed.settle_after_event(session);
    assert!(got.is_err_and(|e| e.contains("sealed")));
    assert_eq!(sealed.get(session), None);
}

/// This test process, as a bound `claude` that runs for the whole test.
fn this_process() -> ClaudeProcess {
    let pid = std::process::id();
    let facts = process_facts(pid).expect("this process is in the table");
    ClaudeProcess {
        pid,
        start_time: facts.start_time,
    }
}

/// A pid no process holds: this test's own child, spawned and reaped.
fn reaped_child_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn a process that exits at once");
    let pid = child.id();
    child.wait().expect("reap the child");
    pid
}

/// Register a harness-shaped record for a new id bound to `claude`, with one
/// live delegation; returns the id.
fn bound_session(state: &DaemonState, claude: ClaudeProcess) -> SessionId {
    use crate::core::session::{ControlModel, Session};
    let session = SessionId::new();
    state.bind_session_claude(session, claude).expect("vacant");
    state.register_session(Session::new(session, "", ControlModel::Tmux, None));
    state.upsert_delegation(Delegation::observed(session, "engineer", "task", None));
    session
}

/// The status of `session`'s one delegation.
fn delegation_status(
    state: &DaemonState,
    session: SessionId,
) -> crate::core::agent::DelegationStatus {
    state
        .all_delegations()
        .into_iter()
        .find(|d| d.session == session)
        .expect("one delegation")
        .status
}

/// #9010: a settled session whose bound claude exited — or whose pid now
/// names a later process — is reaped and its live records staled; one whose
/// claude still runs is kept.
#[test]
fn a_settled_session_whose_claude_exited_is_reaped_9010() {
    use crate::core::agent::DelegationStatus;
    let root = tempfile::tempdir().expect("tempdir");
    let state = daemon_at(root.path());
    let alive = bound_session(&state, this_process());
    let exited = bound_session(
        &state,
        ClaudeProcess {
            pid: reaped_child_pid(),
            start_time: 1,
        },
    );
    let reused = bound_session(
        &state,
        ClaudeProcess {
            pid: std::process::id(),
            start_time: this_process().start_time.saturating_sub(1),
        },
    );

    let result = state.reap_against(&std::collections::HashSet::new());

    assert_eq!(result.reaped, 2, "{result:?}");
    assert!(state.session(alive).is_some(), "a running claude keeps it");
    assert_eq!(delegation_status(&state, alive), DelegationStatus::Running);
    for gone in [exited, reused] {
        assert!(state.session(gone).is_none(), "reaped");
        assert_eq!(delegation_status(&state, gone), DelegationStatus::Stale);
    }
}

/// #9010 Fail-Open Check: a probe that cannot answer keeps the session and
/// its live records.
#[test]
fn an_unanswered_probe_keeps_a_settled_session_9010() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = daemon_at(root.path());
    let session = bound_session(&state, CLAUDE);

    let result = state.reap_against_with(&std::collections::HashSet::new(), |_| {
        ClaudeLiveness::Unknown("the process table is unreadable".to_string())
    });

    assert_eq!(result.reaped, 0);
    assert!(state.session(session).is_some());
    assert_eq!(
        delegation_status(&state, session),
        crate::core::agent::DelegationStatus::Running
    );
}

/// #9010: only proof calls a bound claude gone; every unanswered arm is
/// `Unknown`. A start-time mismatch is a reused pid only when the process
/// now holding the pid is no claude; a claude there, or a name lookup that
/// fails, may be the bound claude after a wall-clock step.
#[test]
fn claude_liveness_needs_proof_to_call_a_claude_gone_9010() {
    let facts = |start_time| {
        move |_| {
            Ok(ProcessFacts {
                parent: None,
                start_time,
            })
        }
    };
    let no_name = |_| -> Result<bool, String> { panic!("no name lookup") };
    let alive = claude_liveness_with(CLAUDE, |_| Ok(true), facts(CLAUDE.start_time), no_name);
    assert_eq!(alive, ClaudeLiveness::Alive);
    let gone = claude_liveness_with(
        CLAUDE,
        |_| Ok(false),
        |_| panic!("no read of a gone pid"),
        no_name,
    );
    assert!(matches!(gone, ClaudeLiveness::Gone(_)), "{gone:?}");
    let shifted = facts(CLAUDE.start_time + 1);
    let reused = claude_liveness_with(CLAUDE, |_| Ok(true), shifted, |_| Ok(false));
    assert!(matches!(reused, ClaudeLiveness::Gone(ref why) if why.contains("not a claude")));
    let stepped = claude_liveness_with(CLAUDE, |_| Ok(true), shifted, |_| Ok(true));
    assert!(
        matches!(stepped, ClaudeLiveness::Unknown(ref why) if why.contains("it is a claude")),
        "{stepped:?}"
    );
    let unnamed = claude_liveness_with(CLAUDE, |_| Ok(true), shifted, |_| Err("EIO".into()));
    assert!(
        matches!(unnamed, ClaudeLiveness::Unknown(ref why) if why.contains("could not be named")),
        "{unnamed:?}"
    );
    let unanswered = claude_liveness_with(CLAUDE, |_| Err("EIO".to_string()), facts(0), no_name);
    assert!(
        matches!(unanswered, ClaudeLiveness::Unknown(_)),
        "{unanswered:?}"
    );
    let unreadable =
        claude_liveness_with(CLAUDE, |_| Ok(true), |_| Err("gone".to_string()), no_name);
    assert!(
        matches!(unreadable, ClaudeLiveness::Unknown(_)),
        "{unreadable:?}"
    );
}

/// An exited `claude`: this test's own child, spawned and reaped.
fn exited_claude() -> ClaudeProcess {
    ClaudeProcess {
        pid: reaped_child_pid(),
        start_time: 1,
    }
}

/// #9010 HIGH regression: a session whose bound claude exited is kept while
/// it still produces hook events — `claude --resume <id>` run outside the
/// daemon keeps the id bound to the old pid. An event older than one reap
/// interval holds nothing.
#[tokio::test]
async fn a_settled_session_with_a_recent_event_is_kept_9010() {
    use crate::core::agent::DelegationStatus;
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let session = bound_session(&state, exited_claude());
    ingest(&state, session, HookEvent::PreToolUse).await;

    let result = state.reap_against(&std::collections::HashSet::new());

    assert_eq!(result.reaped, 0, "{result:?}");
    assert!(state.session(session).is_some(), "a recent event keeps it");
    assert_eq!(
        delegation_status(&state, session),
        DelegationStatus::Running
    );

    let window = std::time::Duration::from_secs(crate::daemon::REAP_INTERVAL_SECS + 1);
    let long_ago = std::time::Instant::now()
        .checked_sub(window)
        .expect("the host has been up longer than one reap interval");
    state
        .session_claudes()
        .events
        .lock()
        .insert(session, long_ago);
    let result = state.reap_against(&std::collections::HashSet::new());
    assert_eq!(result.reaped, 1, "an old event holds nothing: {result:?}");
    assert_eq!(delegation_status(&state, session), DelegationStatus::Stale);
}

/// #9010 HIGH regression: the reaper re-checks a gone claude's session just
/// before removing it. A rebind or a resume grant that lands after the walk,
/// or a later announcer that still runs, keeps the session.
#[test]
fn a_session_rebound_during_the_reap_is_kept_9010() {
    use crate::core::agent::DelegationStatus;
    let none = std::collections::HashSet::new();
    let gone = || ClaudeLiveness::Gone("exited".to_string());
    // Each case runs on its own daemon, so no case's session is in another's
    // sweep.
    let case = || {
        let root = tempfile::tempdir().expect("tempdir");
        let state = daemon_at(root.path());
        (root, state)
    };

    let (_root, state) = case();
    let rebound = bound_session(&state, CLAUDE);
    let result = state.reap_against_with(&none, |_| {
        state
            .session_claudes()
            .rebind(rebound, resumed_claude())
            .expect("rebound during the probe");
        gone()
    });
    assert_eq!(result.reaped, 0, "a rebind after the walk keeps it");
    assert!(state.session(rebound).is_some());
    assert_eq!(
        delegation_status(&state, rebound),
        DelegationStatus::Running
    );

    let (_root, state) = case();
    let resuming = bound_session(&state, CLAUDE);
    let id = resuming.0.to_string();
    let result = state.reap_against_with(&none, |_| {
        state.grant_resumed_session(Some(&id), "tm-resumed", Some("%7"));
        gone()
    });
    assert_eq!(result.reaped, 0, "a grant after the walk keeps it");
    assert!(state.session(resuming).is_some());

    let (_root, state) = case();
    let announced = bound_session(&state, exited_claude());
    state
        .session_claudes()
        .note_announcer(announced, this_process());
    let result = state.reap_against(&none);
    assert_eq!(result.reaped, 0, "a running later announcer keeps it");
    assert_eq!(
        delegation_status(&state, announced),
        DelegationStatus::Running
    );
    let result = state.reap_against_with(&none, |_| gone());
    assert_eq!(
        result.reaped, 1,
        "a gone announcer holds nothing: {result:?}"
    );
    assert!(state.session(announced).is_none());
}

/// #9010: a resume grant holds a dead session only for [`RESUME_HOLD_SECS`].
/// A grant no rebind consumed — a failed launch, rebind or HTTP announce —
/// lets the reaper remove the session once it is that old.
#[test]
fn a_resume_grant_holds_a_reap_only_for_a_bounded_time_9010() {
    let none = std::collections::HashSet::new();
    let gone = |_| ClaudeLiveness::Gone("exited".to_string());
    let grant = |issued_at| ResumeGrant {
        tmux_name: "tm-resumed".to_string(),
        pane_id: Some("%7".to_string()),
        issued_at,
    };
    let now = now_secs();
    assert!(
        grant(now + 30).holds_reap(now),
        "a clock stepped back holds"
    );
    assert!(grant(now - RESUME_HOLD_SECS + 1).holds_reap(now));
    assert!(!grant(now - RESUME_HOLD_SECS).holds_reap(now));

    let root = tempfile::tempdir().expect("tempdir");
    let state = daemon_at(root.path());
    let fresh = bound_session(&state, CLAUDE);
    state
        .session_claudes()
        .grant_resume(fresh, grant(now_secs()));
    let result = state.reap_against_with(&none, gone);
    assert_eq!(result.reaped, 0, "a fresh grant keeps it: {result:?}");
    assert!(state.session(fresh).is_some());

    let root = tempfile::tempdir().expect("tempdir");
    let state = daemon_at(root.path());
    let expired = bound_session(&state, CLAUDE);
    let stale = now_secs() - RESUME_HOLD_SECS - 1;
    state.session_claudes().grant_resume(expired, grant(stale));
    let result = state.reap_against_with(&none, gone);
    assert_eq!(
        result.reaped, 1,
        "an expired grant holds nothing: {result:?}"
    );
    assert!(state.session(expired).is_none());
}

/// #8984 LOW: the socket ingest holds the bind's in-flight guard through the
/// bind, so a later event racing it leaves the id to the bind. Dropping the
/// guard before the bind — `let _ = begin_bind(..)` — fails this test.
#[tokio::test]
async fn the_socket_bind_runs_inside_its_in_flight_guard_8984() {
    use crate::daemon::rpc::sessions_legacy_ops::ingest_hook_from_socket_with;
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let session = SessionId::new();
    let post = crate::daemon::api::HookPost {
        session_id: session.0.to_string(),
        event: HookEvent::SessionStart,
        payload: serde_json::json!({}),
    };

    let bind =
        |held: std::sync::Arc<DaemonState>,
         id: SessionId,
         _peer: crate::daemon::services::delegation_repair_caller::RepairPeer| async move {
            let in_flight = held.session_claudes().binding.lock().get(&id).copied();
            assert_eq!(in_flight, Some(1), "the guard is held during the bind");
            let raced = held.session_claudes().settle_after_event(id);
            assert_eq!(raced, Ok(false), "a racing event leaves the id to the bind");
            held.bind_session_claude(id, CLAUDE)
        };
    ingest_hook_from_socket_with(&state, post, None, bind)
        .await
        .expect("ingested");

    assert_eq!(state.session_claudes().get(session), Some(CLAUDE));
    assert!(
        state.session_claudes().binding.lock().is_empty(),
        "released after the ingest"
    );
}

/// Unix seconds now.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs()
}

/// The `claude` the daemon's resume put in the pane: started after the grant.
fn resumed_claude() -> ClaudeProcess {
    ClaudeProcess {
        pid: 400,
        start_time: now_secs() + 5,
    }
}

/// A session first bound to [`CLAUDE`], owning one live delegation, that the
/// daemon is resuming into pane `%7` of `tm-resumed`.
fn resumed_session(state: &DaemonState) -> SessionId {
    let session = SessionId::new();
    state.bind_session_claude(session, CLAUDE).expect("vacant");
    state.upsert_delegation(Delegation::observed(session, "engineer", "task", None));
    state.grant_resumed_session(Some(&session.0.to_string()), "tm-resumed", Some("%7"));
    session
}

/// A kernel peer seen well after every test claude started.
fn kernel_peer() -> crate::daemon::services::delegation_repair_caller::RepairPeer {
    crate::daemon::services::delegation_repair_caller::RepairPeer::Kernel {
        pid: 401,
        seen_at: now_secs() + 60,
    }
}

/// The caller [`establish_caller_with`] finds for `caller` over `session`'s
/// records, with `live` as the only running process.
fn caller_of(
    state: &DaemonState,
    session: SessionId,
    caller: ClaudeProcess,
    live: ClaudeProcess,
) -> crate::daemon::services::delegation_repair::RepairCaller {
    use crate::daemon::services::delegation_repair_caller::{
        establish_caller_with, owner_claude_with,
    };
    establish_caller_with(
        kernel_peer(),
        &[session],
        |_, _| Ok(caller),
        |s| {
            owner_claude_with(state, s, |pid| {
                if pid == live.pid {
                    Ok(ProcessFacts {
                        parent: None,
                        start_time: live.start_time,
                    })
                } else {
                    Err(format!("no entry for {pid}"))
                }
            })
        },
    )
}

/// #8983: the `claude` the daemon resumed into its own pane rebinds the
/// settled id when it announces it, and is then granted its own records.
#[tokio::test]
async fn a_daemon_resumed_claude_rebinds_its_session_8983() {
    use crate::daemon::services::delegation_repair::RepairCaller;
    use crate::daemon::services::delegation_repair_caller::bind_announcing_claude_with;
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let session = resumed_session(&state);
    let resumed = resumed_claude();

    let got = bind_announcing_claude_with(
        &state,
        session,
        kernel_peer(),
        move |_, _, _| Ok(resumed),
        move |grant| {
            assert_eq!(
                (grant.tmux_name.as_str(), grant.pane_id.as_deref()),
                ("tm-resumed", Some("%7"))
            );
            Ok(resumed)
        },
    )
    .await;

    assert_eq!(got, Ok(()));
    assert_eq!(state.session_claudes().get(session), Some(resumed));
    assert_eq!(state.session_claudes().resume_grant(session), None, "used");
    assert_eq!(
        caller_of(&state, session, resumed, resumed),
        RepairCaller::Session(session)
    );
    let restarted = daemon_at(root.path());
    assert_eq!(restarted.session_claudes().get(session), Some(resumed));
}

/// #8983: a sibling's `claude` announcing a resumed id — it is not the
/// `claude` in the daemon's pane — rebinds nothing and is not the owner; an
/// id the daemon is not resuming takes no announcement at all.
#[tokio::test]
async fn a_sibling_claude_announcing_a_resumed_id_is_not_bound_8983() {
    use crate::daemon::services::delegation_repair::RepairCaller;
    use crate::daemon::services::delegation_repair_caller::bind_announcing_claude_with;
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let session = resumed_session(&state);
    let resumed = resumed_claude();
    let sibling = ClaudeProcess {
        pid: 500,
        start_time: resumed.start_time + 1,
    };

    let got = bind_announcing_claude_with(
        &state,
        session,
        kernel_peer(),
        move |_, _, _| Ok(sibling),
        move |_| Ok(resumed),
    )
    .await;

    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("is not the claude the daemon resumed")),
        "{got:?}"
    );
    assert_eq!(state.session_claudes().get(session), Some(CLAUDE));
    assert!(
        state.session_claudes().resume_grant(session).is_some(),
        "kept"
    );
    assert!(matches!(
        caller_of(&state, session, sibling, sibling),
        RepairCaller::Unestablished(_)
    ));

    let not_resumed = SessionId::new();
    state
        .bind_session_claude(not_resumed, CLAUDE)
        .expect("vacant");
    // #9010: the walked claude is only noted as a later announcer.
    let got = bind_announcing_claude_with(
        &state,
        not_resumed,
        kernel_peer(),
        move |_, _, _| Ok(sibling),
        |_| panic!("no pane lookup without a grant"),
    )
    .await;
    assert_eq!(got, Ok(()));
    assert_eq!(state.session_claudes().get(not_resumed), Some(CLAUDE));
    let noted = state
        .session_claudes()
        .announcers
        .lock()
        .get(&not_resumed)
        .copied();
    assert_eq!(noted, Some(sibling));
    assert!(
        matches!(
            caller_of(&state, not_resumed, sibling, sibling),
            RepairCaller::Unestablished(_)
        ),
        "a noted announcer is never the owner"
    );
}

/// #9010: a process walk that fails for an already-settled id notes no later
/// announcer and is logged at WARN with the session id, not dropped.
#[tokio::test]
async fn a_failed_walk_on_a_settled_id_notes_no_announcer_9010() {
    use crate::daemon::services::delegation_repair_caller::bind_announcing_claude_with;
    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    crate::test_support::enable_event_capture();
    let root = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(daemon_at(root.path()));
    let session = SessionId::new();
    state.bind_session_claude(session, CLAUDE).expect("vacant");
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );

    let got = bind_announcing_claude_with(
        &state,
        session,
        kernel_peer(),
        |_, _, _| Err("the peer exited".to_string()),
        |_| panic!("no pane lookup without a grant"),
    )
    .with_subscriber(subscriber)
    .await;

    assert_eq!(got, Ok(()));
    assert!(state.session_claudes().announcers.lock().is_empty());
    assert_eq!(state.session_claudes().get(session), Some(CLAUDE));
    let lines = buffer.tail(64);
    assert!(
        lines.iter().any(|l| l.contains("WARN")
            && l.contains(&format!("{session:?}"))
            && l.contains("the peer exited")),
        "the failed walk is logged at WARN with the session id: {lines:#?}"
    );
}

/// #8983 Fail-Open Check: no grant, a `claude` older than the grant, a pane
/// lookup that fails, and a sealed registry each rebind nothing.
#[test]
fn a_resume_rebind_fails_closed_8983() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = daemon_at(root.path());
    let resumed = resumed_claude();
    let refused = |session, claude, pane: Result<ClaudeProcess, String>, why: &str| {
        let got = state.rebind_resumed_claude_with(session, claude, |_| pane.clone());
        assert!(
            got.as_ref().is_err_and(|e| e.contains(why)),
            "{why}: {got:?}"
        );
        assert_eq!(state.session_claudes().get(session), Some(CLAUDE), "{why}");
    };
    let ungranted = SessionId::new();
    state
        .bind_session_claude(ungranted, CLAUDE)
        .expect("vacant");
    refused(ungranted, resumed, Ok(resumed), "has not resumed");
    let session = resumed_session(&state);
    refused(session, CLAUDE, Ok(CLAUDE), "started before");
    refused(
        session,
        resumed,
        Err("tmux is gone".into()),
        "could not be found",
    );
    // #8983: a record with no pane id never falls back to the active pane.
    let paneless = SessionId::new();
    state.bind_session_claude(paneless, CLAUDE).expect("vacant");
    state.grant_resumed_session(Some(&paneless.0.to_string()), "tm-resumed", None);
    let got = state.rebind_resumed_claude_with(
        paneless,
        resumed,
        crate::daemon::state::session_claude_resume::pane_claude,
    );
    assert!(
        got.as_ref().is_err_and(|e| e.contains("has no pane id")),
        "{got:?}"
    );
    assert_eq!(state.session_claudes().get(paneless), Some(CLAUDE));
    assert!(state.session_claudes().resume_grant(paneless).is_some());

    let sealed_root = tempfile::tempdir().expect("tempdir");
    let file = registry_file(&daemon_at(sealed_root.path()));
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, b"{").expect("corrupt it");
    let sealed = daemon_at(sealed_root.path());
    let id = SessionId::new();
    sealed.grant_resumed_session(Some(&id.0.to_string()), "tm-resumed", None);
    let got = sealed.rebind_resumed_claude_with(id, resumed, |_| Ok(resumed));
    assert!(got.is_err_and(|e| e.contains("sealed")));
    assert_eq!(sealed.session_claudes().get(id), None);
}

/// #8983 Fail-Open Check: a rebinding that cannot be saved leaves the
/// earlier binding, so no unsaved binding grants.
#[test]
fn an_unsaved_rebind_keeps_the_earlier_binding_8983() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sub = dir.path().join("sub");
    let claudes = SessionClaudes::load_as(sub.join(SESSION_CLAUDES_FILE), current_uid());
    let session = SessionId::new();
    claudes
        .record(session, Announcement::Claude(CLAUDE), || Ok(()))
        .expect("saved");
    std::fs::remove_dir_all(&sub).expect("rmdir");
    std::fs::write(&sub, b"").expect("a file where the directory goes");
    let got = claudes.rebind(session, resumed_claude());
    assert!(got.is_err_and(|e| e.contains("could not be saved")));
    assert_eq!(claudes.get(session), Some(CLAUDE));
}

/// #8983: a resume with no stored id, or a malformed one, launches a fresh
/// session, so it grants nothing.
#[test]
fn a_malformed_resume_id_grants_nothing_8983() {
    let state = DaemonState::new();
    state.grant_resumed_session(None, "tm-resumed", None);
    state.grant_resumed_session(Some("not-a-uuid"), "tm-resumed", None);
    assert!(state.session_claudes().resumes.lock().is_empty());
}
