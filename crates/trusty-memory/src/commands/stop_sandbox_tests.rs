//! Tests for `stop` under `TRUSTY_DATA_DIR_OVERRIDE` (#9140).
//!
//! No test reaches launchd or a live daemon: the live path is a closure that
//! records whether it ran, and every signalled process is a stand-in this
//! test spawned.

use super::*;
use sandbox::{refuse_live_unit_under_override, stop_target, StopTarget};
use std::cell::Cell;
use trusty_common::DATA_DIR_OVERRIDE_ENV;

/// Sets `TRUSTY_DATA_DIR_OVERRIDE` and restores the previous value on drop.
/// Hold `env_test_lock` for its whole life.
struct OverrideGuard(Option<std::ffi::OsString>);

impl OverrideGuard {
    fn set(value: Option<&std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(DATA_DIR_OVERRIDE_ENV);
        // SAFETY: callers hold `env_test_lock`, the crate's override lock.
        unsafe {
            match value {
                Some(v) => std::env::set_var(DATA_DIR_OVERRIDE_ENV, v),
                None => std::env::remove_var(DATA_DIR_OVERRIDE_ENV),
            }
        }
        Self(previous)
    }
}

impl Drop for OverrideGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`; the lock outlives this guard.
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var(DATA_DIR_OVERRIDE_ENV, v),
                None => std::env::remove_var(DATA_DIR_OVERRIDE_ENV),
            }
        }
    }
}

/// A daemon stand-in with a reaper, so `kill -0` stops seeing it once it dies.
struct ReapedStandIn {
    pid: u32,
    _guard: KillPidOnDrop,
    _stdin: std::process::ChildStdin,
    reaper: std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
}

fn reaped_stand_in(dir: &std::path::Path, args: &[&str]) -> ReapedStandIn {
    let mut child = spawn_stand_in(dir, args);
    let pid = child.id();
    let stdin = child.stdin.take().expect("piped stdin");
    let reaper = std::thread::spawn(move || child.wait());
    ReapedStandIn {
        pid,
        _guard: KillPidOnDrop(Some(pid)),
        _stdin: stdin,
        reaper,
    }
}

/// Why (#9140): a sandbox cleanup ran `stop` with the override set and it
/// stopped the live launchd unit. Here the "live" daemon is a stand-in in the
/// table and the live path is a closure that stops it, as the real one would.
/// Nothing serves the override's socket, so no daemon is proven to own the
/// override's data dir: the stop fails and the live path never runs.
#[tokio::test]
async fn stop_under_a_data_dir_override_never_reaches_the_live_unit() {
    let _env = crate::commands::env_test_lock().lock().await;
    let data = tempfile::tempdir().expect("tempdir");
    let bin = tempfile::tempdir().expect("tempdir");
    let live = reaped_stand_in(bin.path(), &["serve", "--foreground"]);
    let rows = live_rows_for(&[live.pid]);
    assert_eq!(rows.len(), 1, "live stand-in visible with argv: {rows:?}");

    let target = {
        let _override = OverrideGuard::set(Some(data.path().as_os_str()));
        StopTarget::resolve().await
    }
    .expect("an absolute override resolves");

    let live_reached = Cell::new(false);
    let result = stop_target(
        &target,
        &rows,
        std::process::id(),
        Duration::from_secs(5),
        || {
            live_reached.set(true);
            stop_daemons_in(&rows, std::process::id(), Duration::from_secs(5))
        },
    );
    assert!(
        !live_reached.get(),
        "an override must never route to the live launchd unit"
    );
    assert!(pid_alive(live.pid), "the live daemon was signalled");
    let err = result.expect_err("no proven owner is a failed stop");
    assert!(format!("{err:#}").contains("cannot prove"), "{err:#}");
}

/// Fail-Open Check (#9140): without proof of ownership the stop is an error
/// and signals nothing — no owner at all, or an owner that is not a
/// `trusty-memory` daemon in the table (here, a stdio bridge).
#[test]
fn sandbox_stop_without_proof_of_ownership_fails_and_signals_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = reaped_stand_in(dir.path(), &["serve", "--foreground"]);
    let bridge = reaped_stand_in(dir.path(), &["serve", "--stdio"]);
    let rows = live_rows_for(&[daemon.pid, bridge.pid]);
    assert_eq!(rows.len(), 2, "stand-ins visible with argv: {rows:?}");

    let socket = dir.path().join("trusty-memory.sock");
    for (owner, expected) in [
        (None, "cannot prove"),
        (Some(bridge.pid), "not a trusty-memory daemon"),
        (Some(std::process::id()), "not a trusty-memory daemon"),
    ] {
        let target = StopTarget::Sandbox {
            socket: socket.clone(),
            owner,
        };
        let err = stop_target(
            &target,
            &rows,
            std::process::id(),
            Duration::from_secs(5),
            || panic!("the live path ran under an override"),
        )
        .expect_err("no proof of ownership is a failed stop");
        assert!(err.to_string().contains(expected), "{owner:?}: {err}");
    }
    assert!(pid_alive(daemon.pid), "the unproven daemon was signalled");
    assert!(pid_alive(bridge.pid), "the bridge was signalled");
}

/// Why (#9140): with two daemons running, only the one serving the
/// override's socket is stopped.
#[test]
fn sandbox_stop_signals_only_the_socket_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let owner = reaped_stand_in(dir.path(), &["serve", "--foreground"]);
    let other = reaped_stand_in(dir.path(), &["serve", "--foreground"]);
    let rows = live_rows_for(&[owner.pid, other.pid]);
    assert_eq!(rows.len(), 2, "stand-ins visible with argv: {rows:?}");

    let target = StopTarget::Sandbox {
        socket: dir.path().join("trusty-memory.sock"),
        owner: Some(owner.pid),
    };
    stop_target(
        &target,
        &rows,
        std::process::id(),
        Duration::from_secs(5),
        || panic!("the live path ran under an override"),
    )
    .expect("the proven owner stops");
    let status = owner.reaper.join().expect("reaper").expect("wait");
    assert!(!status.success(), "owner ended by a signal: {status:?}");
    assert!(
        pid_alive(other.pid),
        "a daemon of another data dir was signalled"
    );
}

/// Why (#9140): ownership is the kernel's answer for the override's socket.
/// What: unset is the live target; a blank override is refused; with a
/// listener bound at the override's socket, the owner is this process.
#[tokio::test]
async fn stop_target_resolves_the_override_socket_owner_by_peer_pid() {
    let _env = crate::commands::env_test_lock().lock().await;
    {
        let _unset = OverrideGuard::set(None);
        assert_eq!(
            StopTarget::resolve().await.expect("unset"),
            StopTarget::Live
        );
    }
    {
        let _blank = OverrideGuard::set(Some(std::ffi::OsStr::new("  ")));
        let err = StopTarget::resolve().await.expect_err("blank is refused");
        assert!(err.to_string().contains("blank"), "{err}");
    }
    let data = tempfile::tempdir().expect("tempdir");
    let _override = OverrideGuard::set(Some(data.path().as_os_str()));
    let socket = crate::socket_path().expect("override socket");
    std::fs::create_dir_all(socket.parent().expect("parent")).expect("mkdir");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");

    let target = StopTarget::resolve().await.expect("resolve");
    assert_eq!(
        target,
        StopTarget::Sandbox {
            socket,
            owner: Some(std::process::id()),
        }
    );
}

/// Why (#9140): `service stop` boots out the live unit with no data-dir
/// check, so an override refuses it; unset, it is allowed.
#[test]
fn service_stop_is_refused_under_a_data_dir_override() {
    let _env = crate::commands::env_test_lock().blocking_lock();
    let data = tempfile::tempdir().expect("tempdir");
    {
        let _override = OverrideGuard::set(Some(data.path().as_os_str()));
        let err = refuse_live_unit_under_override("service stop").expect_err("refused");
        assert!(err.to_string().contains("service stop"), "{err}");
    }
    let _unset = OverrideGuard::set(None);
    refuse_live_unit_under_override("service stop").expect("allowed without an override");
}

/// Why (#9140): `service install` and `service start` bootstrap the live
/// unit just as `service stop` boots it out, so each is refused under an
/// override; `service logs` only reads and is not. Drives the guard
/// `handle_service` calls, never `launchctl`.
#[test]
fn service_install_start_and_stop_are_refused_under_a_data_dir_override() {
    use crate::commands::service::{refuse_live_unit_action, ServiceAction};
    let _env = crate::commands::env_test_lock().blocking_lock();
    let data = tempfile::tempdir().expect("tempdir");
    let _override = OverrideGuard::set(Some(data.path().as_os_str()));
    for (action, name) in [
        (ServiceAction::Install, "service install"),
        (ServiceAction::Start, "service start"),
        (ServiceAction::Stop, "service stop"),
    ] {
        let err = refuse_live_unit_action(&action).expect_err(name);
        assert!(err.to_string().contains(name), "{err}");
    }
    refuse_live_unit_action(&ServiceAction::Logs).expect("logs only reads");
}
