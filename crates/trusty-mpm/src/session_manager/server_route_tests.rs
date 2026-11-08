//! #10101: a record on a named (`-L`) tmux server is reached on that server
//! only. A same-named session on the default server never stands in for it.
//!
//! Reuses the parent's scripted fake tmux: `only_server: None` answers the
//! default server alone, `Some(name)` answers `-L <name>` alone.

use super::*;

/// The workspace server every record here lives on.
const NAMED: &str = "tm-ws-10101-route";

/// Point `id`'s record at a [`NAMED`] socket inside the fixture's scratch
/// dir, so no liveness probe reaches a real socket directory.
async fn put_on_named_server(f: &Fixture, id: &ManagedSessionId) {
    let mut record = f.mgr.get(id).await.expect("record");
    record.tmux_socket = Some(f.dir.path().join(NAMED));
    f.mgr
        .store
        .write()
        .await
        .upsert(record)
        .await
        .expect("seed");
}

/// The calls that reached the default server: every line without `-L`.
fn default_server_calls(calls: &str) -> Vec<&str> {
    calls.lines().filter(|l| !l.starts_with("-L ")).collect()
}

/// Error arm (Fail-Open Check): the default server holds a session with the
/// record's name, pane and server identity, but the record lives on another
/// server. The stop signals and kills nothing on the default server.
/// Red on ad5cf86741, which proved ownership on the default server.
#[serial_test::serial]
#[tokio::test]
async fn stopping_a_named_server_record_never_tears_down_the_default_servers_session() {
    let f = Fixture::new(FakeTmux::default()).await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    put_on_named_server(&f, &id).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("record-only stop");

    let calls = f.calls();
    assert_eq!(report.runtime, RuntimeTeardown::Absent, "{calls}");
    let stray: Vec<&str> = default_server_calls(&calls)
        .into_iter()
        .filter(|l| l.starts_with("send-keys") || l.starts_with("kill-session"))
        .collect();
    assert!(
        stray.is_empty(),
        "the default server's session was touched: {calls}"
    );
    assert!(
        calls.contains(&format!("-L {NAMED} list-sessions")),
        "the probe went to the record's server: {calls}"
    );
}

/// Error arm (Fail-Open Check): a pane read for a named-server record never
/// falls back to the default server's same-named pane. Red on ad5cf86741,
/// which captured the default server's pane.
#[serial_test::serial]
#[tokio::test]
async fn observing_a_named_server_record_never_reads_the_default_servers_pane() {
    let f = Fixture::new(FakeTmux::default()).await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    put_on_named_server(&f, &id).await;

    let observed = f.scoped(f.mgr.observe(&id, 20)).await;

    let calls = f.calls();
    assert!(observed.is_err(), "observe read a pane: {observed:?}");
    assert!(
        default_server_calls(&calls)
            .iter()
            .all(|l| !l.starts_with("capture-pane") && !l.starts_with("list-panes")),
        "the default server was read: {calls}"
    );
}

/// Error arm (Fail-Open Check): the boot reconcile does not count a
/// same-named session on the default server as the named-server record's
/// runtime. Red on ad5cf86741, which kept the record Active.
#[serial_test::serial]
#[tokio::test]
async fn a_same_named_default_server_session_never_keeps_a_named_server_record_live() {
    let f = Fixture::new(FakeTmux::default()).await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    put_on_named_server(&f, &id).await;

    f.scoped(f.mgr.reconcile_on_boot(false))
        .await
        .expect("reconcile");

    let after = f.mgr.get(&id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Stopped, "{}", f.calls());
}

/// The routing keeps its job: a record whose pane is live on its own named
/// server is signalled and reclaimed there, and only there.
#[serial_test::serial]
#[tokio::test]
async fn stopping_a_named_server_record_reclaims_it_on_its_own_server() {
    let f = Fixture::new(FakeTmux {
        only_server: Some(NAMED),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    put_on_named_server(&f, &id).await;

    let report = f
        .scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");

    let calls = f.calls();
    assert_eq!(report.runtime, RuntimeTeardown::Terminated, "{calls}");
    assert!(
        calls
            .lines()
            .any(|l| l == format!("-L {NAMED} kill-session -t $3")),
        "the session was reclaimed by id on its server: {calls}"
    );
    assert!(
        default_server_calls(&calls)
            .iter()
            .all(|l| !l.starts_with("send-keys") && !l.starts_with("kill-session")),
        "the default server was signalled: {calls}"
    );
}

/// Error arm (Fail-Open Check): a named-server teardown reads the pane's pid
/// on the record's own server, so SIGTERM reaches its `claude`; the default
/// server's pid read is never used. Red on 420b3717bf, which skipped the pid
/// read for a named server and sent only the Ctrl-C fallback.
#[serial_test::serial]
#[tokio::test]
async fn a_named_server_teardown_signals_the_claude_read_through_its_server() {
    let f = Fixture::new(FakeTmux {
        only_server: Some(NAMED),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    put_on_named_server(&f, &id).await;

    f.scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");

    let calls = f.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with(&format!("-L {NAMED} display-message"))
                && l.contains("#{pane_pid}")),
        "the pane pid was not read on the record's server: {calls}"
    );
    assert!(
        default_server_calls(&calls)
            .iter()
            .all(|l| !l.contains("#{pane_pid}")),
        "the pid was read on the default server: {calls}"
    );
}
