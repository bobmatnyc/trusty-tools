//! Tests for the Linux lifecycle verbs (#11085).
//!
//! The stop, identity and spawn rules are tested against real processes in
//! trusty-common's `tests/linux_lifecycle.rs`; these tests pin what this crate
//! supplies — the descriptor, the passthrough set and each verb's exit status —
//! plus real-process runs through the module's signal seam (AC2, AC6, AC8, AC9).

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use trusty_common::linux_lifecycle::{
    LifecycleError, PinnedProcess, ProcessTable, Signal, SignalDelivery, StopOutcome, StoppedSignal,
};

use super::*;

/// Fixture variables; set only in the fixture child.
const FIX_SOCKET: &str = "TS_LC_FIXTURE_SOCKET";
const FIX_LOCK: &str = "TS_LC_FIXTURE_LOCK";
const FIX_DIR: &str = "TS_LC_FIXTURE_DIR";
const FIX_IGNORE_TERM: &str = "TS_LC_FIXTURE_IGNORE_TERM";
/// Argv prefix naming the fixture's socket, for a fixture the module spawns
/// with only the descriptor's environment.
const FIX_SOCKET_ARG: &str = "ts-lc-fixture-socket=";

#[test]
#[serial_test::parallel]
fn descriptor_resolves_the_data_dir_the_same_way_for_every_verb() {
    let flag_dir = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("tempdir");
    let flag = flag_dir.path();
    // The flag wins over TRUSTY_DATA_DIR, and names the socket, lock and args.
    let desc =
        descriptor_for(Some(flag), Some(env_dir.path().into()), None, None).expect("descriptor");
    assert_eq!(desc.binary, "trusty-search");
    assert_eq!(desc.socket, flag.join("trusty-search.sock"));
    assert_eq!(
        desc.stopped_signal,
        StoppedSignal::LockFree(flag.join("daemon.lock"))
    );
    let dir = flag.to_str().expect("utf-8");
    assert_eq!(
        desc.liveness_args,
        ["start", "--foreground", "--data-dir", dir]
    );
    assert_eq!(desc.spawn_args, desc.liveness_args);
    assert!(desc
        .instance_env
        .contains(&(DATA_DIR_ENV.to_string(), Some(flag.as_os_str().to_owned()))));
    assert_eq!(desc.working_dir, PathBuf::from("/"));
    // #11085 AC6: the path flag names the resolved dir, for every resolution.
    let path_arg = |dir: &Path| vec![("--data-dir".to_string(), dir.to_path_buf())];
    assert_eq!(desc.instance_path_args, path_arg(flag));
    // Env only: the same dir, and no `--data-dir` the daemon might not carry.
    let desc = descriptor_for(None, Some(env_dir.path().into()), None, None).expect("env");
    assert_eq!(desc.socket, env_dir.path().join("trusty-search.sock"));
    assert_eq!(desc.liveness_args, ["start", "--foreground"]);
    assert_eq!(desc.instance_path_args, path_arg(env_dir.path()));
    assert!(desc.instance_env.contains(&(
        DATA_DIR_ENV.to_string(),
        Some(env_dir.path().as_os_str().to_owned())
    )));
    // The default instance: the resolved default data dir, never empty.
    let default_dir = crate::service::daemon::resolve_daemon_dir(None).expect("default dir");
    let desc = descriptor_for(None, None, None, None).expect("default");
    assert_eq!(desc.instance_path_args, path_arg(&default_dir));
    // An explicit socket wins over the data dir's.
    let sock = env_dir.path().join("other.sock");
    let desc = descriptor_for(None, Some(env_dir.path().into()), None, Some(&sock)).expect("s");
    assert_eq!(desc.socket, sock);
}

#[test]
#[serial_test::parallel]
fn descriptor_refuses_a_blank_override_and_a_relative_data_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    for blank in ["", " ", "\t"] {
        let err = descriptor_for(Some(dir.path()), None, Some(&OsString::from(blank)), None)
            .expect_err("a blank override must be refused");
        assert!(format!("{err:#}").contains("blank"), "{err:#}");
    }
    let err = descriptor_for(Some(Path::new("rel/dir")), None, None, None)
        .expect_err("a relative data dir must be refused");
    assert!(format!("{err:#}").contains("absolute"), "{err:#}");
}

// The security- and data-affecting variables reach the daemon; no credential does.
#[test]
fn env_passthrough_carries_every_daemon_setting_and_no_credential() {
    let carried =
        |name: &str| ENV_PASSTHROUGH.contains(&name) || DaemonDescriptor::BASE_ENV.contains(&name);
    for name in [
        "TRUSTY_DATA_DIR",
        "TRUSTY_DATA_DIR_OVERRIDE",
        "TRUSTY_SANDBOX",
        "TRUSTY_NO_AUTO_DISCOVER",
        "TRUSTY_NO_KG",
        "TRUSTY_VECTOR_QUANT",
        "TRUSTY_DISABLE_MIGRATIONS",
        "TRUSTY_EMBEDDER",
        "TRUSTY_EMBEDDER_MODEL",
        "TRUSTY_DEVICE",
        "TRUSTY_EMBEDDERD_BIN",
        "TRUSTY_EMBEDDERD_PY_BIN",
        "TRUSTY_UV_BIN",
        "FASTEMBED_CACHE_DIR",
        "HF_HOME",
        "OLLAMA_HOST",
        "LD_LIBRARY_PATH",
        "ORT_DYLIB_PATH",
        "TRUSTY_MEMORY_LIMIT_MB",
        "TRUSTY_MAX_RESIDENT_INDEXES",
        "TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS",
        "TRUSTY_TERMINATION_GRACE_SECS",
        "SSL_CERT_FILE",
        "HTTPS_PROXY",
        "DBUS_SESSION_BUS_ADDRESS",
        "RUST_LOG",
        // #11085: the boot-time update check's opt-outs (trusty_common::update).
        "TRUSTY_NO_UPDATE_CHECK",
        "CI",
    ] {
        assert!(carried(name), "{name} must reach the daemon");
    }
    let credential = ["TOKEN", "KEY", "SECRET", "PASSWORD", "CREDENTIAL", "AUTH"];
    for name in ENV_PASSTHROUGH {
        assert!(
            !credential.iter().any(|c| name.contains(c)),
            "{name} is credential-shaped and must not be passed"
        );
    }
    assert!(!ENV_PASSTHROUGH.contains(&"TRUSTY_EXIT_WITH_PARENT"));
}

// Drift guard: a new `env::var` read in this crate must be passed or excluded.
// #11085: it sees only literal `var("X")` / `var_os("X")` reads in this crate's
// `src/`. A read through a constant, a helper, or trusty-common escapes it;
// `env_passthrough_carries_every_daemon_setting_and_no_credential` pins those.
#[test]
fn env_passthrough_covers_every_variable_this_crate_reads() {
    let mut missing = std::collections::BTreeSet::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("entry").path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if !name.ends_with(".rs") || name.contains("tests") || name.contains("_test") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read file");
            let code = text.split("#[cfg(test)]").next().unwrap_or("");
            for marker in ["var(\"", "var_os(\""] {
                for chunk in code.split(marker).skip(1) {
                    let var = chunk.split('"').next().unwrap_or("");
                    let known = ENV_PASSTHROUGH.contains(&var)
                        || DaemonDescriptor::BASE_ENV.contains(&var)
                        || env::EXCLUDED.iter().any(|(n, _)| *n == var);
                    if !var.is_empty() && !known {
                        missing.insert(format!("{var} ({name})"));
                    }
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "not passed and not excluded: {missing:?}"
    );
}

#[test]
fn stop_report_maps_each_outcome_to_its_exit_status() {
    let stopped = StopOutcome::Stopped {
        pid: 7,
        forced: true,
    };
    assert!(stop_report(Ok(stopped)).expect("stopped is success"));
    // P-b: `stop` keeps exit 1 when nothing runs.
    let none = stop_report(Ok(StopOutcome::NotRunning)).expect_err("nothing to stop");
    assert!(format!("{none:#}").contains("No daemon running"));
    // AC3: a survivor is an error naming its pid.
    let survivor = stop_report(Err(LifecycleError::Survivor { pid: 4242 }))
        .expect_err("a survivor is an error");
    assert!(format!("{survivor:#}").contains("4242"), "{survivor:#}");
}

// AC5: no module error, AlreadyRunning included, is a successful start.
#[test]
fn start_report_fails_every_module_error() {
    let sock = PathBuf::from("/tmp/ts-lc/trusty-search.sock");
    let failures = [
        LifecycleError::AlreadyRunning {
            socket: sock.clone(),
            pid: Some(9),
        },
        LifecycleError::NotReady {
            pid: 5,
            timeout: Duration::from_secs(1),
            killed: true,
        },
        LifecycleError::ChildExited { pid: 5 },
        LifecycleError::BinaryMissing {
            path: PathBuf::from("/nowhere/trusty-search"),
        },
        LifecycleError::RunningUnserved {
            socket: sock,
            binary: BINARY.into(),
            pids: vec![11],
        },
    ];
    for failure in failures {
        let shown = failure.to_string();
        assert!(start_report(Err(failure)).is_err(), "{shown} must fail");
    }
}

// Fail-open guard: only exit 0 reads as active; a missing program is inactive.
#[test]
fn systemd_probe_reads_only_exit_zero_as_active() {
    assert_eq!(
        systemd_unit_active(Path::new("/bin/true")),
        Some(UnitScope::System)
    );
    assert_eq!(systemd_unit_active(Path::new("/bin/false")), None);
    assert_eq!(
        systemd_unit_active(Path::new("/nonexistent/systemctl")),
        None
    );
}

/// Counts every signal and delivers none but SIGTERM when `term` is set.
struct Recorder {
    sent: AtomicUsize,
    deliver_term: bool,
}

impl SignalDelivery for Recorder {
    fn deliver(&self, target: &PinnedProcess, signal: Signal) -> io::Result<()> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        match signal {
            Signal::Term if self.deliver_term => target.send_signal(signal),
            _ => Ok(()),
        }
    }
}

/// The fixture: when [`FIX_DIR`] is set, optionally bind [`FIX_SOCKET`], hold
/// the flock on [`FIX_LOCK`], ignore SIGTERM, and run until the dir is gone.
#[test]
fn lifecycle_fixture_entry() {
    let Some(dir) = std::env::var_os(FIX_DIR).map(PathBuf::from) else {
        return;
    };
    if std::env::var_os(FIX_IGNORE_TERM).is_some() {
        // SAFETY: installing SIG_IGN for SIGTERM has no preconditions.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    let _lock = std::env::var_os(FIX_LOCK).map(|p| {
        let f = std::fs::File::create(p).expect("lock file");
        f.lock().expect("flock");
        f
    });
    let from_argv = std::env::args_os().find_map(|a| {
        let a = a.into_string().ok()?;
        a.strip_prefix(FIX_SOCKET_ARG).map(OsString::from)
    });
    let listener = std::env::var_os(FIX_SOCKET).or(from_argv).map(|s| {
        // A stopped fixture leaves its socket file; a restart binds over it.
        let _ = std::fs::remove_file(&s);
        let l = std::os::unix::net::UnixListener::bind(s).expect("fixture bind");
        l.set_nonblocking(true).expect("nonblocking");
        l
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    while dir.exists() && Instant::now() < deadline {
        if let Some(l) = &listener {
            let _ = l.accept();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::process::exit(0);
}

/// Spawn the fixture as `<links>/bin/trusty-search` with `argv` after `--`.
///
/// The test binary is hard-linked beside itself, so `/proc/<pid>/exe` names
/// `trusty-search`. `env` is set on the child; `TRUSTY_DATA_DIR` is removed
/// unless `env` names it. Waits until `socket`, when given, accepts.
fn spawn_fixture(
    links: &Path,
    argv: &[&str],
    env: &[(&str, &OsString)],
    socket: Option<&Path>,
) -> Child {
    let link = install_fixture(links);
    let mut cmd = Command::new(&link);
    cmd.args([
        "commands::linux_lifecycle::tests::lifecycle_fixture_entry",
        "--exact",
        "--",
    ])
    .args(argv)
    .env_remove(DATA_DIR_ENV)
    .env(FIX_DIR, links)
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn fixture");
    if let Some(socket) = socket {
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::os::unix::net::UnixStream::connect(socket).is_err() {
            assert!(Instant::now() < deadline, "fixture never served");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    child
}

/// Hard-link the test binary as `<links>/bin/trusty-search`; returns the link.
fn install_fixture(links: &Path) -> PathBuf {
    let bin = links.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let link = bin.join(BINARY);
    if !link.exists() {
        let exe = std::env::current_exe().expect("current exe");
        std::fs::hard_link(&exe, &link)
            .or_else(|_| std::fs::copy(&exe, &link).map(|_| ()))
            .expect("link the fixture");
    }
    link
}

/// A dir beside the test binary, for the hard link (same filesystem).
fn links_dir() -> tempfile::TempDir {
    let exe = std::env::current_exe().expect("exe");
    tempfile::tempdir_in(exe.parent().expect("exe dir")).expect("links dir")
}

/// Wait until `pid` has exec'd: its argv shows the daemon's `--foreground`,
/// so its `/proc` argv and environ are the fixture's, not the parent's.
fn wait_exec(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !std::fs::read(format!("/proc/{pid}/cmdline"))
        .is_ok_and(|c| c.split(|b| *b == 0).any(|a| a == b"--foreground"))
    {
        assert!(Instant::now() < deadline, "fixture argv never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Spawn a sibling as `<links>/sh-bin/trusty-search -c <loop> <argv>`.
///
/// Why: the module reads path flags only up to a `--` argument, and the
/// libtest fixture puts every daemon argument after one. A copy of `/bin/sh`
/// named `trusty-search` carries `argv` with no `--`, as a real daemon does.
/// It runs until [`FIX_DIR`] is gone, at most 60 s. `TRUSTY_DATA_DIR` is
/// removed; `env` is set.
fn spawn_sh_sibling(links: &Path, argv: &[&str], env: &[(&str, &OsString)]) -> Child {
    let bin = links.join("sh-bin");
    std::fs::create_dir_all(&bin).expect("sh bin dir");
    let sh = bin.join(BINARY);
    std::fs::copy("/bin/sh", &sh).expect("copy /bin/sh");
    let script = format!(
        "i=0; while [ -d \"${FIX_DIR}\" ] && [ $i -lt 1200 ]; do sleep 0.05; i=$((i+1)); done"
    );
    let mut cmd = Command::new(&sh);
    cmd.arg("-c")
        .arg(script)
        .args(argv)
        .env_remove(DATA_DIR_ENV)
        .env(FIX_DIR, links)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    // A parallel test's fork can hold the copy's write fd until its exec.
    let mut tries = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            other => break other.expect("spawn sh sibling"),
        }
    }
}

/// Show the scan `pid`'s real `/proc` entry in the table under `table`.
///
/// Why: #11085 — a test that scans the live `/proc` changes result with the
/// host's processes; the table holds only the test's own.
fn link_proc(table: &Path, pid: u32) {
    std::os::unix::fs::symlink(format!("/proc/{pid}"), table.join(pid.to_string()))
        .expect("link the proc entry");
}

/// A fake entry for a `trusty-search start --foreground` daemon of another
/// uid whose `environ` cannot be read: the host process shape that failed
/// AC6 with `InstanceUnreadable` (#11085). The scan must skip it by its uid.
fn foreign_daemon_entry(table: &Path, pid: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = table.join(pid.to_string());
    std::fs::create_dir_all(&dir).expect("foreign entry");
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() }.wrapping_add(1);
    let status =
        format!("Name:\t{BINARY}\nState:\tS (sleeping)\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n");
    std::fs::write(dir.join("status"), status).expect("status");
    let cmdline = format!("/usr/local/bin/{BINARY}\0start\0--foreground\0");
    std::fs::write(dir.join("cmdline"), cmdline).expect("cmdline");
    std::os::unix::fs::symlink(format!("/usr/local/bin/{BINARY}"), dir.join("exe")).expect("exe");
    let environ = dir.join("environ");
    std::fs::write(&environ, b"").expect("environ");
    std::fs::set_permissions(&environ, std::fs::Permissions::from_mode(0o000)).expect("chmod");
}

fn reap(mut child: Child) -> bool {
    let alive = child.try_wait().expect("try_wait").is_none();
    let _ = child.kill();
    let _ = child.wait();
    alive
}

// AC6: a daemon started with `--data-dir X` is stopped by `stop --data-dir X`
// with TRUSTY_DATA_DIR unset, through the socket peer and the lock signal.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn stop_with_the_data_dir_flag_stops_the_daemon_serving_that_dir() {
    let links = links_dir();
    let data = tempfile::tempdir().expect("data dir");
    let desc = descriptor_for(Some(data.path()), None, None, None).expect("descriptor");
    let lock = data.path().join("daemon.lock").into_os_string();
    let sock = desc.socket.clone().into_os_string();
    let dir = data.path().to_str().expect("utf-8");
    let fixture = spawn_fixture(
        links.path(),
        &["start", "--foreground", "--data-dir", dir],
        &[(FIX_SOCKET, &sock), (FIX_LOCK, &lock)],
        Some(&desc.socket),
    );
    let pid = fixture.id();
    let outcome = lc::stop(&desc).await;
    let alive = reap(fixture);
    assert_eq!(
        outcome.expect("stop"),
        StopOutcome::Stopped { pid, forced: false }
    );
    assert!(!alive, "the fixture exited on SIGTERM");
}

// AC8: `daemon.lock` naming a foreign pid draws no signal; stop exits 1.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn stop_never_signals_a_foreign_pid_named_in_daemon_lock() {
    let data = tempfile::tempdir().expect("data dir");
    let mut sleeper = Command::new("sleep").arg("30").spawn().expect("sleep");
    std::fs::write(data.path().join("daemon.lock"), sleeper.id().to_string()).expect("lock");
    let desc = descriptor_for(Some(data.path()), None, None, None).expect("descriptor");
    let result = stop_report(lc::stop(&desc).await);
    let alive = sleeper.try_wait().expect("try_wait").is_none();
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let err = result.expect_err("nothing serves the socket");
    assert!(format!("{err:#}").contains("No daemon running"), "{err:#}");
    assert!(alive, "the pid named in daemon.lock was signalled");
}

// AC9: with the unit active, restart sends no signal, fails, names systemctl.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn restart_refuses_while_the_systemd_unit_is_active() {
    let links = links_dir();
    let run = tempfile::tempdir().expect("run dir");
    let socket = run.path().join("ts.sock");
    let sock = socket.clone().into_os_string();
    let fixture = spawn_fixture(links.path(), &[], &[(FIX_SOCKET, &sock)], Some(&socket));
    let desc = descriptor_for(Some(run.path()), None, None, Some(&socket)).expect("descriptor");
    let recorder = Recorder {
        sent: AtomicUsize::new(0),
        deliver_term: true,
    };
    let procs = ProcessTable::system();
    let system = restart_with(&desc, &recorder, &procs, || Some(UnitScope::System)).await;
    let user = restart_with(&desc, &recorder, &procs, || Some(UnitScope::User)).await;
    let alive = reap(fixture);
    let system = format!("{:#}", system.expect_err("refused"));
    assert!(system.contains("sudo systemctl restart trusty-search.service"));
    let user = format!("{:#}", user.expect_err("refused"));
    assert!(user.contains("systemctl --user restart trusty-search.service"));
    assert_eq!(recorder.sent.load(Ordering::SeqCst), 0, "a signal was sent");
    assert!(alive, "the daemon was touched");
}

// AC2: a survivor fails the restart, names its pid, and nothing is started.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn restart_names_a_survivor_and_starts_nothing() {
    let links = links_dir();
    let run = tempfile::tempdir().expect("run dir");
    let socket = run.path().join("ts.sock");
    let sock = socket.clone().into_os_string();
    let one = OsString::from("1");
    let fixture = spawn_fixture(
        links.path(),
        &[],
        &[(FIX_SOCKET, &sock), (FIX_IGNORE_TERM, &one)],
        Some(&socket),
    );
    let pid = fixture.id();
    let mut desc = descriptor_for(Some(run.path()), None, None, Some(&socket)).expect("descriptor");
    // #11085: restart checks the binary before its stop; the stop error
    // below returns before any start.
    desc.install_dir = links.path().join("bin");
    desc.stop_grace = Duration::from_millis(300);
    desc.kill_wait = Duration::from_millis(500);
    let recorder = Recorder {
        sent: AtomicUsize::new(0),
        deliver_term: true,
    };
    // The `--data-dir` liveness arg keeps every host daemon out of the scan.
    let result = restart_with(&desc, &recorder, &ProcessTable::system(), || None).await;
    let alive = reap(fixture);
    let shown = format!(
        "{:#}",
        result.expect_err("a survivor must fail the restart")
    );
    assert!(shown.contains(&pid.to_string()), "{shown}");
    assert!(shown.contains("survived SIGKILL"), "{shown}");
    assert!(alive, "the fixture survived, as the seam made it");
}

// AC11 / P-b: a stopped daemon is a `stop` error and a `restart` that reaches
// the start. #11085: the fixture binary is installed, so restart passes its
// pre-stop binary check; the spawned fixture finds no FIX_DIR in its cleared
// environment and exits, so only a start that ran fails this way.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn restart_of_a_daemon_that_is_not_running_goes_on_to_start() {
    let links = links_dir();
    let data = tempfile::tempdir().expect("data dir");
    let mut desc = descriptor_for(Some(data.path()), None, None, None).expect("descriptor");
    desc.install_dir = install_fixture(links.path())
        .parent()
        .expect("bin dir")
        .to_path_buf();
    let fixture_args = [
        "commands::linux_lifecycle::tests::lifecycle_fixture_entry",
        "--exact",
    ];
    desc.spawn_args = fixture_args.map(String::from).to_vec();
    let stop = stop_report(lc::stop(&desc).await);
    let restart = restart_with(&desc, &KernelDelivery, &ProcessTable::system(), || None).await;
    let stop = stop.expect_err("stop on a stopped daemon exits 1");
    assert!(format!("{stop:#}").contains("No daemon running"));
    let restart = format!("{:#}", restart.expect_err("the fixture exits at boot"));
    assert!(
        restart.contains("exited before serving the socket"),
        "restart reached start: {restart}"
    );
}

// #11085 AC6: a sibling started as `start --foreground --data-dir X` WITHOUT
// TRUSTY_DATA_DIR in its environ carries every liveness arg of the default
// instance and the same environ. The descriptor's `--data-dir` path flag, set
// to the default data dir, tells it apart: `stop` of the default instance
// answers NotRunning, and `restart` of a HEALTHY default daemon stops it and
// starts a new one that serves, leaving the sibling running.
// #11085: the scan reads a table of this test's own processes plus a
// foreign-uid trusty-search daemon hiding its environ, so no host process
// can change the result.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn ac6_a_sibling_without_trusty_data_dir_no_longer_blocks_the_default_instance() {
    let links = links_dir();
    let run = tempfile::tempdir().expect("run dir");
    let procs_dir = run.path().join("proc");
    std::fs::create_dir(&procs_dir).expect("proc table dir");
    foreign_daemon_entry(&procs_dir, 4_194_401);
    let procs = ProcessTable::at(&procs_dir);
    let sibling_dir = run.path().join("sibling");
    // A live daemon's data dir exists; an unresolvable path stays undecided.
    std::fs::create_dir(&sibling_dir).expect("sibling dir");
    let sib = sibling_dir.to_str().expect("utf-8").to_string();
    let socket = run.path().join("default.sock");
    // The default-data-dir descriptor, kept off this host's live paths.
    let mut desc = descriptor_for(None, None, None, Some(&socket)).expect("desc");
    desc.stopped_signal = StoppedSignal::LockFree(run.path().join("daemon.lock"));
    // A unique override, so no live daemon on this host matches the scan.
    let key = run.path().join("override").into_os_string();
    desc.instance_env
        .retain(|(n, _)| n != trusty_common::DATA_DIR_OVERRIDE_ENV);
    desc.instance_env.push((
        trusty_common::DATA_DIR_OVERRIDE_ENV.into(),
        Some(key.clone()),
    ));
    let argv = ["start", "--foreground", "--data-dir", sib.as_str()];

    let sibling = spawn_sh_sibling(
        links.path(),
        &argv,
        &[(trusty_common::DATA_DIR_OVERRIDE_ENV, &key)],
    );
    wait_exec(sibling.id());
    link_proc(&procs_dir, sibling.id());
    let stopped = lc::stop_with_table(&desc, &KernelDelivery, &procs).await;

    // A healthy default daemon on the descriptor's socket. A spawned one gets
    // only the descriptor's environment, so the fixture keys go in
    // `instance_env` and the socket in argv; the sibling carries FIX_DIR too,
    // so it still matches every `instance_env` value.
    let sock_arg = format!("{FIX_SOCKET_ARG}{}", socket.display());
    let fixture_args = [
        "commands::linux_lifecycle::tests::lifecycle_fixture_entry",
        "--exact",
        "--",
        "start",
        "--foreground",
        sock_arg.as_str(),
    ];
    desc.instance_env
        .push((FIX_DIR.into(), Some(links.path().into())));
    desc.install_dir = links.path().join("bin");
    desc.spawn_args = fixture_args.iter().map(|a| a.to_string()).collect();
    let old = spawn_fixture(
        links.path(),
        &["start", "--foreground", sock_arg.as_str()],
        &[(trusty_common::DATA_DIR_OVERRIDE_ENV, &key)],
        Some(&socket),
    );
    let old_pid = old.id();
    link_proc(&procs_dir, old_pid);
    let restarted = restart_with(&desc, &KernelDelivery, &procs, || None).await;
    let old_alive = reap(old);
    let serving = std::os::unix::net::UnixStream::connect(&socket).is_ok();
    let cleanup = lc::stop_with_table(&desc, &KernelDelivery, &procs).await;
    let sibling_alive = reap(sibling);

    assert_eq!(
        stopped.expect("the sibling is told apart"),
        StopOutcome::NotRunning
    );
    restarted.expect("restart of the healthy default daemon");
    assert!(!old_alive, "the old default daemon was stopped");
    assert!(serving, "a new default daemon serves the socket");
    match cleanup.expect("cleanup stop") {
        StopOutcome::Stopped { pid, .. } => assert_ne!(pid, old_pid),
        other => panic!("expected the restarted daemon to stop, got {other:?}"),
    }
    assert!(sibling_alive, "the sibling was signalled");
}
