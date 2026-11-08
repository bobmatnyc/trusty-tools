//! #10101: the runtime-exit reaper keys panes by (server, session). A fake
//! tmux answers the default server and one workspace server; no real tmux
//! runs.

use std::os::unix::net::UnixListener;
use std::sync::Arc;

use super::*;
use crate::daemon::orphan_gc::ChildLivenessProbe;
use crate::session_manager::FakeNoopTmuxDriver;

/// The workspace server the record lives on.
const NAMED: &str = "tm-ws-10101-reap";

/// A probe that reports no live child for every pane.
struct NoChild;

impl ChildLivenessProbe for NoChild {
    fn has_live_child(&self, _pane_pid: Option<u32>) -> bool {
        false
    }
}

/// A scratch socket dir holding a live [`NAMED`] server, and a fake tmux
/// whose default server shows `default_cmd` and whose [`NAMED`] server shows
/// `named_cmd`, each as the one pane of session `name`.
fn fixture(
    name: &str,
    default_cmd: &str,
    named_cmd: &str,
) -> (tempfile::TempDir, UnixListener, TmuxDriver) {
    let dir = crate::test_support::hermetic_temp_dir();
    let live = UnixListener::bind(dir.path().join(NAMED)).expect("bind");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = -L ] && [ \"$2\" = {NAMED} ] && [ \"$3\" = list-panes ]; then \
         printf '{name}\\t{named_cmd}\\t99992\\t%%2\\t/tmp\\n'; exit 0; fi\n\
         if [ \"$1\" = list-panes ]; then printf '{name}\\t{default_cmd}\\t99991\\t%%1\\t/tmp\\n'; fi\n\
         exit 0\n"
    );
    let bin = dir.path().join("fake-tmux");
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    (dir, live, driver)
}

/// A manager holding one `Active` record, session `name`, on the [`NAMED`]
/// server in `dir`.
async fn named_record(dir: &std::path::Path, name: &str) -> (Arc<SessionManager>, SessionRecord) {
    let mgr = Arc::new(
        SessionManager::new(dir, Arc::new(FakeNoopTmuxDriver))
            .await
            .expect("session manager"),
    );
    let record = mgr
        .create(
            "reap".into(),
            Some(dir.to_path_buf()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    mgr.set_workspace(&record.id, dir.to_path_buf(), ManagedSessionState::Active)
        .await
        .expect("set Active");
    let mut record = mgr.get(&record.id).await.expect("record");
    record.tmux_name = name.to_owned();
    record.tmux_socket = Some(dir.join(NAMED));
    mgr.store
        .write()
        .await
        .upsert(record.clone())
        .await
        .expect("seed socket");
    (mgr, record)
}

/// Error arm (Fail-Open Check): a PM on a workspace server whose `claude`
/// exited (its pane is a bare shell there) is marked `Stopped`, while the
/// default server shows a live pane of the same name. Red on 2df3760494,
/// whose reaper listed only the default server and left the record `Active`.
#[serial_test::serial]
#[tokio::test]
async fn a_named_pm_whose_claude_exited_is_stopped_on_its_own_server() {
    let (sockets, _live, driver) = fixture("tm-reap-10101", "claude", "zsh");
    let (mgr, record) = named_record(sockets.path(), "tm-reap-10101").await;

    let stopped = reap_runtime_exited_managed_in(&mgr, &driver, sockets.path(), &NoChild).await;

    let after = mgr.get(&record.id).await.expect("record");
    assert_eq!(stopped, 1);
    assert_eq!(after.state, ManagedSessionState::Stopped);
}

/// Error arm (Fail-Open Check): a bare-shell pane on the default server that
/// carries a workspace-server record's name never stops that record while
/// its PM runs on its own server, nor when its server's panes are unknown.
/// Red on 2df3760494, which matched the record to the default server's pane
/// by name and marked it `Stopped`.
#[serial_test::serial]
#[tokio::test]
async fn a_default_pane_with_a_named_records_name_never_stops_it() {
    let (sockets, _live, driver) = fixture("tm-reap-10101", "zsh", "claude");
    let (mgr, record) = named_record(sockets.path(), "tm-reap-10101").await;

    let stopped = reap_runtime_exited_managed_in(&mgr, &driver, sockets.path(), &NoChild).await;
    assert_eq!(stopped, 0, "the default server's pane stopped a running PM");

    // Fail closed: the record's server is unknown, so nothing is stopped.
    let bare = PaneInfo {
        session_name: record.tmux_name.clone(),
        pane_current_command: "zsh".to_owned(),
        pane_pid: Some(99991),
        pane_id: Some("%1".to_owned()),
        pane_current_path: None,
    };
    let unknown = ServerPanes {
        default: vec![bare],
        unknown: [NAMED.to_owned()].into_iter().collect(),
        socket_dir: Some(sockets.path().to_path_buf()),
        ..ServerPanes::default()
    };
    assert!(find_runtime_exited_on(std::slice::from_ref(&record), &unknown, &NoChild).is_empty());
    let after = mgr.get(&record.id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Active);
}
