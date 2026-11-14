//! Tests for the Linux lifecycle verbs (#11085).
//!
//! The stop, identity and spawn rules are tested against real processes in
//! trusty-common's `tests/linux_lifecycle.rs`; these tests pin what this crate
//! supplies — the descriptor and the exit status of each verb — plus one
//! real-process run of `restart` through the module's signal seam (AC2).

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use trusty_common::linux_lifecycle::{
    LifecycleError, PinnedProcess, Signal, SignalDelivery, StopOutcome, StoppedSignal,
};

use super::*;

const SOCK: &str = "/tmp/tm-lc-test/trusty-memory.sock";

/// The fixture's socket variable; set only in the fixture child.
const FIXTURE_SOCKET_ENV: &str = "TM_LC_FIXTURE_SOCKET";

#[test]
fn descriptor_names_the_daemon_args_and_the_socket_stop_signal() {
    let _env = crate::commands::env_test_lock().blocking_lock();
    let desc = descriptor_for(None, PathBuf::from(SOCK)).expect("descriptor");
    assert_eq!(desc.binary, "trusty-memory");
    assert_eq!(desc.socket, PathBuf::from(SOCK));
    assert_eq!(desc.stopped_signal, StoppedSignal::SocketRefuses);
    // #11085: the module does not reject an empty liveness_args or [""]; an
    // empty one counts every stdio bridge as the daemon.
    assert_eq!(desc.liveness_args, ["serve", "--foreground"]);
    assert!(desc.liveness_args.iter().all(|a| !a.trim().is_empty()));
    assert_eq!(desc.spawn_args, desc.liveness_args);
    for name in [
        "RUST_LOG",
        "HTTPS_PROXY",
        "https_proxy",
        "NO_PROXY",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "CARGO_HOME",
    ] {
        assert!(
            desc.env_passthrough.iter().any(|n| n == name),
            "{name} must reach the daemon"
        );
    }
    // The module's default instance key is the only one memory needs.
    assert_eq!(
        desc.instance_env,
        vec![(
            DATA_DIR_OVERRIDE_ENV.to_string(),
            std::env::var_os(DATA_DIR_OVERRIDE_ENV)
        )]
    );
    assert_eq!(desc.working_dir, PathBuf::from("/"));
}

#[test]
fn descriptor_refuses_a_blank_data_dir_override() {
    let _env = crate::commands::env_test_lock().blocking_lock();
    for blank in ["", " ", "\t"] {
        let err = descriptor_for(Some(&OsString::from(blank)), PathBuf::from(SOCK))
            .expect_err("a blank override must be refused");
        assert!(format!("{err:#}").contains("blank"), "{err:#}");
    }
    assert!(descriptor_for(Some(&OsString::from("/tmp/x")), PathBuf::from(SOCK)).is_ok());
}

#[test]
fn stop_report_maps_each_outcome_to_its_exit_status() {
    let stopped = StopOutcome::Stopped {
        pid: 7,
        forced: false,
    };
    assert!(stop_report(Ok(stopped)).expect("stopped is success"));
    // P-b: `stop` keeps exit 1 when nothing runs.
    let none = stop_report(Ok(StopOutcome::NotRunning)).expect_err("nothing to stop");
    assert!(format!("{none:#}").contains("No daemon running"));
    let survivor = stop_report(Err(LifecycleError::Survivor { pid: 4242 }))
        .expect_err("a survivor is an error");
    assert!(format!("{survivor:#}").contains("4242"), "{survivor:#}");
}

// Fail-open guard: the AlreadyRunning arm is the one error turned into success.
#[test]
fn start_report_treats_only_already_running_as_success() {
    let already = LifecycleError::AlreadyRunning {
        socket: PathBuf::from(SOCK),
        pid: Some(9),
    };
    assert!(start_report(Err(already)).is_ok());
    let failures = [
        LifecycleError::NotReady {
            pid: 5,
            timeout: Duration::from_secs(1),
            killed: true,
        },
        LifecycleError::ChildExited { pid: 5 },
        LifecycleError::BinaryMissing {
            path: PathBuf::from("/nowhere/trusty-memory"),
        },
        LifecycleError::RunningUnserved {
            socket: PathBuf::from(SOCK),
            binary: BINARY.into(),
            pids: vec![11],
        },
    ];
    for failure in failures {
        let shown = failure.to_string();
        assert!(start_report(Err(failure)).is_err(), "{shown} must fail");
    }
}

/// Delivers SIGTERM, drops SIGKILL: a survivor an unprivileged test can make.
struct DropKill;

impl SignalDelivery for DropKill {
    fn deliver(&self, target: &PinnedProcess, signal: Signal) -> io::Result<()> {
        match signal {
            Signal::Kill => Ok(()),
            other => target.send_signal(other),
        }
    }
}

/// The fixture: when [`FIXTURE_SOCKET_ENV`] is set, serve that socket and
/// ignore SIGTERM until the socket's directory is gone; otherwise a no-op.
#[test]
fn lifecycle_fixture_entry() {
    let Some(socket) = std::env::var_os(FIXTURE_SOCKET_ENV).map(PathBuf::from) else {
        return;
    };
    // SAFETY: installing SIG_IGN for SIGTERM has no preconditions.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("fixture bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let dir = socket.parent().expect("socket dir").to_path_buf();
    let deadline = Instant::now() + Duration::from_secs(60);
    while dir.exists() && Instant::now() < deadline {
        let _ = listener.accept();
        std::thread::sleep(Duration::from_millis(20));
    }
    std::process::exit(0);
}

/// Spawn the fixture as `<dir>/bin/<binary>` serving `socket`.
///
/// The test binary is hard-linked beside itself, so `/proc/<pid>/exe` names
/// `binary` and the link stays on one filesystem.
fn spawn_fixture(dir: &std::path::Path, binary: &str, socket: &std::path::Path) -> Child {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let exe = std::env::current_exe().expect("current exe");
    let link = bin.join(binary);
    std::fs::hard_link(&exe, &link)
        .or_else(|_| std::fs::copy(&exe, &link).map(|_| ()))
        .expect("link the fixture");
    let child = Command::new(&link)
        // `--exact` matches the full test path, not the bare fn name.
        .args([
            "commands::linux_lifecycle::tests::lifecycle_fixture_entry",
            "--exact",
            "--nocapture",
        ])
        .env(FIXTURE_SOCKET_ENV, socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fixture");
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        assert!(Instant::now() < deadline, "fixture never served");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

// AC2: a survivor fails the restart, names its pid, and nothing is started.
#[tokio::test(flavor = "multi_thread")]
async fn restart_report_names_a_survivor_and_starts_nothing() {
    let _env = crate::commands::env_test_lock().lock().await;
    let exe_dir = std::env::current_exe().expect("exe");
    let tmp = tempfile::tempdir_in(exe_dir.parent().expect("exe dir")).expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    let socket = tmp.path().join("run").join("trusty-memory.sock");
    std::fs::create_dir_all(socket.parent().expect("run dir")).expect("run dir");
    let mut fixture = spawn_fixture(tmp.path(), BINARY, &socket);

    // SAFETY: env_test_lock is held for the whole window the variable is set.
    unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, &data) };
    let mut desc =
        descriptor_for(Some(&data.clone().into_os_string()), socket.clone()).expect("descriptor");
    // An install dir with no binary: a start that ran would fail BinaryMissing.
    desc.install_dir = tmp.path().join("empty-install");
    desc.stop_grace = Duration::from_millis(300);
    desc.kill_wait = Duration::from_millis(500);
    let result = restart_with(&desc, &DropKill).await;
    // SAFETY: as above.
    unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) };

    let alive = fixture.try_wait().expect("try_wait").is_none();
    let _ = fixture.kill();
    let _ = fixture.wait();
    let err = result.expect_err("a survivor must fail the restart");
    let shown = format!("{err:#}");
    assert!(shown.contains(&fixture.id().to_string()), "{shown}");
    assert!(shown.contains("survived SIGKILL"), "{shown}");
    assert!(!shown.contains("not installed"), "start ran: {shown}");
    assert!(alive, "the fixture survived, as the seam made it");
}

// AC11 / P-b: a daemon that is not running is a `stop` error and a `restart`
// that reaches the start; here the start meets an empty install dir.
#[tokio::test(flavor = "multi_thread")]
async fn restart_of_a_daemon_that_is_not_running_goes_on_to_start() {
    let _env = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    // SAFETY: env_test_lock is held for the whole window the variable is set.
    unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, &data) };
    let mut desc = descriptor_for(
        Some(&data.clone().into_os_string()),
        tmp.path().join("absent.sock"),
    )
    .expect("descriptor");
    desc.install_dir = tmp.path().join("empty-install");
    let stop = stop_report(lc::stop(&desc).await);
    let restart = restart_with(&desc, &KernelDelivery).await;
    // SAFETY: as above.
    unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) };

    let stop = stop.expect_err("stop on a stopped daemon exits 1");
    assert!(format!("{stop:#}").contains("No daemon running"));
    let restart = format!("{:#}", restart.expect_err("no binary to start"));
    assert!(
        restart.contains("not installed"),
        "restart reached start: {restart}"
    );
}
