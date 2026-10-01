//! Coverage for the kernel-bound session `claude` registry (#8531).

use super::*;
use crate::core::agent::Delegation;

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
