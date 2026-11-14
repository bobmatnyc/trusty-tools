//! Tests for the console's Linux lifecycle verbs (#11085).
//!
//! The module's stop, identity and spawn rules are tested in trusty-common's
//! `tests/linux_lifecycle.rs`. These pin what this crate supplies: the
//! descriptor, the environment, the `http_addr` fallback (AC10) and the exit
//! status of each verb, with real fixture processes for the fallback.
//! No test here mutates the process environment.

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use trusty_common::linux_lifecycle::{PinnedProcess, Signal};

use super::addr_pid::{AddrLookup, Listener, lookup, parse_listener};
use super::env::EXCLUDED;
use super::*;

/// The fixture's data dir; set only in the fixture child.
const FIXTURE_DIR_ENV: &str = "CONSOLE_LC_FIXTURE_DIR";
/// Set in the fixture child to skip writing `http_addr`.
const FIXTURE_NO_ADDR_ENV: &str = "CONSOLE_LC_FIXTURE_NO_ADDR";
/// Set in the fixture child to ignore SIGTERM.
const FIXTURE_IGNORE_TERM_ENV: &str = "CONSOLE_LC_FIXTURE_IGNORE_TERM";

fn target_in(dir: &Path, override_value: Option<&OsString>) -> Target {
    let mut target = target_for(
        override_value,
        dir.join("trusty-console.sock"),
        dir.join("http_addr"),
    )
    .expect("target");
    // An install dir with no binary: a start that ran fails BinaryMissing.
    target.desc.install_dir = dir.join("empty-install");
    target
}

#[test]
fn descriptor_names_serve_args_socket_signal_and_instance() {
    let data = OsString::from("/tmp/console-lc-data");
    let target = target_for(
        Some(&data),
        PathBuf::from("/tmp/c/trusty-console.sock"),
        PathBuf::from("/tmp/c/http_addr"),
    )
    .expect("target");
    let desc = &target.desc;
    assert_eq!(desc.binary, "trusty-console");
    assert_eq!(desc.socket, PathBuf::from("/tmp/c/trusty-console.sock"));
    assert_eq!(desc.stopped_signal, StoppedSignal::SocketRefuses);
    // A `port`/`config` CLI process lacks `serve`, so it never passes identity.
    assert_eq!(desc.liveness_args, ["serve"]);
    assert_eq!(desc.spawn_args, desc.liveness_args);
    assert_eq!(
        desc.instance_env,
        vec![(DATA_DIR_OVERRIDE_ENV.to_string(), Some(data))]
    );
    assert!(DaemonDescriptor::BASE_ENV.contains(&DATA_DIR_OVERRIDE_ENV));
    assert_eq!(desc.working_dir, PathBuf::from("/"));
    assert_eq!(target.addr_file, PathBuf::from("/tmp/c/http_addr"));
}

// #11085: a name missing here is one a `start`-spawned console silently drops.
#[test]
fn env_passthrough_carries_security_and_data_names_and_no_credential() {
    let target = target_in(Path::new("/tmp/c"), None);
    let passed = |name: &str| target.desc.env_passthrough.iter().any(|n| n == name);
    for name in [
        // Tailnet exposure of the console.
        "TRUSTY_CONSOLE_BIND",
        // Which daemon sockets the console dials.
        "TRUSTY_MPM_SOCKET",
        "TRUSTY_MEMORY_SOCKET",
        "TRUSTY_SEARCH_SOCKET",
        "TRUSTY_DATA_DIR",
        // The secrets bridge's registry root and external vault.
        "TRUSTY_MPM_ROOT",
        "TRUSTY_SECRETS_EXTERNAL",
        "TRUSTY_ANALYZE_EXTERNAL",
        "HTTPS_PROXY",
        "SSL_CERT_FILE",
        "CARGO_HOME",
        "RUST_LOG",
    ] {
        assert!(passed(name), "{name} must reach the console");
    }
    for name in &target.desc.env_passthrough {
        // By name segment: `TRUSTY_SECRETS_EXTERNAL` is a vault-location flag,
        // while `GITHUB_WEBHOOK_SECRET` and `*_TOKEN_FILE` are credentials.
        let upper = name.to_ascii_uppercase();
        let credential = upper.split('_').any(|segment| {
            ["KEY", "APIKEY", "TOKEN", "SECRET", "PASSWORD", "PASS"].contains(&segment)
        });
        assert!(!credential, "{name} looks like a credential");
    }
    for (name, _) in EXCLUDED {
        assert!(!passed(name), "{name} is excluded");
    }
    assert!(!passed(trusty_common::console_token::TOKEN_FILE_ENV));
}

#[test]
fn descriptor_refuses_blank_and_non_utf8_override() {
    use std::os::unix::ffi::OsStringExt as _;
    let dir = Path::new("/tmp/c");
    for blank in ["", " ", "\t"] {
        let err = target_for(
            Some(&OsString::from(blank)),
            dir.join("s.sock"),
            dir.join("http_addr"),
        )
        .expect_err("a blank override must be refused");
        assert!(format!("{err:#}").contains("blank"), "{err:#}");
    }
    let raw = OsString::from_vec(b"/tmp/c-\xff".to_vec());
    let err = target_for(Some(&raw), dir.join("s.sock"), dir.join("http_addr"))
        .expect_err("a non-UTF-8 override must be refused");
    assert!(format!("{err:#}").contains("UTF-8"), "{err:#}");
}

#[test]
fn start_and_restart_refuse_while_a_token_file_is_set() {
    let err = refuse_dropped_token_file(Some(OsString::from("/home/u/.console-token")))
        .expect_err("a set token file must be refused");
    assert!(format!("{err:#}").contains("TRUSTY_CONSOLE_TOKEN_FILE"));
    assert!(refuse_dropped_token_file(None).is_ok());
    assert!(refuse_dropped_token_file(Some(OsString::new())).is_ok());
}

#[test]
fn stop_report_maps_each_outcome_to_its_exit_status() {
    let stopped = StopOutcome::Stopped {
        pid: 7,
        forced: false,
    };
    assert!(stop_report(Ok((stopped, Route::HttpAddr(7)))).expect("stopped"));
    let none = stop_report(Ok((StopOutcome::NotRunning, Route::Socket))).expect_err("exit 1");
    assert!(format!("{none:#}").contains("No trusty-console running"));
    let failures = [
        LifecycleError::Survivor { pid: 4242 },
        LifecycleError::NotThisInstance {
            pid: 4242,
            binary: BINARY.into(),
        },
        LifecycleError::IdentityMismatch {
            pid: 4242,
            expected: BINARY.into(),
            found: "bash".into(),
        },
        LifecycleError::RestartBlocked {
            serving: vec![4242],
            binary: BINARY.into(),
            others: vec![4243],
        },
    ];
    for failure in failures {
        let shown = failure.to_string();
        let err = stop_report(Err(failure.into())).expect_err("every error fails");
        assert!(format!("{err:#}").contains(&shown), "{err:#}");
        assert!(format!("{err:#}").contains("4242"), "{err:#}");
    }
}

// Fail-open guard: AlreadyRunning is the one error turned into success.
#[test]
fn start_report_treats_only_already_running_as_success() {
    let already = LifecycleError::AlreadyRunning {
        socket: PathBuf::from("/tmp/c/trusty-console.sock"),
        pid: Some(9),
    };
    assert!(start_report(Err(already)).is_ok());
    let failures = [
        LifecycleError::ChildExited { pid: 5 },
        LifecycleError::RunningUnserved {
            socket: PathBuf::from("/tmp/c/trusty-console.sock"),
            binary: BINARY.into(),
            pids: vec![11],
        },
        LifecycleError::NotThisInstance {
            pid: 5,
            binary: BINARY.into(),
        },
    ];
    for failure in failures {
        let shown = failure.to_string();
        assert!(start_report(Err(failure)).is_err(), "{shown} must fail");
    }
}

#[test]
fn proc_net_tcp_lines_parse_to_listeners() {
    let v4 = "   0: 0100007F:1E6C 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 424242 1 0000000000000000 100 0 0 10 0";
    assert_eq!(
        parse_listener(v4),
        Some(Listener {
            addr: "127.0.0.1:7788".parse().expect("addr"),
            uid: 1000,
            inode: 424242,
        })
    );
    // ESTABLISHED (01) is not a listener.
    assert_eq!(parse_listener(&v4.replace(" 0A ", " 01 ")), None);
    let v6 = "   0: 00000000000000000000000001000000:1E6C 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 99 1 0000000000000000 100 0 0 10 0";
    let parsed = parse_listener(v6).expect("v6 listener");
    assert_eq!(
        parsed.addr,
        "[::1]:7788".parse::<SocketAddr>().expect("addr")
    );
    assert_eq!(parse_listener("garbage"), None);
}

#[test]
fn addr_lookup_finds_this_process_and_nothing_on_a_dead_port() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("http_addr");
    assert_eq!(lookup(&file).expect("absent"), AddrLookup::NoAddrFile);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::fs::write(&file, addr.to_string()).expect("write");
    assert_eq!(
        lookup(&file).expect("live"),
        AddrLookup::Pid(std::process::id(), addr)
    );
    drop(listener);
    assert_eq!(lookup(&file).expect("dead"), AddrLookup::NoListener(addr));
}

#[test]
fn addr_lookup_refuses_an_unparseable_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("http_addr");
    std::fs::write(&file, "not-an-address").expect("write");
    assert!(lookup(&file).is_err());
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

/// The fixture: when [`FIXTURE_DIR_ENV`] is set, act as a console whose UDS
/// bind failed — try the socket (the parent planted a file there, so the bind
/// fails), listen on TCP, write `http_addr`, and run until the dir is gone.
#[test]
fn lifecycle_fixture_entry() {
    let Some(dir) = std::env::var_os(FIXTURE_DIR_ENV).map(PathBuf::from) else {
        return;
    };
    if std::env::var_os(FIXTURE_IGNORE_TERM_ENV).is_some() {
        // SAFETY: installing SIG_IGN for SIGTERM has no preconditions.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    let socket = dir.join("trusty-console.sock");
    assert!(
        std::os::unix::net::UnixListener::bind(&socket).is_err(),
        "the socket path must be unbindable"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture bind");
    if std::env::var_os(FIXTURE_NO_ADDR_ENV).is_none() {
        let addr = listener.local_addr().expect("addr").to_string();
        std::fs::write(dir.join("http_addr.tmp"), addr).expect("write addr");
        std::fs::rename(dir.join("http_addr.tmp"), dir.join("http_addr")).expect("rename");
    } else {
        std::fs::write(dir.join("ready"), b"").expect("ready marker");
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while dir.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::process::exit(0);
}

/// Spawn the fixture as `<root>/bin/trusty-console serve …` with
/// `TRUSTY_DATA_DIR_OVERRIDE=<override>`, its data dir `dir`, and a regular
/// file planted at the socket path so its UDS bind fails (AC10).
fn spawn_fixture(root: &Path, dir: &Path, override_value: &Path, extra: &[&str]) -> Child {
    std::fs::create_dir_all(dir).expect("data dir");
    std::fs::write(dir.join("trusty-console.sock"), b"not a socket").expect("block the socket");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let link = bin.join(BINARY);
    let exe = std::env::current_exe().expect("current exe");
    std::fs::hard_link(&exe, &link)
        .or_else(|_| std::fs::copy(&exe, &link).map(|_| ()))
        .expect("link the fixture");
    let mut cmd = Command::new(&link);
    // libtest takes `serve` as a second exact filter, which matches no test.
    cmd.args([
        "linux_lifecycle::tests::lifecycle_fixture_entry",
        "serve",
        "--exact",
        "--nocapture",
    ])
    .env(FIXTURE_DIR_ENV, dir)
    .env(DATA_DIR_OVERRIDE_ENV, override_value)
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    for name in extra {
        cmd.env(name, "1");
    }
    let child = cmd.spawn().expect("spawn fixture");
    let marker = if extra.contains(&FIXTURE_NO_ADDR_ENV) {
        dir.join("ready")
    } else {
        dir.join("http_addr")
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "fixture never came up");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// A short root (sun_path is 108 bytes) with `data/` as the override.
fn fixture_root() -> (Roots, PathBuf, PathBuf) {
    let exe = std::env::current_exe().expect("exe");
    // Beside the test binary, so the hard link stays on one filesystem.
    let bin = tempfile::tempdir_in(exe.parent().expect("exe dir")).expect("tempdir");
    // A short data dir: the exe-dir path would overrun sun_path's 108 bytes.
    let short = tempfile::tempdir().expect("short tempdir");
    let data = short.path().join("data");
    let dir = data.join(BINARY);
    (Roots { bin, _short: short }, data, dir)
}

/// The fixture's two temp dirs, removed together on drop.
struct Roots {
    bin: tempfile::TempDir,
    _short: tempfile::TempDir,
}

impl Roots {
    fn path(&self) -> &Path {
        self.bin.path()
    }
}

// AC10: the socket bind failed, so only `http_addr` finds the console.
#[tokio::test(flavor = "multi_thread")]
async fn stop_finds_the_console_through_http_addr_when_its_socket_bind_failed() {
    let (root, data, dir) = fixture_root();
    let mut fixture = spawn_fixture(root.path(), &dir, &data, &[]);
    let target = target_in(&dir, Some(&data.clone().into_os_string()));

    let result = stop_with(&target, &KernelDelivery).await;
    let exited = fixture.try_wait().expect("try_wait").is_some();
    let _ = fixture.kill();
    let _ = fixture.wait();
    let (outcome, route) = result.expect("the fallback must stop the console");
    assert_eq!(
        outcome,
        StopOutcome::Stopped {
            pid: fixture.id(),
            forced: false
        }
    );
    assert_eq!(
        route,
        Route::HttpAddr(fixture.id()),
        "the fallback identity is used"
    );
    assert!(exited, "the console exited");
}

// Fail-open guard: RunningUnserved with no http_addr stays an error, unsignalled.
#[tokio::test(flavor = "multi_thread")]
async fn stop_with_an_unserved_console_and_no_http_addr_signals_nothing() {
    let (root, data, dir) = fixture_root();
    let mut fixture = spawn_fixture(root.path(), &dir, &data, &[FIXTURE_NO_ADDR_ENV]);
    let target = target_in(&dir, Some(&data.clone().into_os_string()));

    let result = stop_with(&target, &KernelDelivery).await;
    let alive = fixture.try_wait().expect("try_wait").is_none();
    let _ = fixture.kill();
    let _ = fixture.wait();
    let err = result.expect_err("an unserved console with no http_addr is an error");
    assert!(
        format!("{err:#}").contains(&fixture.id().to_string()),
        "{err:#}"
    );
    assert!(alive, "nothing was signalled");
}

// Fail-open guard: NotRunning is kept only when http_addr leads to no
// listener; a live listener that is not this console is an error.
#[tokio::test(flavor = "multi_thread")]
async fn stop_on_not_running_with_a_foreign_listener_in_http_addr_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let override_value = dir.path().join("data").into_os_string();
    let target = target_in(dir.path(), Some(&override_value));
    // This test process holds the listener; its exe is not trusty-console.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::fs::write(&target.addr_file, addr.to_string()).expect("write");

    let err = stop_with(&target, &KernelDelivery)
        .await
        .expect_err("a foreign listener must not read as not running");
    assert!(format!("{err:#}").contains(BINARY), "{err:#}");
    drop(listener);
    let (outcome, _) = stop_with(&target, &KernelDelivery)
        .await
        .expect("a dead address is not running");
    assert_eq!(outcome, StopOutcome::NotRunning);
}

// AC2: a survivor found through http_addr fails the restart and starts nothing.
#[tokio::test(flavor = "multi_thread")]
async fn restart_through_http_addr_reports_a_survivor_and_starts_nothing() {
    let (root, data, dir) = fixture_root();
    let mut fixture = spawn_fixture(root.path(), &dir, &data, &[FIXTURE_IGNORE_TERM_ENV]);
    let mut target = target_in(&dir, Some(&data.clone().into_os_string()));
    target.desc.stop_grace = Duration::from_millis(300);
    target.desc.kill_wait = Duration::from_millis(500);

    let result = restart_report(restart_with(&target, &DropKill).await);
    let alive = fixture.try_wait().expect("try_wait").is_none();
    let _ = fixture.kill();
    let _ = fixture.wait();
    let shown = format!("{:#}", result.expect_err("a survivor fails the restart"));
    assert!(shown.contains(&fixture.id().to_string()), "{shown}");
    assert!(shown.contains("survived SIGKILL"), "{shown}");
    assert!(!shown.contains("not installed"), "start ran: {shown}");
    assert!(alive, "the fixture survived, as the seam made it");
}

// AC11 / P-b: `stop` on a stopped console exits 1; `restart` reaches start.
#[tokio::test(flavor = "multi_thread")]
async fn stop_on_nothing_running_exits_1_and_restart_reaches_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let override_value = dir.path().join("data").into_os_string();
    let target = target_in(dir.path(), Some(&override_value));

    let stop = stop_report(stop_with(&target, &KernelDelivery).await).expect_err("exit 1");
    assert!(format!("{stop:#}").contains("No trusty-console running"));
    let restart = restart_report(restart_with(&target, &KernelDelivery).await);
    let shown = format!("{:#}", restart.expect_err("no binary to start"));
    assert!(
        shown.contains("not installed"),
        "restart reached start: {shown}"
    );
}
