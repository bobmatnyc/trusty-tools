//! `service restart` terminates a detached daemon and verifies the result
//! (#8686).
//!
//! Why: `launchctl bootout` left a daemon detached from launchd (PPID 1)
//! serving the old version on :7878, and the restart read as successful.
//! What: drives [`restart_with`] over recorded effects. PID 1685 stands for the
//! detached daemon from the report: bootout does not stop it. The scoping tests
//! drive [`scoped_daemon_pids`] and [`terminate_scoped`] over fixed process
//! tables, so no real daemon is signalled.
//! Test: this module.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::service_restart::{
    restart_unit, restart_with, scoped_daemon_pids, terminate_scoped, unit_data_dir_override,
    unit_socket_path, version_from_health, RestartEffects,
};
use super::start::reap_orphans::Candidate;

const DETACHED: u32 = 1685;

/// #8686: the detached PID that survives bootout is terminated, and only then
/// is the unit bootstrapped.
#[test]
fn a_detached_daemon_is_terminated_before_the_bootstrap() {
    let steps = RefCell::new(Vec::<String>::new());
    let dead = RefCell::new(Vec::<u32>::new());
    let report = restart_with(
        vec![DETACHED],
        "0.55.0",
        || {
            steps.borrow_mut().push("bootout".into());
            Ok(())
        },
        |pid| !dead.borrow().contains(&pid),
        |pids| {
            steps.borrow_mut().push(format!("terminate {pids:?}"));
            dead.borrow_mut().extend_from_slice(pids);
            Vec::new()
        },
        || {
            steps.borrow_mut().push("bootstrap".into());
            Ok(())
        },
        || Ok("0.55.0".to_string()),
    )
    .expect("the restart succeeds");
    assert_eq!(
        steps.into_inner(),
        vec!["bootout", "terminate [1685]", "bootstrap"]
    );
    assert_eq!(report.replaced, vec![DETACHED]);
    assert_eq!(report.version, "0.55.0");
}

/// #8686: a PID that outlives SIGTERM and SIGKILL stops the restart before the
/// bootstrap, which would otherwise crash-loop against the held port.
#[test]
fn a_survivor_aborts_before_the_bootstrap() {
    let bootstrapped = RefCell::new(false);
    let err = restart_with(
        vec![DETACHED],
        "0.55.0",
        || Ok(()),
        |_| true,
        |pids| pids.to_vec(),
        || {
            *bootstrapped.borrow_mut() = true;
            Ok(())
        },
        || Ok("0.55.0".to_string()),
    )
    .expect_err("a surviving old daemon fails the restart");
    assert!(!bootstrapped.into_inner(), "no bootstrap onto a held port");
    assert!(err.to_string().contains("1685"), "{err}");
}

/// #8686 acceptance: `/health` must report the new version; the old version
/// answering means an old daemon is still serving.
#[test]
fn an_old_version_on_health_fails_the_restart() {
    let err = restart_with(
        vec![DETACHED],
        "0.54.3",
        || Ok(()),
        |_| false,
        |_| Vec::new(),
        || Ok(()),
        || Ok("0.54.2".to_string()),
    )
    .expect_err("an old version on /health fails the restart");
    assert!(err.to_string().contains("0.54.2"), "{err}");
}

/// The health probe reads `version` from the report.
#[test]
fn health_version_is_read_from_the_report() {
    let report = serde_json::json!({"status": "ok", "version": "0.55.0", "indexes": 3});
    assert_eq!(version_from_health(&report).as_deref(), Some("0.55.0"));
    assert_eq!(version_from_health(&serde_json::json!({})), None);
}

fn daemon(pid: u32, data_dir: &str) -> Candidate {
    Candidate {
        pid,
        argv: [
            "trusty-search",
            "start",
            "--foreground",
            "--data-dir",
            data_dir,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        environ: vec!["HOME=/Users/op".to_string()],
    }
}

/// HIGH-2 (#4395): the restart targets only daemons on the unit's data dir; a
/// second instance on another data dir is never in the kill set. On pre-fix
/// code every running `trusty-search start` process was.
#[test]
fn a_daemon_on_another_data_dir_is_not_targeted() {
    let table = [daemon(10, "/data/unit"), daemon(20, "/data/other")];
    assert_eq!(
        scoped_daemon_pids(
            &table,
            Path::new("/data/unit"),
            Path::new("/platform/default")
        ),
        vec![10]
    );
}

/// HIGH-2: a PID that stopped being a daemon on this data dir after SIGTERM
/// (exited, and possibly reused) is not SIGKILLed, and is not a survivor.
#[test]
fn a_pid_no_longer_a_scoped_daemon_is_not_sigkilled() {
    let scans = RefCell::new(0);
    let signals = RefCell::new(Vec::new());
    let survivors = terminate_scoped(
        &[10],
        || {
            *scans.borrow_mut() += 1;
            // The first scan, before SIGTERM, still finds it; later scans do
            // not, although PID 10 is alive as some other process.
            if *scans.borrow() == 1 {
                vec![10]
            } else {
                Vec::new()
            }
        },
        |pid, sig| signals.borrow_mut().push((pid, sig)),
        |_, _| {},
    );
    assert_eq!(signals.into_inner(), vec![(10, "TERM")]);
    assert!(survivors.is_empty(), "{survivors:?}");
}

/// HIGH-2: a daemon still serving this data dir after the grace is SIGKILLed,
/// and reported if it survives that too.
#[test]
fn a_scoped_daemon_that_ignores_sigterm_is_sigkilled() {
    let signals = RefCell::new(Vec::new());
    let survivors = terminate_scoped(
        &[10],
        || vec![10],
        |pid, sig| signals.borrow_mut().push((pid, sig)),
        |_, _| {},
    );
    assert_eq!(signals.into_inner(), vec![(10, "TERM"), (10, "KILL")]);
    assert_eq!(survivors, vec![10]);
}

/// HIGH-2: the unit's data dir comes from its plist — `--data-dir` first, then
/// `TRUSTY_DATA_DIR` — and `None` means the platform default.
#[test]
fn the_unit_data_dir_comes_from_the_plist() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let env = vec![("TRUSTY_DATA_DIR".to_string(), "/data/env".to_string())];
    let read = |a: &[&str], e: &[(String, String)]| {
        unit_data_dir_override(&args(a), e).expect("a declared data dir")
    };
    assert_eq!(
        read(&["ts", "start", "--data-dir", "/data/flag"], &env),
        Some(PathBuf::from("/data/flag"))
    );
    assert_eq!(
        read(&["ts", "start", "--data-dir=/data/eq"], &[]),
        Some(PathBuf::from("/data/eq"))
    );
    assert_eq!(
        read(&["ts", "start"], &env),
        Some(PathBuf::from("/data/env"))
    );
    assert_eq!(read(&["ts", "start"], &[]), None);
}

const PLATFORM_DEFAULT: &str = "/platform/default";

/// Recorded [`RestartEffects`]: a process table a signal removes PIDs from,
/// and every signal and launchd step taken.
struct Recorder {
    table: RefCell<Vec<Candidate>>,
    signals: RefCell<Vec<(u32, &'static str)>>,
    steps: RefCell<Vec<&'static str>>,
}

impl Recorder {
    fn new(table: Vec<Candidate>) -> Self {
        Self {
            table: RefCell::new(table),
            signals: RefCell::new(Vec::new()),
            steps: RefCell::new(Vec::new()),
        }
    }
}

impl RestartEffects for Recorder {
    fn candidates(&self) -> Vec<Candidate> {
        self.table.borrow().clone()
    }
    fn signal(&self, pid: u32, sig: &'static str) {
        self.signals.borrow_mut().push((pid, sig));
        self.table.borrow_mut().retain(|c| c.pid != pid);
    }
    fn wait_until(&self, _: std::time::Duration, _: &dyn Fn() -> bool) {}
    fn socket_for(&self, _: Option<&Path>) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from("/unit/trusty-search.sock"))
    }
    fn bootout(&self) -> anyhow::Result<()> {
        self.steps.borrow_mut().push("bootout");
        Ok(())
    }
    fn bootstrap(&self) -> anyhow::Result<()> {
        self.steps.borrow_mut().push("bootstrap");
        Ok(())
    }
    fn health_version(&self, _: &Path) -> anyhow::Result<String> {
        Ok("0.55.0".to_string())
    }
}

/// Restart a unit over `rec` whose table holds a daemon on the platform
/// default — the daemon a fallback to the default would signal.
fn restart_over_default_daemon(args: &[&str], rec: &Recorder) -> anyhow::Result<()> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    restart_unit(&args, &[], Path::new(PLATFORM_DEFAULT), "0.55.0", rec).map(|_| ())
}

/// MEDIUM (#8686 review): a plist with no `ProgramArguments` (binary or
/// malformed) aborts the restart before any scan, bootout or signal. On pre-fix
/// code it fell back to the platform default and signalled that daemon.
#[test]
fn a_unit_without_program_arguments_signals_nothing() {
    let rec = Recorder::new(vec![daemon(30, PLATFORM_DEFAULT)]);
    let result = restart_over_default_daemon(&[], &rec);
    assert!(rec.signals.borrow().is_empty(), "{:?}", rec.signals);
    assert!(rec.steps.borrow().is_empty(), "{:?}", rec.steps);
    let err = result.expect_err("no ProgramArguments fails");
    assert!(err.to_string().contains("ProgramArguments"), "{err}");
}

/// MEDIUM (#8686 review): a dangling or empty `--data-dir` aborts the restart
/// before any scan, bootout or signal, as `reap_orphans::declared_data_dir`
/// does. On pre-fix code it fell back to the platform default.
#[test]
fn a_dangling_data_dir_signals_nothing() {
    for args in [
        &["trusty-search", "start", "--data-dir"][..],
        &["trusty-search", "start", "--data-dir="][..],
    ] {
        let rec = Recorder::new(vec![daemon(30, PLATFORM_DEFAULT)]);
        let result = restart_over_default_daemon(args, &rec);
        assert!(
            rec.signals.borrow().is_empty(),
            "{args:?}: {:?}",
            rec.signals
        );
        assert!(rec.steps.borrow().is_empty(), "{args:?}: {:?}", rec.steps);
        let err = result.expect_err("a dangling flag fails");
        assert!(err.to_string().contains("`--data-dir`"), "{args:?}: {err}");
    }
}

/// #8686: a unit with a declared data dir signals only its own detached
/// daemon, never the one on the platform default, then bootstraps.
#[test]
fn the_restart_signals_only_the_units_daemon() {
    let rec = Recorder::new(vec![daemon(10, "/data/unit"), daemon(30, PLATFORM_DEFAULT)]);
    restart_over_default_daemon(
        &["trusty-search", "start", "--data-dir", "/data/unit"],
        &rec,
    )
    .expect("the restart succeeds");
    assert_eq!(rec.signals.into_inner(), vec![(10, "TERM")]);
    assert_eq!(rec.steps.into_inner(), vec!["bootout", "bootstrap"]);
}

/// HIGH-2: the health probe dials the socket under the unit's data dir, not
/// the one the CLI's own `TRUSTY_DATA_DIR` names.
#[test]
fn the_health_probe_uses_the_units_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = unit_socket_path(Some(dir.path())).expect("socket path");
    assert_eq!(socket.parent(), Some(dir.path()));
}
