//! Tests for [`super`] — `tcode tui`'s attach-or-spawn daemon policy
//! (#4512; retransported onto the daemon's Unix socket in #6637).
//!
//! Why a sibling file: `daemon_autospawn.rs` is a production file under the
//! 500-SLOC cap (issue #610); the same `#[cfg(test)] #[path = ...]` split
//! `tui_client/engine.rs` uses keeps these cases in a test-capped file.
//! What: every branch of [`super::ensure_daemon_with`] is exercised against
//! REAL child processes and a REAL socket answering `health` — no mocked
//! internals — so the spawn, the readiness gate, and the binding check are
//! all genuinely executed. Stub children stand in for the daemon: a `sh`
//! script that records its pid and argv then sleeps (a daemon that stays
//! up), one that exits immediately (a daemon that fails to bind), and one
//! that touches a marker file (to prove a branch never spawned anything).
//!
//! Because readiness comes from the stub SOCKET rather than from the stub
//! child, `ensure_daemon_with` can return before the child has run a single
//! line. Nothing a stub writes may therefore be read directly — go through
//! [`SleepingStub::argv`] / [`SleepingStub::pid`], which poll (#6231, #5073).
//!
//! The `TCODE_DAEMON_URL` isolation the old `EnvGuard` needed is gone with the
//! env var itself. `TRUSTY_DATA_DIR_OVERRIDE` is still set, for the
//! spawned-daemon log path alone.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use super::*;

/// Serializes every case here, since they all mutate the process-global
/// `TRUSTY_DATA_DIR_OVERRIDE`. Mirrors `crate::task::mock_llm`'s convention.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// RAII guard repointing `resolve_data_dir("trusty-code")` at a fresh temp
/// directory, so the spawned-daemon log belongs to this test alone.
struct EnvGuard {
    _data_dir: tempfile::TempDir,
}

impl EnvGuard {
    fn isolated() -> Self {
        let data_dir = tempfile::tempdir().expect("data dir");
        // SAFETY: test-only env mutation, serialized by `ENV_LOCK`.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, data_dir.path());
        }
        Self {
            _data_dir: data_dir,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test-only env mutation, serialized by `ENV_LOCK`.
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
    }
}

/// A stub daemon answering `health` on a real hardened socket.
///
/// Why hand-rolled rather than the real daemon: these tests are about the
/// attach/spawn DECISION, and a real daemon would drag a router, a workstream
/// store and a log-drain scheduler in behind it. One method and one frame is
/// the whole surface `ensure_daemon_with` consults.
fn stub_daemon_socket(dir: &Path, binding: Option<serde_json::Value>) -> PathBuf {
    let socket = dir.join("tcode.sock");
    // `connect_hardened` refuses a socket whose containing directory is wider
    // than `0700`, and `tempfile` creates one at the process umask.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .expect("harden the stub socket directory");
    let listener = UnixListener::bind(&socket).expect("bind the stub socket");
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .expect("harden the stub socket");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let binding = binding.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    // A bare connect-and-close is `socket_is_serving`'s probe.
                    return;
                }
                let request: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(request) => request,
                    Err(_) => return,
                };
                let mut result = serde_json::json!({"server": "tcode", "status": "ok"});
                if let Some(binding) = binding {
                    result["binding"] = binding;
                }
                let body = format!(
                    "{}\n",
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request["id"].clone(),
                        "result": result,
                    })
                );
                let _ = reader.get_mut().write_all(body.as_bytes()).await;
                let _ = reader.get_mut().flush().await;
            });
        }
    });
    socket
}

/// The `binding` payload a daemon bound to `root` publishes.
fn binding_json(root: Option<&Path>) -> serde_json::Value {
    trusty_code::binding::ProjectBinding::resolve(root.map(Path::to_path_buf))
        .expect("must bind")
        .to_json()
}

/// Write an executable `sh` stub to `dir` and return its path. The stub
/// stands in for the `tcode` binary that would be spawned.
fn stub_binary(dir: &Path, name: &str, body: &str) -> PathBuf {
    let script = dir.join(name);
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).expect("write stub");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    script
}

/// A stub that records its own pid and the argv it was called with, then
/// sleeps — the "daemon that comes up and stays up" case.
///
/// The pid is written to disk rather than read off a `Child` handle because
/// `ensure_daemon_with` does not RETURN one: it drops the handle without
/// signalling, which is exactly the behaviour
/// [`the_tui_never_signals_the_daemon_on_exit`] has to observe from outside.
/// Reading either record straight after the call raced the write and failed
/// with `NotFound` under parallel load (#6231, #5073), so [`SleepingStub::argv`]
/// and [`SleepingStub::pid`] are the only supported readers and both poll.
struct SleepingStub {
    /// The executable `sh` script `ensure_daemon_with` is pointed at.
    path: PathBuf,
    /// Where the stub writes `"$@"` — written FIRST, before the pid.
    argv_log: PathBuf,
    /// Where the stub writes `$$`.
    pid_log: PathBuf,
}

impl SleepingStub {
    /// Write the stub into `dir`, logging argv and pid beside it.
    ///
    /// The caller must keep `dir`'s `TempDir` alive longer than the returned
    /// value: [`Drop`] reads `pid_log` out of that directory to reap the
    /// child, so the stub has to drop FIRST (declare it after the `TempDir`).
    fn new(dir: &Path) -> Self {
        let argv_log = dir.join("argv");
        let pid_log = dir.join("pid");
        let path = stub_binary(
            dir,
            "tcode-sleep",
            &format!(
                "echo \"$@\" > {}\necho $$ > {}\nsleep 300",
                argv_log.display(),
                pid_log.display()
            ),
        );
        Self {
            path,
            argv_log,
            pid_log,
        }
    }

    /// Block until the stub has recorded its argv, then return it.
    ///
    /// Non-empty is the condition, not mere existence: `sh`'s `>` redirect
    /// creates the file before `echo` writes into it, so an existence-only
    /// wait can still read `""` — which would make
    /// [`spawns_projectless_when_the_tui_is_projectless`]'s
    /// `!argv.contains("--project")` assertion pass vacuously.
    async fn argv(&self) -> String {
        wait_for_record(&self.argv_log, "argv").await
    }

    /// Block until the stub has recorded its pid, then return it.
    async fn pid(&self) -> u32 {
        wait_for_record(&self.pid_log, "pid")
            .await
            .trim()
            .parse::<u32>()
            .expect("the stub records its pid as a bare integer")
    }
}

impl Drop for SleepingStub {
    /// Reap the `sleep 300` child, panic or no panic.
    ///
    /// Why: `ensure_daemon_with` deliberately never signals a daemon it
    /// spawned, so nothing else will. A panicking assertion used to strand
    /// one five-minute sleeper per failed test, and a flake investigation
    /// re-runs the suite ~10× — the strays pile up. Best-effort by design: a
    /// test whose stub was never spawned leaves no pid file.
    fn drop(&mut self) {
        if let Ok(raw) = std::fs::read_to_string(&self.pid_log)
            && let Ok(pid) = raw.trim().parse::<u32>()
        {
            kill(pid);
        }
    }
}

/// Poll `path` until it holds a non-empty record, then return it.
///
/// The budget is 2s (200 × 10ms): long enough to absorb a `fork`/`exec`
/// delayed by a saturated machine, short enough that a stub which genuinely
/// never ran fails the test rather than hanging it.
async fn wait_for_record(path: &Path, label: &str) -> String {
    for _ in 0..200 {
        if let Ok(raw) = std::fs::read_to_string(path)
            && !raw.trim().is_empty()
        {
            return raw;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the spawned stub never recorded its {label} at {path:?}");
}

/// A stub that touches `marker` — used to PROVE a branch never spawned it.
fn marker_stub(dir: &Path, marker: &Path) -> PathBuf {
    stub_binary(
        dir,
        "tcode-marker",
        &format!("touch {}\nsleep 300", marker.display()),
    )
}

fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 performs permission/existence checks only and never
    // delivers a signal; it has no memory-safety effects.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn kill(pid: u32) {
    // SAFETY: `pid` names a stub process this test spawned; `kill` has no
    // memory-safety effects.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// A LIVE daemon bound to the SAME project must be attached to, never
/// re-spawned: the binary is never executed at all.
#[tokio::test]
async fn attaches_to_a_live_daemon_without_spawning() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();
    let project = tempfile::tempdir().expect("project");
    let canonical = project.path().canonicalize().expect("canonicalize");
    let sock_dir = tempfile::tempdir().expect("socket dir");
    let socket = stub_daemon_socket(sock_dir.path(), Some(binding_json(Some(&canonical))));

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let resolved = ensure_daemon_with(Some(&canonical), &stub, &socket)
        .await
        .expect("must attach to the live daemon");

    assert_eq!(resolved, socket);
    assert!(
        !marker.exists(),
        "must not have spawned anything: {marker:?}"
    );
}

/// A projectless TUI must attach to a projectless daemon — the other
/// agreeing pair.
#[tokio::test]
async fn attaches_to_a_projectless_daemon_when_projectless() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();
    let sock_dir = tempfile::tempdir().expect("socket dir");
    let socket = stub_daemon_socket(sock_dir.path(), Some(binding_json(None)));

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let resolved = ensure_daemon_with(None, &stub, &socket)
        .await
        .expect("a projectless TUI must attach to a projectless daemon");
    assert_eq!(resolved, socket);
    assert!(!marker.exists(), "must not have spawned anything");
}

/// The daemon on the well-known socket may be serving a DIFFERENT project.
/// Attaching would run every session against the wrong repository, so it
/// must be refused — and refused WITHOUT starting a competing daemon on a
/// socket that is already bound (#4512).
#[tokio::test]
async fn refuses_a_daemon_bound_to_another_project() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();
    let their_project = tempfile::tempdir().expect("their project");
    let our_project = tempfile::tempdir().expect("our project");
    let theirs = their_project.path().canonicalize().expect("canonicalize");
    let ours = our_project.path().canonicalize().expect("canonicalize");
    let sock_dir = tempfile::tempdir().expect("socket dir");
    let socket = stub_daemon_socket(sock_dir.path(), Some(binding_json(Some(&theirs))));

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let err = ensure_daemon_with(Some(&ours), &stub, &socket)
        .await
        .expect_err("a daemon on another project must not be attached to");

    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&theirs.display().to_string()),
        "error must name the daemon's project: {rendered}"
    );
    assert!(
        rendered.contains(&ours.display().to_string()),
        "error must name the requested project: {rendered}"
    );
    assert!(
        !marker.exists(),
        "must not start a competing daemon on a socket already bound: {rendered}"
    );
}

/// Projectless and bound are not interchangeable in EITHER direction — a
/// project-bound TUI must not silently lose its project to a projectless
/// daemon, and a projectless TUI must not silently inherit a project it
/// never named.
#[tokio::test]
async fn refuses_a_project_bound_client_against_a_projectless_daemon() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();
    let project = tempfile::tempdir().expect("project");
    let ours = project.path().canonicalize().expect("canonicalize");

    // Bound client, projectless daemon.
    {
        let sock_dir = tempfile::tempdir().expect("socket dir");
        let socket = stub_daemon_socket(sock_dir.path(), Some(binding_json(None)));
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("spawned");
        let stub = marker_stub(dir.path(), &marker);
        let err = ensure_daemon_with(Some(&ours), &stub, &socket)
            .await
            .expect_err("a bound TUI must not attach to a projectless daemon");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("<projectless>"), "{rendered}");
        assert!(!marker.exists(), "must not spawn a competing daemon");
    }

    // Projectless client, bound daemon.
    {
        let sock_dir = tempfile::tempdir().expect("socket dir");
        let socket = stub_daemon_socket(sock_dir.path(), Some(binding_json(Some(&ours))));
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("spawned");
        let stub = marker_stub(dir.path(), &marker);
        let err = ensure_daemon_with(None, &stub, &socket)
            .await
            .expect_err("a projectless TUI must not inherit a daemon's project");
        let rendered = format!("{err:#}");
        assert!(rendered.contains(&ours.display().to_string()), "{rendered}");
        assert!(rendered.contains("<projectless>"), "{rendered}");
        assert!(!marker.exists(), "must not spawn a competing daemon");
    }
}

/// A daemon too old to report its binding cannot be verified, so it is
/// refused — failing CLOSED, since "old build" is no evidence that its
/// project is the right one.
#[tokio::test]
async fn refuses_a_daemon_that_cannot_report_its_binding() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();
    let sock_dir = tempfile::tempdir().expect("socket dir");
    let socket = stub_daemon_socket(sock_dir.path(), None);

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let err = ensure_daemon_with(None, &stub, &socket)
        .await
        .expect_err("an unverifiable daemon must not be attached to");

    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("does not report which project"),
        "error must say the binding could not be confirmed: {rendered}"
    );
    assert!(!marker.exists(), "must not spawn a competing daemon");
}

/// `ReportedBinding` must read every shape `health` can answer with, and
/// never guess.
#[test]
fn reported_binding_parses_every_health_shape() {
    let project = tempfile::tempdir().expect("project");
    let canonical = project.path().canonicalize().expect("canonicalize");
    assert_eq!(
        ReportedBinding::from_health(&serde_json::json!({"binding": binding_json(None)})),
        ReportedBinding::Projectless
    );
    assert_eq!(
        ReportedBinding::from_health(
            &serde_json::json!({"binding": binding_json(Some(&canonical))})
        ),
        ReportedBinding::Bound(canonical)
    );
    assert_eq!(
        ReportedBinding::from_health(&serde_json::json!({"status": "ok"})),
        ReportedBinding::Unreported
    );
    assert_eq!(
        ReportedBinding::from_health(&serde_json::json!({"binding": "nonsense"})),
        ReportedBinding::Unreported
    );
}

/// Each state has to name itself in a way an operator can act on.
#[test]
fn reported_binding_describes_each_state() {
    assert_eq!(ReportedBinding::Projectless.describe(), "<projectless>");
    assert_eq!(
        ReportedBinding::Bound(PathBuf::from("/tmp/x")).describe(),
        "/tmp/x"
    );
    assert!(
        ReportedBinding::Unreported
            .describe()
            .contains("unreported")
    );
}

/// No daemon at all: one must be SPAWNED and waited for, with `--project`
/// forwarded through so its binding matches the TUI's.
#[tokio::test]
async fn spawns_a_daemon_when_none_is_running() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();
    let project = tempfile::tempdir().expect("project");
    let canonical = project.path().canonicalize().expect("canonicalize");

    let sock_dir = tempfile::tempdir().expect("socket dir");
    let socket = sock_dir.path().join("tcode.sock");
    // Bound shortly AFTER the call starts, so the attach branch cannot win
    // and the readiness wait is genuinely exercised.
    let late = sock_dir.path().to_path_buf();
    let binding = binding_json(Some(&canonical));
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        stub_daemon_socket(&late, Some(binding));
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let stub = SleepingStub::new(dir.path());

    let resolved = ensure_daemon_with(Some(&canonical), &stub.path, &socket)
        .await
        .expect("must spawn a daemon");
    assert_eq!(resolved, socket);

    // #6231: the stub is a real child, and readiness came from the socket
    // rather than from it — so its argv has to be waited for, not assumed.
    let argv = stub.argv().await;
    assert!(
        argv.contains("serve") && argv.contains("--http"),
        "must spawn `serve --http`: {argv}"
    );
    assert!(
        argv.contains("--project") && argv.contains(canonical.to_str().expect("utf8")),
        "must forward --project to the daemon: {argv}"
    );
}

/// A PROJECTLESS TUI must spawn a projectless daemon — `--project` is
/// omitted entirely rather than defaulting to the launch directory.
#[tokio::test]
async fn spawns_projectless_when_the_tui_is_projectless() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();

    let sock_dir = tempfile::tempdir().expect("socket dir");
    let socket = sock_dir.path().join("tcode.sock");
    let late = sock_dir.path().to_path_buf();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        stub_daemon_socket(&late, Some(binding_json(None)));
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let stub = SleepingStub::new(dir.path());

    ensure_daemon_with(None, &stub.path, &socket)
        .await
        .expect("must spawn a daemon");

    // #5073: an existence-only wait would be worse than the race — an empty
    // argv passes the negative assertion below for the wrong reason.
    let argv = stub.argv().await;
    assert!(
        !argv.contains("--project"),
        "a projectless TUI must not bind the daemon to a project: {argv}"
    );
}

/// **The readiness gate is the SOCKET, not `GET /health`.**
///
/// A daemon whose HTTP listener answers but whose socket is unbound is a
/// daemon `tcode tui` cannot drive at all — every client call goes over the
/// socket. This spawns a child that stays up, stands a healthy HTTP endpoint
/// beside it, and asserts the wait is still pending; binding the socket is
/// what completes it.
#[tokio::test]
async fn daemon_autospawn_waits_on_socket_not_http() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();

    // A healthy HTTP daemon, which must not satisfy the gate.
    let http = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/health"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"server": "tcode", "status": "ok", "binding": binding_json(None)}),
        ))
        .mount(&http)
        .await;

    let sock_dir = tempfile::tempdir().expect("socket dir");
    let socket = sock_dir.path().join("tcode.sock");
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = SleepingStub::new(dir.path());

    let spawn_socket = socket.clone();
    let spawn_stub = stub.path.clone();
    let waiting =
        tokio::spawn(async move { ensure_daemon_with(None, &spawn_stub, &spawn_socket).await });

    // The child is up and HTTP is healthy; readiness must NOT be reached.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        !waiting.is_finished(),
        "a healthy HTTP listener must not satisfy the socket readiness gate"
    );

    // Binding the socket is what completes it.
    stub_daemon_socket(sock_dir.path(), Some(binding_json(None)));
    let resolved = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the wait must complete once the socket answers")
        .expect("the wait task must not panic")
        .expect("readiness must be reported");
    assert_eq!(resolved, socket);
}

/// **The rule this module exists to guarantee.** The TUI NEVER signals the
/// daemon on exit, regardless of which process started it (owner directive,
/// 2026-08-01): the daemon owns PM lifecycle and agent dispatch, so a client
/// quitting must not destroy live work.
///
/// Both cases are asserted against real processes, after everything
/// `ensure_daemon_with` returned has been dropped — the drop is the moment a
/// `kill_on_drop` handle or a teardown step would have fired:
///
/// 1. a daemon THIS call spawned, and
/// 2. a pre-existing daemon it merely attached to.
#[tokio::test]
async fn the_tui_never_signals_the_daemon_on_exit() {
    let _lock = ENV_LOCK.lock().await;

    // 1. A daemon we spawned ourselves. The stub lives at FUNCTION scope on
    // purpose: its `Drop` reaps the child, and reaping it inside the block
    // below would kill the very process the `is_alive` assertion is about.
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = SleepingStub::new(dir.path());
    let sock_dir = tempfile::tempdir().expect("socket dir");
    let our_pid = {
        let _env = EnvGuard::isolated();
        let socket = sock_dir.path().join("tcode.sock");
        let late = sock_dir.path().to_path_buf();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            stub_daemon_socket(&late, Some(binding_json(None)));
        });
        let resolved = ensure_daemon_with(None, &stub.path, &socket)
            .await
            .expect("must spawn a daemon");
        assert_eq!(resolved, socket);
        stub.pid().await
        // Everything `ensure_daemon_with` produced is dropped here.
    };
    assert!(
        is_alive(our_pid),
        "a daemon the TUI spawned must OUTLIVE it (pid {our_pid} was killed)"
    );

    // 2. Somebody else's daemon, started outside `ensure_daemon_with`.
    let _env = EnvGuard::isolated();
    let foreign_sock_dir = tempfile::tempdir().expect("socket dir");
    let foreign_socket = stub_daemon_socket(foreign_sock_dir.path(), Some(binding_json(None)));
    let foreign_dir = tempfile::tempdir().expect("tempdir");
    let foreign_stub = SleepingStub::new(foreign_dir.path());
    let mut foreign = tokio::process::Command::new(&foreign_stub.path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn foreign daemon");
    let foreign_pid = foreign.id().expect("foreign pid");

    ensure_daemon_with(None, &foreign_stub.path, &foreign_socket)
        .await
        .expect("must attach");

    assert!(
        is_alive(foreign_pid),
        "a pre-existing daemon must survive the TUI too (pid {foreign_pid})"
    );
    foreign.kill().await.expect("clean up foreign daemon");
}

/// A spawned daemon that dies immediately (the socket-already-bound case)
/// must be reported straight away, not spun out to the startup timeout.
#[tokio::test]
async fn reports_a_daemon_that_dies_on_startup() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated();

    let dir = tempfile::tempdir().expect("tempdir");
    let stub = stub_binary(dir.path(), "tcode-dies", "exit 3");
    let sock_dir = tempfile::tempdir().expect("socket dir");
    // Nothing is bound here, so readiness can only ever come from the child
    // — which exits at once.
    let socket = sock_dir.path().join("tcode.sock");
    let started = std::time::Instant::now();
    let err = ensure_daemon_with(None, &stub, &socket)
        .await
        .expect_err("a daemon that exits must surface an error");

    assert!(
        started.elapsed() < STARTUP_TIMEOUT,
        "must fail fast rather than spin out the startup budget"
    );
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("exited during startup"),
        "error must name the early exit: {rendered}"
    );
}
