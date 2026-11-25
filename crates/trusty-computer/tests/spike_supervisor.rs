//! Port of the spike's `tests/test_supervisor.py` (5 cases), on the fake launcher.

mod common;

use std::time::Duration;

use common::{FakeLauncher, short_tmp};
use trusty_computer::driver::DriverError;
use trusty_computer::launcher::DAEMON_ENV;
use trusty_computer::manifest::LeaseScope;
use trusty_computer::supervisor::Supervisor;

const SCOPE: LeaseScope = LeaseScope {
    pid: 4242,
    window_id: 77,
};

fn make(dir: &tempfile::TempDir, launcher: FakeLauncher) -> Supervisor<FakeLauncher> {
    Supervisor::new(launcher, dir.path())
        .expect("supervisor")
        .with_ready_timeout(Duration::from_secs(2))
}

fn version(meta: &serde_json::Value) -> &str {
    meta["driver_version"].as_str().unwrap_or_default()
}

#[test]
fn supervisor_start_stop_restart() {
    let dir = short_tmp();
    let mut sup = make(&dir, FakeLauncher::new("0.23.2", None));
    assert_eq!(version(&sup.start(SCOPE).expect("start")), "0.23.2");
    assert_eq!(sup.running_version().as_deref(), Some("0.23.2"));
    assert_eq!(version(&sup.restart(SCOPE).expect("restart")), "0.23.2");
    let l = sup.launcher();
    assert_eq!((l.starts, l.stops, sup.restarts()), (2, 1, 1));
    sup.stop().expect("stop");
    assert_eq!(sup.running_version(), None);
}

#[test]
fn supervisor_start_is_idempotent_when_running() {
    let dir = short_tmp();
    let mut sup = make(&dir, FakeLauncher::new("0.23.2", None));
    sup.start(SCOPE).expect("first");
    sup.start(SCOPE).expect("second");
    assert_eq!(sup.launcher().starts, 1);
    sup.stop().expect("stop");
}

#[test]
fn supervisor_refuses_unpinned_binary() {
    let dir = short_tmp();
    let mut sup = make(&dir, FakeLauncher::new("0.53.0", None));
    let err = sup.start(SCOPE).expect_err("must refuse");
    assert!(err.to_string().contains("!= pinned"), "{err}");
    assert_eq!(sup.launcher().starts, 0);
}

#[test]
fn supervisor_stops_daemon_reporting_wrong_version() {
    let dir = short_tmp();
    let mut sup = make(&dir, FakeLauncher::new("0.23.2", Some("0.24.0")));
    let err = sup.start(SCOPE).expect_err("must refuse");
    assert!(
        matches!(&err, DriverError::Version(m) if m.contains("daemon reports 0.24.0")),
        "{err}"
    );
    assert_eq!(sup.launcher().stops, 1);
    assert_eq!(sup.running_version(), None);
    assert_eq!((sup.pid(), sup.scope()), (None, None));
}

#[test]
fn supervisor_daemon_env_turns_off_telemetry_update_and_gate() {
    let get = |k: &str| DAEMON_ENV.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    assert_eq!(get("CUA_DRIVER_RS_TELEMETRY_ENABLED"), Some("false"));
    assert_eq!(get("CUA_DRIVER_RS_UPDATE_CHECK"), Some("false"));
    assert_eq!(get("CUA_DRIVER_RS_PERMISSIONS_GATE"), Some("false"));
}

/// #11460 fix round: a failed `owns` check leaves no pid or scope behind,
/// so a second start or rescope cannot treat the daemon as verified.
#[test]
fn failed_start_clears_pid_and_scope() {
    let dir = short_tmp();
    let mut launcher = FakeLauncher::new("0.23.2", None);
    launcher.disown = true;
    let mut sup = make(&dir, launcher);
    let err = sup.start(SCOPE).expect_err("an unowned pid refuses");
    assert!(err.to_string().contains("is not a daemon serving"), "{err}");
    assert_eq!((sup.pid(), sup.scope()), (None, None));
    let again = sup.start(SCOPE).expect_err("second start refuses too");
    assert_eq!((sup.pid(), sup.scope()), (None, None), "{again}");
    let rescope = sup
        .rescope(SCOPE)
        .expect_err("rescope must not pass as a no-op");
    assert_eq!((sup.pid(), sup.scope()), (None, None), "{rescope}");
}
