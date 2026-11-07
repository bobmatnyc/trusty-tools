//! #10101: the tmux-gone reaper judges each record on its own tmux server.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use super::DaemonState;
use crate::session_manager::tmux_route::LiveSessions;
use crate::session_manager::{
    FakeNoopTmuxDriver, ManagedSessionId, ManagedSessionState, SessionManager, StopCause,
};

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
        SessionManager::new(tmp.path(), Arc::new(FakeNoopTmuxDriver))
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
    // Its server is gone: a whole-server loss stays auto-resumable.
    let lost = mgr.get(&ids[2]).await.expect("lost");
    assert_eq!(lost.state, ManagedSessionState::Stopped);
    assert_eq!(lost.stop_cause, Some(StopCause::Unexpected));
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
