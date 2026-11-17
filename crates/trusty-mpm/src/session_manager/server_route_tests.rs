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

/// The fixture for a record live on its own [`NAMED`] server only: the
/// default server answers nothing.
async fn on_named_server_only() -> (Fixture, ManagedSessionId) {
    let f = Fixture::new(FakeTmux {
        only_server: Some(NAMED),
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    put_on_named_server(&f, &id).await;
    (f, id)
}

/// Assert a call to `verb` reached [`NAMED`] and none reached the default
/// server.
fn assert_only_on_named(calls: &str, verb: &str) {
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with(&format!("-L {NAMED} {verb}"))),
        "no {verb} reached the record's server: {calls}"
    );
    assert!(
        default_server_calls(calls)
            .iter()
            .all(|l| !l.starts_with(verb)),
        "a {verb} reached the default server: {calls}"
    );
}

/// #10101 live check: `tm sessions output <managed id>` ran a bare
/// `capture-pane -t %0` against the default server, which on a live host
/// reads an unrelated pane. The capture runs on the record's own server.
#[serial_test::serial]
#[tokio::test]
async fn capturing_a_named_server_record_reads_its_own_server_10101() {
    let (f, id) = on_named_server_only().await;

    let captured = f.scoped(f.mgr.capture_pane(&id, 50)).await;

    assert!(captured.is_ok(), "{captured:?}: {}", f.calls());
    assert_only_on_named(&f.calls(), "capture-pane");
}

/// #10101: an answer to a pending decision is typed on the server its pane
/// was proven on, never into the default server's same-numbered pane.
#[serial_test::serial]
#[tokio::test]
async fn answering_a_named_server_record_types_on_its_server_10101() {
    let (f, id) = on_named_server_only().await;
    f.mgr
        .set_pending_decision(&id, "merge or rebase?", Some("merge"))
        .await
        .expect("pending");

    let answered = f
        .scoped(
            f.mgr
                .answer_decision(&id, "merge", &crate::session_manager::AnswerClaim::by("t")),
        )
        .await;

    assert!(answered.is_ok(), "{answered:?}: {}", f.calls());
    assert_only_on_named(&f.calls(), "send-keys");
}

/// #10101: the stop snapshot reads the record's own pane on its own server,
/// so another server's `%9` is never written into this workspace.
#[serial_test::serial]
#[tokio::test]
async fn stopping_a_named_server_record_snapshots_its_pane_on_its_server_10101() {
    let (f, id) = on_named_server_only().await;
    let ws = f.dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("workspace");
    let mut record = f.mgr.get(&id).await.expect("record");
    record.workspace_path = Some(ws);
    f.mgr
        .store
        .write()
        .await
        .upsert(record)
        .await
        .expect("seed");

    f.scoped(f.mgr.stop_reporting(&id, StopCause::Deliberate))
        .await
        .expect("stop");

    assert_only_on_named(&f.calls(), "capture-pane");
}

/// #10101: prune's liveness gate asks the record's own server, so a live PM
/// on its workspace server never reads as stopped and prunable.
#[serial_test::serial]
#[tokio::test]
async fn a_named_server_record_runs_only_on_its_own_server_10101() {
    let (f, id) = on_named_server_only().await;
    let record = f.mgr.get(&id).await.expect("record");

    let running = f
        .scoped(async { crate::session_manager::prune::is_running(&record, f.mgr.tmux.as_ref()) })
        .await;

    let calls = f.calls();
    assert!(running.expect("probe"), "{calls}");
    assert!(
        calls.contains(&format!("-L {NAMED} list-sessions")),
        "{calls}"
    );
}

/// Error arm (Fail-Open Check, #10101): a record whose socket sits outside this
/// process's tmux socket directory cannot be routed, so `is_running` reads it
/// as running. A `stopped`-filter prune, previewed or real, selects nothing:
/// no record is tombstoned and no tmux teardown is issued.
#[serial_test::serial]
#[tokio::test]
async fn a_prune_leaves_an_unroutable_record_alone_10101() {
    let f = Fixture::new(FakeTmux::default()).await;
    let id = f.seed_on("stopped", Some("%9"), Some(LIVE_SERVER)).await;
    let mut record = f.mgr.get(&id).await.expect("record");
    // #10101: outside the fixture's socket dir, so `route` refuses it.
    record.tmux_socket = Some(f.dir.path().join("elsewhere").join(NAMED));
    f.mgr
        .store
        .write()
        .await
        .upsert(record)
        .await
        .expect("seed");

    // The preview runs no teardown, so only the liveness gate keeps the
    // record out of it.
    for dry_run in [true, false] {
        let outcome = f
            .scoped(f.mgr.prune_managed(
                crate::session_manager::PruneFilter::Stopped,
                dry_run,
                false,
                None,
                None,
            ))
            .await
            .expect("prune");
        assert!(
            outcome.sessions.is_empty(),
            "dry_run={dry_run}: an unroutable record was selected: {:?}: {}",
            outcome.sessions,
            f.calls()
        );
    }

    let calls = f.calls();
    let after = f.mgr.get(&id).await.expect("record kept");
    assert_eq!(after.state, ManagedSessionState::Stopped, "{calls}");
    assert!(
        !calls.contains("kill-session") && !calls.contains("send-keys"),
        "a teardown was issued: {calls}"
    );
}
