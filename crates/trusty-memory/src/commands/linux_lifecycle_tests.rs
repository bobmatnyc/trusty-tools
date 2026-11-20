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
    LifecycleError, PinnedProcess, ProcessTable, Signal, SignalDelivery, StopOutcome, StoppedSignal,
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
    assert!(DaemonDescriptor::BASE_ENV.contains(&DATA_DIR_OVERRIDE_ENV));
    assert_eq!(
        desc.instance_env,
        vec![(
            DATA_DIR_OVERRIDE_ENV.to_string(),
            std::env::var_os(DATA_DIR_OVERRIDE_ENV)
        )]
    );
    assert_eq!(desc.working_dir, PathBuf::from("/"));
}

// #11085: a variable missing here is one a `start`-spawned daemon silently
// ignores, while a bridge auto-start (whole environment) honours it.
#[test]
fn descriptor_passes_the_security_and_data_variables_but_no_credential() {
    let _env = crate::commands::env_test_lock().blocking_lock();
    let desc = descriptor_for(None, PathBuf::from(SOCK)).expect("descriptor");
    let passed = |name: &str| desc.env_passthrough.iter().any(|n| n == name);
    for name in [
        // Gates the authz refusal of `force=true palace_create`.
        "TRUSTY_MEMORY_MULTI_TENANT",
        // The model variant every stored vector was embedded with.
        "TRUSTY_EMBEDDER_MODEL",
        "TRUSTY_DEVICE",
        "FASTEMBED_CACHE_DIR",
        "FASTEMBED_CACHE_PATH",
        "HF_HOME",
        "TRUSTY_MEMORY_REDB_CACHE_MB",
        "TRUSTY_BM25_DAEMON",
        "TRUSTY_WRITE_OP_BUDGET_SECS",
        "TRUSTY_TERMINATION_GRACE_SECS",
        "OLLAMA_HOST",
        "LD_LIBRARY_PATH",
    ] {
        assert!(passed(name), "{name} must reach the daemon");
    }
    for name in &desc.env_passthrough {
        let credential = ["KEY", "TOKEN", "SECRET", "PASSWORD"]
            .iter()
            .any(|word| name.contains(word));
        assert!(!credential, "{name} looks like a credential");
    }
    assert!(!passed("OPENROUTER_API_KEY"));
    // A parent-death stamp is the caller's, never the daemon's (#11098).
    assert!(!passed("TRUSTY_EXIT_WITH_PARENT"));
}

#[test]
fn descriptor_refuses_a_non_utf8_data_dir_override() {
    use std::os::unix::ffi::OsStringExt as _;
    let _env = crate::commands::env_test_lock().blocking_lock();
    // `resolve_data_dir` reads this as unset, so it names the live data dir.
    let raw = OsString::from_vec(b"/tmp/tm-\xff".to_vec());
    let err = descriptor_for(Some(&raw), PathBuf::from(SOCK))
        .expect_err("a non-UTF-8 override must be refused");
    assert!(format!("{err:#}").contains("UTF-8"), "{err:#}");
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

/// Hard-link the test binary as `<dir>/bin/<binary>`; returns the link.
fn install_fixture(dir: &std::path::Path, binary: &str) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let exe = std::env::current_exe().expect("current exe");
    let link = bin.join(binary);
    std::fs::hard_link(&exe, &link)
        .or_else(|_| std::fs::copy(&exe, &link).map(|_| ()))
        .expect("link the fixture");
    link
}

/// Spawn the fixture as `<dir>/bin/<binary>` serving `socket`.
///
/// The test binary is hard-linked beside itself, so `/proc/<pid>/exe` names
/// `binary` and the link stays on one filesystem.
fn spawn_fixture(dir: &std::path::Path, binary: &str, socket: &std::path::Path) -> Child {
    let link = install_fixture(dir, binary);
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

/// A process table under `dir` holding only `pids`, each a link to its real
/// `/proc` entry.
///
/// Why: #11085 — a scan of the live `/proc` fails closed on a host
/// trusty-memory daemon whose environ it cannot read, which failed these
/// tests on such a host.
fn own_procs(dir: &std::path::Path, pids: &[u32]) -> ProcessTable {
    std::fs::create_dir_all(dir).expect("proc table dir");
    for pid in pids {
        std::os::unix::fs::symlink(format!("/proc/{pid}"), dir.join(pid.to_string()))
            .expect("link the proc entry");
    }
    ProcessTable::at(dir)
}

// AC2: a survivor fails the restart, names its pid, and nothing is started.
#[tokio::test(flavor = "multi_thread")]
async fn restart_report_names_a_survivor_and_starts_nothing() {
    let _env = crate::commands::env_test_lock().lock().await;
    let exe_dir = std::env::current_exe().expect("exe");
    let tmp = tempfile::tempdir_in(exe_dir.parent().expect("exe dir")).expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    // A short directory: the exe-dir path would overrun sun_path's 108 bytes.
    let run = tempfile::tempdir().expect("run dir");
    let socket = run.path().join("tm.sock");
    let mut fixture = spawn_fixture(tmp.path(), BINARY, &socket);

    // SAFETY: env_test_lock is held for the whole window the variable is set.
    unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, &data) };
    let mut desc =
        descriptor_for(Some(&data.clone().into_os_string()), socket.clone()).expect("descriptor");
    // #11085: restart checks the binary before its stop; the stop error
    // below returns before any start.
    desc.install_dir = tmp.path().join("bin");
    desc.stop_grace = Duration::from_millis(300);
    desc.kill_wait = Duration::from_millis(500);
    // #11085: the scan sees the fixture (the serving pid it leaves out) only.
    let procs = own_procs(&run.path().join("proc"), &[fixture.id()]);
    let result = restart_with(&desc, &DropKill, &procs).await;
    // SAFETY: as above.
    unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) };

    let alive = fixture.try_wait().expect("try_wait").is_none();
    let _ = fixture.kill();
    let _ = fixture.wait();
    let err = result.expect_err("a survivor must fail the restart");
    let shown = format!("{err:#}");
    assert!(shown.contains(&fixture.id().to_string()), "{shown}");
    assert!(shown.contains("survived SIGKILL"), "{shown}");
    assert!(alive, "the fixture survived, as the seam made it");
}

// AC11 / P-b: a daemon that is not running is a `stop` error and a `restart`
// that reaches the start. #11085: the fixture binary is installed, so restart
// passes its pre-stop binary check; the spawned fixture finds no socket
// variable in its cleared environment and exits, so only a start that ran
// fails this way.
#[tokio::test(flavor = "multi_thread")]
async fn restart_of_a_daemon_that_is_not_running_goes_on_to_start() {
    let _env = crate::commands::env_test_lock().lock().await;
    let exe_dir = std::env::current_exe().expect("exe");
    let tmp = tempfile::tempdir_in(exe_dir.parent().expect("exe dir")).expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    // A short directory: the exe-dir path would overrun sun_path's 108 bytes.
    let run = tempfile::tempdir().expect("run dir");
    install_fixture(tmp.path(), BINARY);
    // SAFETY: env_test_lock is held for the whole window the variable is set.
    unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, &data) };
    let mut desc = descriptor_for(
        Some(&data.clone().into_os_string()),
        run.path().join("absent.sock"),
    )
    .expect("descriptor");
    desc.install_dir = tmp.path().join("bin");
    desc.spawn_args = [
        "commands::linux_lifecycle::tests::lifecycle_fixture_entry",
        "--exact",
    ]
    .map(String::from)
    .to_vec();
    // #11085: nothing of this test runs, so the scan sees an empty table.
    let procs = own_procs(&run.path().join("proc"), &[]);
    let stop = stop_report(lc::stop_with_table(&desc, &KernelDelivery, &procs).await);
    let restart = restart_with(&desc, &KernelDelivery, &procs).await;
    // SAFETY: as above.
    unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) };

    let stop = stop.expect_err("stop on a stopped daemon exits 1");
    assert!(format!("{stop:#}").contains("No daemon running"));
    let restart = format!("{:#}", restart.expect_err("the fixture exits at boot"));
    assert!(
        restart.contains("exited before serving the socket"),
        "restart reached start: {restart}"
    );
}
