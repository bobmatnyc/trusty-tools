//! #10101: the tmux-gone reaper judges each record on its own tmux server.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::DaemonState;
use crate::session_manager::tmux_route::LiveSessions;
use crate::session_manager::{
    FakeNoopTmuxDriver, ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver,
    SessionManager, StopCause,
};

/// [`FakeNoopTmuxDriver`] that also reaches every named server, on the
/// socket `/tmp/tmux-0/<server>` the records below store.
///
/// Why: #10101 — the reaper holds a record whose server the rename follow
/// could not read, so a driver that reaches no named server would hold every
/// workspace-server record and the stop arms below would never run.
/// What: `None` is the default server. A named server lists no session when
/// its socket exists, and otherwise fails as tmux does for an exited server
/// (`error connecting to <socket> (No such file or directory)`).
struct RoutingNoop(Option<PathBuf>);

/// Where [`RoutingNoop`]'s named servers' sockets live.
const SOCKET_DIR: &str = "/tmp/tmux-0";

impl ManagedTmuxDriver for RoutingNoop {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        FakeNoopTmuxDriver.create_session(name, workdir)
    }
    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        FakeNoopTmuxDriver.kill_session(name)
    }
    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        FakeNoopTmuxDriver.send_line(name, text)
    }
    fn send_keys_literal(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        FakeNoopTmuxDriver.send_keys_literal(name, text)
    }
    fn send_interrupt(&self, name: &str) -> Result<(), ManagedError> {
        FakeNoopTmuxDriver.send_interrupt(name)
    }
    fn capture(&self, name: &str, lines: usize) -> Result<String, ManagedError> {
        FakeNoopTmuxDriver.capture(name, lines)
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        match &self.0 {
            Some(socket) if socket.symlink_metadata().is_err() => {
                Err(ManagedError::TmuxUnavailable(format!(
                    "error connecting to {} (No such file or directory)",
                    socket.display()
                )))
            }
            _ => FakeNoopTmuxDriver.list_sessions(),
        }
    }
    fn get_pane_id(&self, name: &str) -> Option<String> {
        FakeNoopTmuxDriver.get_pane_id(name)
    }
    fn pane_identity(
        &self,
        pane_id: &str,
    ) -> Result<crate::session_manager::pane_identity::PaneIdentity, ManagedError> {
        FakeNoopTmuxDriver.pane_identity(pane_id)
    }
    fn send_keys_literal_to_pane(&self, n: &str, p: &str, t: &str) -> Result<(), ManagedError> {
        FakeNoopTmuxDriver.send_keys_literal_to_pane(n, p, t)
    }
    fn send_interrupt_to_pane(&self, name: &str, pane_id: &str) -> Result<(), ManagedError> {
        FakeNoopTmuxDriver.send_interrupt_to_pane(name, pane_id)
    }
    fn capture_pane(&self, n: &str, p: &str, lines: usize) -> Result<String, ManagedError> {
        FakeNoopTmuxDriver.capture_pane(n, p, lines)
    }
    fn pane_harness(&self, n: &str, p: &str, h: &str) -> crate::core::process::PaneClaude {
        FakeNoopTmuxDriver.pane_harness(n, p, h)
    }
    fn on_server(&self, server: &str) -> Option<Arc<dyn ManagedTmuxDriver>> {
        Some(Arc::new(RoutingNoop(Some(
            Path::new(SOCKET_DIR).join(server),
        ))))
    }
}

/// One Active record per `(task, socket)`, wired into a state.
async fn seeded(
    sockets: &[(&str, Option<&str>)],
) -> (
    tempfile::TempDir,
    Arc<SessionManager>,
    DaemonState,
    Vec<ManagedSessionId>,
) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mgr = Arc::new(
        SessionManager::new(tmp.path(), Arc::new(RoutingNoop(None)))
            .await
            .expect("session manager"),
    );
    let mut ids = Vec::new();
    for (task, socket) in sockets {
        let workspace = PathBuf::from(format!("/tmp/test-reap-10101-{task}"));
        let record = mgr
            .create(
                (*task).into(),
                Some(workspace.clone()),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        mgr.set_workspace(&record.id, workspace, ManagedSessionState::Active)
            .await
            .expect("set Active");
        let mut record = mgr.get(&record.id).await.expect("record");
        record.tmux_socket = socket.map(PathBuf::from);
        mgr.store
            .write()
            .await
            .upsert(record.clone())
            .await
            .expect("seed socket");
        ids.push(record.id);
    }
    let state = DaemonState::with_session_manager(Arc::clone(&mgr));
    (tmp, mgr, state, ids)
}

/// Error arm (Fail-Open Check): a PM live on its workspace server is absent
/// from the default server's list, and the reaper leaves it Active. Red on
/// 420b3717bf, whose reaper checked only the default server's names.
#[tokio::test]
async fn the_reaper_never_stops_a_live_pm_on_a_workspace_server() {
    let (_tmp, mgr, state, ids) = seeded(&[
        ("live", Some("/tmp/tmux-0/tm-ws-a")),
        ("killed", Some("/tmp/tmux-0/tm-ws-a")),
        ("lost", Some("/tmp/tmux-0/tm-ws-gone")),
    ])
    .await;
    let live_name = mgr.get(&ids[0]).await.expect("live").tmux_name;
    let live = LiveSessions {
        default: HashSet::from(["tm-someone-else".to_owned()]),
        named: HashMap::from([("tm-ws-a".to_owned(), HashSet::from([live_name]))]),
        ..LiveSessions::default()
    };

    state.reap_managed_against(&live).await;

    let alive = mgr.get(&ids[0]).await.expect("live");
    assert_eq!(alive.state, ManagedSessionState::Active);
    assert_eq!(alive.stop_cause, None);
    // Its server is up and lists other sessions: a targeted kill.
    let killed = mgr.get(&ids[1]).await.expect("killed");
    assert_eq!(killed.state, ManagedSessionState::Stopped);
    assert_eq!(killed.stop_cause, Some(StopCause::Deliberate));
    // Its server is gone with its socket: a clean exit or a kill of its last
    // session, so a decision, never auto-resumed (#10101 ruling, arm a).
    let lost = mgr.get(&ids[2]).await.expect("lost");
    assert_eq!(lost.state, ManagedSessionState::Stopped);
    assert_eq!(lost.stop_cause, Some(StopCause::Deliberate));
}

/// Error arm (Fail-Open Check): a workspace server that could not be listed
/// leaves its records Active, and the default server's records still reap.
#[tokio::test]
async fn an_unlistable_workspace_server_leaves_its_records_alone() {
    let (_tmp, mgr, state, ids) =
        seeded(&[("stuck", Some("/tmp/tmux-0/tm-ws-stuck")), ("gone", None)]).await;
    let live = LiveSessions {
        default: HashSet::from(["tm-someone-else".to_owned()]),
        unknown: HashSet::from(["tm-ws-stuck".to_owned()]),
        ..LiveSessions::default()
    };

    state.reap_managed_against(&live).await;

    assert_eq!(
        mgr.get(&ids[0]).await.expect("stuck").state,
        ManagedSessionState::Active
    );
    assert_eq!(
        mgr.get(&ids[1]).await.expect("gone").state,
        ManagedSessionState::Stopped
    );
}

/// #10101 ruling arm (a): a workspace server that exited removed its socket,
/// so the rename follow's listing fails. That is a gone server, not an
/// unreadable one: the reaper still stops its PM as Deliberate.
#[tokio::test]
async fn a_pm_whose_server_socket_is_gone_is_still_stopped_deliberate() {
    let socket = Path::new(SOCKET_DIR).join("tm-ws-ended-10101");
    assert!(
        socket.symlink_metadata().is_err(),
        "the socket must be absent"
    );
    let (_tmp, mgr, state, ids) = seeded(&[("ended", socket.to_str())]).await;
    let live = LiveSessions {
        default: HashSet::from(["tm-someone-else".to_owned()]),
        ..LiveSessions::default()
    };

    state.reap_managed_against(&live).await;

    let ended = mgr.get(&ids[0]).await.expect("ended");
    assert_eq!(ended.state, ManagedSessionState::Stopped);
    assert_eq!(ended.stop_cause, Some(StopCause::Deliberate));
}

/// Seed three default-server records read on tmux server `1:1`, and the
/// live default set holding only the first one's name (#10700).
async fn three_on_server_one() -> (
    tempfile::TempDir,
    Arc<SessionManager>,
    DaemonState,
    Vec<ManagedSessionId>,
    HashSet<String>,
) {
    let (tmp, mgr, state, ids) =
        seeded(&[("first", None), ("second", None), ("third", None)]).await;
    for id in &ids {
        let mut record = mgr.get(id).await.expect("record");
        record.tmux_server = Some(crate::test_support::FAKE_PANE_SERVER.to_owned());
        mgr.store
            .write()
            .await
            .upsert(record)
            .await
            .expect("seed server");
    }
    let first = mgr.get(&ids[0]).await.expect("first").tmux_name;
    (tmp, mgr, state, ids, HashSet::from([first]))
}

/// #10700: the first resume after a tmux server loss starts a NEW default
/// server. The sessions still missing were lost with the old one, so they
/// stay auto-resumable. Red on a200d64eed, which stamped them Deliberate
/// because the recreated server listed a session.
#[tokio::test]
async fn reap_keeps_sessions_lost_to_a_restarted_default_server_auto_resumable() {
    let (_tmp, mgr, state, ids, default) = three_on_server_one().await;
    let live = LiveSessions {
        default,
        default_server: Some("2:2".to_owned()),
        ..LiveSessions::default()
    };

    state.reap_managed_against(&live).await;

    assert_eq!(
        mgr.get(&ids[0]).await.expect("first").state,
        ManagedSessionState::Active
    );
    for id in &ids[1..] {
        let after = mgr.get(id).await.expect("after");
        assert_eq!(after.state, ManagedSessionState::Stopped);
        assert_eq!(after.stop_cause, Some(StopCause::Unexpected));
        assert!(
            after.is_auto_resumable(),
            "a session lost with the old tmux server must stay auto-resumable"
        );
    }
}

/// #10700 counterpart: the default server stayed up (its live identity, read
/// through `default_server_identity`, matches the records'), so a missing
/// session was killed on purpose and is stamped Deliberate.
#[tokio::test]
async fn reap_marks_a_targeted_kill_on_the_same_default_server_deliberate() {
    let (_tmp, mgr, state, ids, default) = three_on_server_one().await;
    let identity =
        crate::session_manager::tmux_route::default_server_identity(&RoutingNoop(None), &default);
    assert_eq!(
        identity.as_deref(),
        Some(crate::test_support::FAKE_PANE_SERVER)
    );
    let live = LiveSessions {
        default,
        default_server: identity,
        ..LiveSessions::default()
    };

    state.reap_managed_against(&live).await;

    for id in &ids[1..] {
        let after = mgr.get(id).await.expect("after");
        assert_eq!(after.state, ManagedSessionState::Stopped);
        assert_eq!(after.stop_cause, Some(StopCause::Deliberate));
        assert!(!after.is_auto_resumable());
    }
}
