//! Coverage for establishing a repair caller from the kernel's peer (#8531).
//!
//! The walk and the owner lookup are injected, so every refusal arm runs
//! without a live `claude`; `owner_claude_pid`'s native arm runs against a
//! real child process.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::*;
use crate::core::session::{ControlModel, Session, SessionHost};

const OWNER_CLAUDE: u32 = 4100;
const SIBLING_CLAUDE: u32 = 4200;

fn claude(pid: u32) -> ClaudeProcess {
    ClaudeProcess { pid, start_time: 1 }
}

/// An owner lookup that names `OWNER_CLAUDE` for `owner` and fails for any
/// other session.
fn owner_lookup(owner: SessionId) -> impl Fn(SessionId) -> Result<u32, String> {
    move |s| {
        if s == owner {
            Ok(OWNER_CLAUDE)
        } else {
            Err("no record".to_string())
        }
    }
}

fn unestablished(caller: &RepairCaller) -> &str {
    match caller {
        RepairCaller::Unestablished(why) => why,
        RepairCaller::Session(s) => panic!("expected no caller, got session {}", s.0),
    }
}

/// #8531: a process under the owner's own `claude` is the owner.
#[test]
fn the_owner_process_establishes_the_owner_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        RepairPeer::Kernel(9001),
        &[SessionId::new(), owner],
        |_| Ok(Some(claude(OWNER_CLAUDE))),
        owner_lookup(owner),
    );
    assert_eq!(caller, RepairCaller::Session(owner));
}

/// #8531, the issue's acceptance case: a process under ANOTHER session's
/// `claude` is refused, whatever id it could have named.
#[test]
fn a_sibling_session_process_is_not_the_owner_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        RepairPeer::Kernel(9002),
        &[owner],
        |_| Ok(Some(claude(SIBLING_CLAUDE))),
        owner_lookup(owner),
    );
    let why = unestablished(&caller);
    assert!(why.contains(&SIBLING_CLAUDE.to_string()), "{why}");
    assert!(
        !why.contains(&owner.0.to_string()),
        "names no owner id: {why}"
    );
}

/// #8531 Fail-Open Check: no kernel pid (HTTP, or a socket that could not
/// read one) establishes nobody, and nothing else is consulted.
#[test]
fn an_unproven_peer_is_unestablished_8531() {
    let owner = SessionId::new();
    for peer in [RepairPeer::http(), RepairPeer::from_socket(None)] {
        let caller = establish_caller_with(
            peer,
            &[owner],
            |_| panic!("no walk without a pid"),
            owner_lookup(owner),
        );
        unestablished(&caller);
    }
    assert_eq!(RepairPeer::from_socket(Some(7)), RepairPeer::Kernel(7));
}

/// #8531 Fail-Open Check: an ancestry that cannot be read refuses — it is
/// never read as "under the owner".
#[test]
fn an_unreadable_ancestry_is_unestablished_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        RepairPeer::Kernel(9003),
        &[owner],
        |_| Err("ps failed".to_string()),
        owner_lookup(owner),
    );
    assert!(
        unestablished(&caller).contains("could not be read: ps failed"),
        "{caller:?}"
    );
}

/// #8531: a caller with no `claude` above it runs in no session at all.
#[test]
fn a_caller_with_no_claude_above_it_is_unestablished_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        RepairPeer::Kernel(9004),
        &[owner],
        |_| Ok(None),
        owner_lookup(owner),
    );
    assert!(
        unestablished(&caller).contains("no claude session process"),
        "{caller:?}"
    );
}

/// #8531 Fail-Open Check: an owner whose process cannot be found refuses,
/// and the reason says so rather than reading the lookup as "no match".
#[test]
fn an_owner_process_that_cannot_be_found_is_unestablished_8531() {
    let owner = SessionId::new();
    let caller = establish_caller_with(
        RepairPeer::Kernel(9005),
        &[owner],
        |_| Ok(Some(claude(OWNER_CLAUDE))),
        |_| Err("no claude process could be found in the pane".to_string()),
    );
    assert!(
        unestablished(&caller).contains("no claude process could be found in the pane"),
        "{caller:?}"
    );
}

fn native_owner(
    state: &DaemonState,
    pid: u32,
    created_at: SystemTime,
    status: SessionStatus,
) -> SessionId {
    let id = SessionId::new();
    let mut s = Session::new(id, "/repo", ControlModel::Tmux, None);
    s.origin = SessionHost::Native;
    s.pid = Some(pid);
    s.created_at = created_at;
    s.status = status;
    state.register_session(s);
    id
}

/// #8531: a native session's recorded pid is its process while that process
/// predates the record.
#[test]
fn a_native_owner_pid_that_predates_its_record_is_its_process_8531() {
    let state = DaemonState::new();
    let pid = std::process::id();
    let owner = native_owner(
        &state,
        pid,
        SystemTime::now() + Duration::from_secs(5),
        SessionStatus::Active,
    );
    assert_eq!(
        owner_claude_pid(&state, owner, crate::core::twin_arming::process_facts),
        Ok(pid)
    );
}

/// #8531 Fail-Open Check: a recorded pid now naming a process that started
/// after the record — a reused pid — is not the owner's process.
#[test]
fn a_native_owner_pid_started_after_its_record_is_refused_8531() {
    let state = DaemonState::new();
    let owner = native_owner(
        &state,
        4242,
        SystemTime::UNIX_EPOCH + Duration::from_secs(10),
        SessionStatus::Active,
    );
    let facts = |_| {
        Ok(ProcessFacts {
            parent: Some(1),
            start_time: 20,
        })
    };
    let got = owner_claude_pid(&state, owner, facts);
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("started after the owning session registered")),
        "{got:?}"
    );
    let gone = owner_claude_pid(&state, owner, |_| Err("no entry".to_string()));
    assert_eq!(gone, Err("no entry".to_string()));
}

/// #8531 Fail-Open Check: no record, or a stopped one, has no process.
#[test]
fn a_missing_or_stopped_owner_record_is_an_error_8531() {
    let state = DaemonState::new();
    let facts = |_| panic!("no process lookup without a live record");
    assert!(owner_claude_pid(&state, SessionId::new(), facts).is_err());
    let stopped = native_owner(
        &state,
        std::process::id(),
        SystemTime::now(),
        SessionStatus::Stopped,
    );
    assert!(owner_claude_pid(&state, stopped, facts).is_err());
}

/// #8531 end to end over the real socket: this test process asserts the
/// owner's id in the old `caller_session` param and is refused — it runs
/// under no `claude` the owner's record names.
#[tokio::test]
async fn a_socket_caller_without_a_claude_owner_process_is_refused_8531() {
    use crate::core::agent::DelegationStatus;

    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(DaemonState::new());
    let owner = SessionId::new();
    // A tmux session no server hosts, so its `claude` cannot be found.
    let mut s = Session::new(owner, "/repo", ControlModel::Tmux, None);
    s.tmux_name = format!("tm-8531-absent-{}", owner.0.simple());
    state.register_session(s);
    let mut d = Delegation::observed(owner, "version-control", "task", Some("toolu-8531".into()));
    d.agent_id = Some("a8531".to_string());
    state.upsert_delegation(d);

    let socket = dir.path().join("mpm.sock");
    let bound = crate::daemon::socket::bind(&socket).await.expect("bind");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(crate::daemon::socket::serve_until_shutdown(
        bound,
        Arc::clone(&state),
        async {
            let _ = stopped.await;
        },
    ));
    let client = crate::client::DaemonClient::over_socket(&socket);
    let mut answer = None;
    for _ in 0..200 {
        let sent = client
            .post("/api/v1/delegations/a8531/repair")
            .json(&serde_json::json!({ "force": true, "caller_session": owner.0.to_string() }))
            .send()
            .await;
        if let Ok(resp) = sent {
            answer = Some(resp.json::<serde_json::Value>().await.expect("json"));
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = stop.send(());
    let _ = server.await;

    let answer = answer.expect("the socket answered");
    assert_eq!(answer["outcome"], serde_json::json!("refused"), "{answer}");
    assert_eq!(
        state.all_delegations()[0].status,
        DelegationStatus::Running,
        "a refused repair writes nothing"
    );
}
