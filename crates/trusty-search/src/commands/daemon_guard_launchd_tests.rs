//! Tests for the launchd-aware daemon guard (#10413). None touches the real
//! launchd: the kickstart, the owner re-check and the spawn are injected, and
//! `run_bounded` runs a stub script.

use super::daemon_guard_launchd::*;
use super::mock_socket::serve_ok_at;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use trusty_common::launchd_claim::SocketOwner;
use trusty_search::service::daemon_client::DaemonClient;

const LABEL: &str = "com.trusty.search";
/// Short enough for a test, long enough for one 500 ms poll round.
const BUDGET: Duration = Duration::from_millis(900);

fn launchd() -> SocketOwner {
    SocketOwner::Launchd {
        label: LABEL.to_owned(),
    }
}

fn state(loaded: bool, pid: Option<u32>) -> LaunchdState {
    LaunchdState {
        owner: launchd(),
        loaded,
        pid,
    }
}

async fn no_kickstart() -> anyhow::Result<()> {
    panic!("kickstart must not run in this arm");
}

async fn no_recheck() -> LaunchdState {
    panic!("recheck must not run in this arm");
}

fn no_spawn() -> anyhow::Result<u32> {
    panic!("a launchd verdict must never spawn");
}

/// The truth table: nothing under a launchd verdict maps to `Spawn`.
#[test]
fn decide_truth_table() {
    let cases = [
        (SocketOwner::OnDemand, false, None, Plan::Spawn),
        (SocketOwner::OnDemand, true, Some(1), Plan::Spawn),
        (launchd(), true, None, Plan::Kickstart),
        (launchd(), true, Some(42), Plan::Wait),
        (launchd(), false, None, Plan::WaitUnloaded),
        (launchd(), false, Some(42), Plan::WaitUnloaded),
    ];
    for (owner, loaded, pid, want) in cases {
        assert_eq!(
            decide(&owner, loaded, pid),
            want,
            "{owner:?} {loaded} {pid:?}"
        );
    }
}

/// Every isolation input flips the gate; only the clean production case holds.
#[test]
fn production_socket_gate_cases() {
    let sock = Path::new("/h/.trusty-search/trusty-search.sock");
    let other = Path::new("/tmp/x/ts.sock");
    let some = Some(OsStr::new("/tmp/x"));
    let empty = Some(OsStr::new(""));
    let clean = IsolationEnv::default();
    assert!(is_production_socket(&clean, sock, Some(sock)));
    // Empty data-dir values count as unset.
    let env = IsolationEnv {
        search_data_dir: empty,
        data_dir: empty,
        ..clean
    };
    assert!(is_production_socket(&env, sock, Some(sock)));
    // Each isolating input denies.
    for env in [
        IsolationEnv {
            search_socket: some,
            ..clean
        },
        IsolationEnv {
            search_socket: empty,
            ..clean
        },
        IsolationEnv {
            search_data_dir: some,
            ..clean
        },
        IsolationEnv {
            data_dir: some,
            ..clean
        },
        IsolationEnv {
            data_dir_override: some,
            ..clean
        },
        IsolationEnv {
            data_dir_override: empty,
            ..clean
        },
    ] {
        assert!(!is_production_socket(&env, sock, Some(sock)), "{env:?}");
    }
    // A client socket that is not the canonical one, or no canonical at all.
    assert!(!is_production_socket(&clean, other, Some(sock)));
    assert!(!is_production_socket(&clean, sock, None));
}

#[test]
fn kickstart_argv_has_no_kill_flag() {
    let argv = kickstart_argv(501, LABEL);
    assert_eq!(argv, ["kickstart", "gui/501/com.trusty.search"]);
    assert!(!argv.iter().any(|a| a == "-k"));
}

fn stub(dir: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("stub-launchctl");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[tokio::test]
async fn run_bounded_logs_argv_and_succeeds() {
    let dir = tempfile::tempdir().expect("dir");
    let log = dir.path().join("argv.log");
    let prog = stub(dir.path(), &format!("echo \"$@\" > {}", log.display()));
    run_bounded(&prog, &kickstart_argv(7, LABEL), Duration::from_secs(5))
        .await
        .expect("exit 0");
    let logged = std::fs::read_to_string(log).expect("log");
    assert_eq!(logged.trim(), "kickstart gui/7/com.trusty.search");
}

#[tokio::test]
async fn run_bounded_reports_nonzero_exit() {
    let dir = tempfile::tempdir().expect("dir");
    let prog = stub(dir.path(), "echo nope >&2; exit 3");
    let err = run_bounded(&prog, &[], Duration::from_secs(5))
        .await
        .expect_err("exit 3");
    assert!(err.to_string().contains("nope"), "{err}");
}

#[tokio::test]
async fn run_bounded_times_out() {
    let dir = tempfile::tempdir().expect("dir");
    let prog = stub(dir.path(), "exec sleep 30");
    let started = std::time::Instant::now();
    let err = run_bounded(&prog, &[], Duration::from_millis(300))
        .await
        .expect_err("timeout");
    assert!(err.to_string().contains("did not finish"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

fn scratch() -> (tempfile::TempDir, DaemonClient, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("dir");
    let sock = dir.path().join("ts.sock");
    (dir, DaemonClient::at(&sock), sock)
}

/// Loaded, idle unit: kickstart runs, it binds the socket, the guard returns Ok
/// and the spawn is never reached.
#[tokio::test(flavor = "multi_thread")]
async fn ensure_with_kickstarts_an_idle_unit_and_never_spawns() {
    let (_dir, client, sock) = scratch();
    let stop = Arc::new(std::sync::Mutex::new(None));
    let slot = stop.clone();
    let kick = move || async move {
        *slot.lock().expect("slot") = Some(serve_ok_at(&sock));
        Ok(())
    };
    ensure_with(
        &client,
        BUDGET,
        state(true, None),
        kick,
        no_recheck,
        no_spawn,
    )
    .await
    .expect("the kickstarted unit serves");
    assert!(stop.lock().expect("slot").is_some(), "kickstart ran");
}

/// Loaded and running: wait only. Neither kickstart nor spawn run.
#[tokio::test(flavor = "multi_thread")]
async fn ensure_with_waits_for_a_running_unit() {
    let (_dir, client, sock) = scratch();
    let late = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        serve_ok_at(&sock)
    });
    ensure_with(
        &client,
        BUDGET,
        state(true, Some(9)),
        no_kickstart,
        no_recheck,
        no_spawn,
    )
    .await
    .expect("served after a short wait");
    drop(late.await.expect("task"));
}

/// Plist present, unit not loaded: wait the budget, error naming the unit, no
/// spawn, no kickstart.
#[tokio::test(flavor = "multi_thread")]
async fn ensure_with_errors_for_an_unloaded_unit_without_spawning() {
    let (_dir, client, _sock) = scratch();
    let err = ensure_with(
        &client,
        BUDGET,
        state(false, None),
        no_kickstart,
        no_recheck,
        no_spawn,
    )
    .await
    .expect_err("nothing serves");
    let text = err.to_string();
    assert!(
        text.contains(LABEL) && text.contains("launchctl print"),
        "{text}"
    );
}

/// A failed kickstart whose re-check says OnDemand now spawns (once).
#[tokio::test(flavor = "multi_thread")]
async fn ensure_with_spawns_when_kickstart_fails_and_unit_is_gone() {
    let (_dir, client, sock) = scratch();
    let spawned = Arc::new(AtomicU32::new(0));
    let count = spawned.clone();
    let spawn = move || {
        count.fetch_add(1, Ordering::SeqCst);
        std::mem::forget(serve_ok_at(&sock));
        Ok(1)
    };
    ensure_with(
        &client,
        BUDGET,
        state(true, None),
        || async { Err(anyhow::anyhow!("kickstart refused")) },
        || async { LaunchdState::on_demand() },
        spawn,
    )
    .await
    .expect("spawned daemon serves");
    assert_eq!(spawned.load(Ordering::SeqCst), 1);
}

/// A failed kickstart with the unit still registered waits out the budget and
/// errors; the spawn is unreachable.
#[tokio::test(flavor = "multi_thread")]
async fn ensure_with_never_spawns_when_kickstart_fails_and_unit_remains() {
    let (_dir, client, _sock) = scratch();
    let err = ensure_with(
        &client,
        BUDGET,
        state(true, None),
        || async { Err(anyhow::anyhow!("kickstart refused")) },
        || async { state(false, None) },
        no_spawn,
    )
    .await
    .expect_err("nothing serves");
    assert!(err.to_string().contains(LABEL), "{err}");
}

/// OnDemand: spawn as today; neither kickstart nor re-check run.
#[tokio::test(flavor = "multi_thread")]
async fn ensure_with_spawns_when_nothing_supervises_the_socket() {
    let (_dir, client, sock) = scratch();
    let spawned = Arc::new(AtomicU32::new(0));
    let count = spawned.clone();
    let spawn = move || {
        count.fetch_add(1, Ordering::SeqCst);
        std::mem::forget(serve_ok_at(&sock));
        Ok(1)
    };
    ensure_with(
        &client,
        BUDGET,
        LaunchdState::on_demand(),
        no_kickstart,
        no_recheck,
        spawn,
    )
    .await
    .expect("spawned daemon serves");
    assert_eq!(spawned.load(Ordering::SeqCst), 1);
}

fn plist_wait_state() -> LaunchdState {
    state(false, None)
}

/// #10413 round 1: a read that times out with a plist installed must wait, not
/// spawn. The reader sleeps past the bound; no real launchctl runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_timed_out_read_with_a_plist_is_not_loaded_launchd() {
    let got = state_with(
        || {
            std::thread::sleep(Duration::from_secs(2));
            LaunchdState::on_demand()
        },
        Duration::from_millis(100),
        true,
    )
    .await;
    assert_eq!(got, plist_wait_state());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicked_read_with_a_plist_is_not_loaded_launchd() {
    let got = state_with(|| panic!("reader exploded"), Duration::from_secs(5), true).await;
    assert_eq!(got, plist_wait_state());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_read_without_a_plist_is_on_demand() {
    let got = state_with(|| panic!("reader exploded"), Duration::from_secs(5), false).await;
    assert_eq!(got, LaunchdState::on_demand());
}

#[test]
fn unanswered_read_with_a_plist_waits_instead_of_spawning() {
    let s = unanswered_state(true);
    assert_eq!(decide(&s.owner, s.loaded, s.pid), Plan::WaitUnloaded);
    let s = unanswered_state(false);
    assert_eq!(decide(&s.owner, s.loaded, s.pid), Plan::Spawn);
}

/// The recheck after a failed kickstart times out: the guard must not spawn.
#[tokio::test(flavor = "multi_thread")]
async fn a_recheck_that_times_out_with_a_plist_never_spawns() {
    let (_dir, client, _sock) = scratch();
    let err = ensure_with(
        &client,
        BUDGET,
        state(true, None),
        || async { Err(anyhow::anyhow!("kickstart refused")) },
        || {
            state_with(
                || {
                    std::thread::sleep(Duration::from_secs(2));
                    LaunchdState::on_demand()
                },
                Duration::from_millis(100),
                true,
            )
        },
        no_spawn,
    )
    .await
    .expect_err("nothing serves");
    assert!(err.to_string().contains(LABEL), "{err}");
}

#[test]
fn device_note_only_for_a_non_spawn_plan() {
    assert!(device_note(None, Plan::Kickstart).is_none());
    assert!(device_note(Some("cpu"), Plan::Spawn).is_none());
    for plan in [Plan::Kickstart, Plan::Wait, Plan::WaitUnloaded] {
        let note = device_note(Some("cpu"), plan).expect("note");
        assert!(
            note.contains("--device cpu") && note.contains("daemon.env"),
            "{note}"
        );
    }
}
