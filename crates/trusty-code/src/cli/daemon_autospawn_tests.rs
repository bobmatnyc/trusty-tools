//! Tests for [`super`] — `tcode tui`'s attach-or-spawn daemon policy
//! (#4512).
//!
//! Why a sibling file: `daemon_autospawn.rs` is a production file under the
//! 500-SLOC cap (issue #610); the same `#[cfg(test)] #[path = ...]` split
//! `tui_client/engine.rs` uses keeps these cases in a test-capped file.
//! What: every branch of [`super::ensure_daemon_with`] is exercised against
//! REAL child processes and a real `wiremock` health endpoint — no mocked
//! internals — so the spawn, the readiness gate, and the binding check are
//! all genuinely executed. Stub children stand in for the daemon: a `sh`
//! script that records its pid and argv then sleeps (a daemon that stays
//! up), one that exits immediately (a daemon that fails to bind), and one
//! that touches a marker file (to prove a branch never spawned anything).
//!
//! Because readiness here comes from `wiremock` rather than from the stub,
//! `ensure_daemon_with` can return before the stub child has run a single
//! line. Nothing a stub writes may therefore be read directly — go through
//! [`SleepingStub::argv`] / [`SleepingStub::pid`], which poll (#6231, #5073).

use std::path::{Path, PathBuf};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

/// Serializes every case here, since they all mutate the process-global
/// environment `lookup_daemon` reads. Mirrors the `ENV_LOCK` convention
/// already used by `tui_client::discovery`'s tests and `crate::task::mock_llm`.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// RAII guard isolating BOTH discovery sources for one test, and restoring
/// the environment on drop even if the test panics.
///
/// `TRUSTY_DATA_DIR_OVERRIDE` (trusty-common's documented test escape hatch)
/// repoints `resolve_data_dir("trusty-code")` at a fresh temp directory, so
/// the `http_addr` discovery file and the spawned-daemon log belong to this
/// test alone — a real daemon running on the developer's machine can never
/// make a "no daemon running" case attach to it instead.
struct EnvGuard {
    _data_dir: tempfile::TempDir,
}

impl EnvGuard {
    /// `daemon_url = None` means "TCODE_DAEMON_URL unset", i.e. discovery
    /// falls through to the (now empty) discovery file.
    fn isolated(daemon_url: Option<&str>) -> Self {
        let data_dir = tempfile::tempdir().expect("data dir");
        // SAFETY: test-only env mutation, serialized by `ENV_LOCK`.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, data_dir.path());
            match daemon_url {
                Some(url) => std::env::set_var(DAEMON_URL_ENV, url),
                None => std::env::remove_var(DAEMON_URL_ENV),
            }
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
            std::env::remove_var(DAEMON_URL_ENV);
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
    }
}

/// A `wiremock` server whose `GET /health` reports `binding` — i.e. a daemon
/// that looks alive to `lookup_daemon` and to `spin_until_ready`, and that
/// answers the #4512 binding question the way a real daemon would.
async fn healthy_daemon(root: Option<&Path>) -> MockServer {
    let binding = trusty_code::binding::ProjectBinding::resolve(root.map(Path::to_path_buf))
        .expect("must bind");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server": "tcode",
            "status": "ok",
            "binding": binding.to_json(),
        })))
        .mount(&server)
        .await;
    server
}

/// A daemon from before #4512: healthy, but reporting no binding at all.
async fn daemon_without_a_binding_field() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"server": "tcode", "status": "ok"})),
        )
        .mount(&server)
        .await;
    server
}

/// Write an executable `sh` stub to `dir` and return its path. The stub
/// stands in for the `tcode` binary that would be spawned.
fn stub_binary(dir: &Path, name: &str, body: &str) -> PathBuf {
    let script = dir.join(name);
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }
    script
}

/// A stub that records its own pid and the argv it was called with, then
/// sleeps — the "daemon that comes up and stays up" case.
///
/// The pid is written to disk rather than read off a `Child` handle because
/// `ensure_daemon_with` no longer RETURNS one: it drops the handle without
/// signalling, which is exactly the behaviour
/// [`the_tui_never_signals_the_daemon_on_exit`] has to observe from outside.
///
/// Why a struct rather than a bare path: the stub's two records are written
/// by a real `sh` child AFTER `spawn` returns, and readiness in these tests
/// comes from a `wiremock` endpoint that is already healthy — so
/// `ensure_daemon_with` returns without the child having run a single line.
/// Reading either record straight afterwards raced the write and failed with
/// `NotFound` under parallel load (#6231, #5073). [`SleepingStub::argv`] and
/// [`SleepingStub::pid`] are the only supported readers, and both poll.
/// [`Drop`] then reaps the `sleep 300` even when an assertion panicked first.
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
    /// `!argv.contains("--project")` assertion pass vacuously. Every spawn
    /// under test passes at least `serve --http`, so a non-empty read is a
    /// complete one.
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
    /// re-runs the suite ~10× — the strays pile up and starve the very run
    /// meant to prove stability. Best-effort by design: a test whose stub was
    /// never spawned leaves no pid file, and that is not an error.
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
/// The budget is 2s (200 × 10ms), unchanged from the pid wait this replaces:
/// long enough to absorb a `fork`/`exec` delayed by a saturated machine,
/// short enough that a stub which genuinely never ran fails the test rather
/// than hanging it. `label` names the record in the panic message.
async fn wait_for_record(path: &Path, label: &str) -> String {
    for _ in 0..200 {
        if let Ok(raw) = std::fs::read_to_string(path)
            && !raw.trim().is_empty()
        {
            return raw;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 performs permission/existence checks only and never
    // delivers a signal; it has no memory-safety effects.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(unix)]
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
    let project = tempfile::tempdir().expect("project");
    let canonical = project.path().canonicalize().expect("canonicalize");
    let server = healthy_daemon(Some(&canonical)).await;
    let _env = EnvGuard::isolated(Some(&server.uri()));

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let url = ensure_daemon_with(
        &reqwest::Client::new(),
        Some(&canonical),
        &stub,
        "http://unused.invalid",
    )
    .await
    .expect("must attach to the live daemon");

    assert_eq!(url, server.uri().trim_end_matches('/'));
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
    let server = healthy_daemon(None).await;
    let _env = EnvGuard::isolated(Some(&server.uri()));

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let url = ensure_daemon_with(
        &reqwest::Client::new(),
        None,
        &stub,
        "http://unused.invalid",
    )
    .await
    .expect("a projectless TUI must attach to a projectless daemon");
    assert_eq!(url, server.uri().trim_end_matches('/'));
    assert!(!marker.exists(), "must not have spawned anything");
}

/// The daemon on the well-known port may be serving a DIFFERENT project.
/// Attaching would run every session against the wrong repository, so it
/// must be refused — and refused WITHOUT starting a competing daemon on a
/// port that is already taken (#4512).
#[tokio::test]
async fn refuses_a_daemon_bound_to_another_project() {
    let _lock = ENV_LOCK.lock().await;
    let their_project = tempfile::tempdir().expect("their project");
    let our_project = tempfile::tempdir().expect("our project");
    let theirs = their_project.path().canonicalize().expect("canonicalize");
    let ours = our_project.path().canonicalize().expect("canonicalize");
    let server = healthy_daemon(Some(&theirs)).await;
    let _env = EnvGuard::isolated(Some(&server.uri()));

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let err = ensure_daemon_with(&reqwest::Client::new(), Some(&ours), &stub, &server.uri())
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
        "must not start a competing daemon on a port already in use: {rendered}"
    );
}

/// Projectless and bound are not interchangeable in EITHER direction — a
/// project-bound TUI must not silently lose its project to a projectless
/// daemon, and a projectless TUI must not silently inherit a project it
/// never named.
#[tokio::test]
async fn refuses_a_project_bound_client_against_a_projectless_daemon() {
    let _lock = ENV_LOCK.lock().await;
    let project = tempfile::tempdir().expect("project");
    let ours = project.path().canonicalize().expect("canonicalize");

    // Bound client, projectless daemon.
    {
        let server = healthy_daemon(None).await;
        let _env = EnvGuard::isolated(Some(&server.uri()));
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("spawned");
        let stub = marker_stub(dir.path(), &marker);
        let err = ensure_daemon_with(&reqwest::Client::new(), Some(&ours), &stub, &server.uri())
            .await
            .expect_err("a bound TUI must not attach to a projectless daemon");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("<projectless>"), "{rendered}");
        assert!(!marker.exists(), "must not spawn a competing daemon");
    }

    // Projectless client, bound daemon.
    {
        let server = healthy_daemon(Some(&ours)).await;
        let _env = EnvGuard::isolated(Some(&server.uri()));
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("spawned");
        let stub = marker_stub(dir.path(), &marker);
        let err = ensure_daemon_with(&reqwest::Client::new(), None, &stub, &server.uri())
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
    let server = daemon_without_a_binding_field().await;
    let _env = EnvGuard::isolated(Some(&server.uri()));

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let err = ensure_daemon_with(&reqwest::Client::new(), None, &stub, &server.uri())
        .await
        .expect_err("an unverifiable daemon must not be attached to");

    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("does not report which project"),
        "error must say the binding could not be confirmed: {rendered}"
    );
    assert!(!marker.exists(), "must not spawn a competing daemon");
}

/// No candidate daemon at all: a daemon must be SPAWNED and waited for, with
/// `--project` forwarded through to it so its binding matches the TUI's.
#[tokio::test]
async fn spawns_a_daemon_when_none_is_running() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated(None);
    let project = tempfile::tempdir().expect("project");
    let canonical = project.path().canonicalize().expect("canonicalize");
    let server = healthy_daemon(Some(&canonical)).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let stub = SleepingStub::new(dir.path());

    let url = ensure_daemon_with(
        &reqwest::Client::new(),
        Some(&canonical),
        &stub.path,
        &server.uri(),
    )
    .await
    .expect("must spawn a daemon");
    assert_eq!(url, server.uri());

    // #6231: the stub is a real child, and readiness came from `wiremock`
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
    let _env = EnvGuard::isolated(None);
    let server = healthy_daemon(None).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let stub = SleepingStub::new(dir.path());

    ensure_daemon_with(&reqwest::Client::new(), None, &stub.path, &server.uri())
        .await
        .expect("must spawn a daemon");

    // #5073: reading here without waiting failed with `NotFound` under a
    // parallel suite. An existence-only wait would be worse than the race —
    // an empty argv passes the negative assertion below for the wrong
    // reason — so `argv()` waits for a non-empty record.
    let argv = stub.argv().await;
    assert!(
        !argv.contains("--project"),
        "a projectless TUI must not bind the daemon to a project: {argv}"
    );
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
    // `dir` is declared first so it outlives the stub that reads out of it.
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = SleepingStub::new(dir.path());
    let our_pid = {
        let _env = EnvGuard::isolated(None);
        let server = healthy_daemon(None).await;

        let url = ensure_daemon_with(&reqwest::Client::new(), None, &stub.path, &server.uri())
            .await
            .expect("must spawn a daemon");
        assert_eq!(url, server.uri());
        stub.pid().await
        // Everything `ensure_daemon_with` produced is dropped here.
    };
    assert!(
        is_alive(our_pid),
        "a daemon the TUI spawned must OUTLIVE it (pid {our_pid} was killed)"
    );

    // 2. Somebody else's daemon, started outside `ensure_daemon_with`.
    let server = healthy_daemon(None).await;
    let _env = EnvGuard::isolated(Some(&server.uri()));
    let foreign_dir = tempfile::tempdir().expect("tempdir");
    let foreign_stub = SleepingStub::new(foreign_dir.path());
    let mut foreign = tokio::process::Command::new(&foreign_stub.path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn foreign daemon");
    let foreign_pid = foreign.id().expect("foreign pid");

    ensure_daemon_with(
        &reqwest::Client::new(),
        None,
        &foreign_stub.path,
        "http://unused.invalid",
    )
    .await
    .expect("must attach");

    assert!(
        is_alive(foreign_pid),
        "a pre-existing daemon must survive the TUI too (pid {foreign_pid})"
    );
    foreign.kill().await.expect("clean up foreign daemon");
}

/// A spawned daemon that dies immediately (the port-already-taken case)
/// must be reported straight away, not spun out to the startup timeout.
#[tokio::test]
async fn reports_a_daemon_that_dies_on_startup() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated(None);

    let dir = tempfile::tempdir().expect("tempdir");
    let stub = stub_binary(dir.path(), "tcode-dies", "exit 3");
    // Nothing is bound here, so readiness can only ever come from the child
    // — which exits at once.
    let started = std::time::Instant::now();
    let err = ensure_daemon_with(&reqwest::Client::new(), None, &stub, "http://127.0.0.1:1")
        .await
        .expect_err("a daemon that exits must surface an error");

    assert!(
        started.elapsed() < STARTUP_TIMEOUT,
        "must fail fast rather than spin out the startup budget"
    );
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("exited during startup"),
        "error must say the daemon died: {rendered}"
    );
}

/// An explicitly-set-but-unreachable `TCODE_DAEMON_URL` must FAIL, never
/// silently start a daemon somewhere else — the operator named an address
/// and we obey it.
#[tokio::test]
async fn refuses_to_spawn_for_an_unreachable_explicit_url() {
    let _lock = ENV_LOCK.lock().await;
    // Port 1 is reserved and unbound: reachable-looking, never answering.
    let _env = EnvGuard::isolated(Some("http://127.0.0.1:1"));
    let server = healthy_daemon(None).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let err = ensure_daemon_with(&reqwest::Client::new(), None, &stub, &server.uri())
        .await
        .expect_err("an unreachable explicit URL must be an error");

    assert!(
        !marker.exists(),
        "must not spawn a daemon when {DAEMON_URL_ENV} names one explicitly"
    );
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("http://127.0.0.1:1"),
        "error must name the unreachable URL: {rendered}"
    );
    assert!(
        rendered.contains(DAEMON_URL_ENV),
        "error must name the env var responsible: {rendered}"
    );
}
