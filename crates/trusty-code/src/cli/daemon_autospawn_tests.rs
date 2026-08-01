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
fn sleeping_stub(dir: &Path, argv_log: &Path, pid_log: &Path) -> PathBuf {
    stub_binary(
        dir,
        "tcode-sleep",
        &format!(
            "echo \"$@\" > {}\necho $$ > {}\nsleep 300",
            argv_log.display(),
            pid_log.display()
        ),
    )
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

/// Block until `pid_log` exists and holds a pid — the stub writes it a few
/// milliseconds after `spawn` returns.
async fn spawned_pid(pid_log: &Path) -> u32 {
    for _ in 0..200 {
        if let Ok(raw) = std::fs::read_to_string(pid_log)
            && let Ok(pid) = raw.trim().parse::<u32>()
        {
            return pid;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the spawned stub never recorded its pid at {pid_log:?}");
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
    let argv_log = dir.path().join("argv");
    let pid_log = dir.path().join("pid");
    let stub = sleeping_stub(dir.path(), &argv_log, &pid_log);

    let url = ensure_daemon_with(
        &reqwest::Client::new(),
        Some(&canonical),
        &stub,
        &server.uri(),
    )
    .await
    .expect("must spawn a daemon");
    assert_eq!(url, server.uri());

    let argv = std::fs::read_to_string(&argv_log).expect("stub must have recorded its argv");
    assert!(
        argv.contains("serve") && argv.contains("--http"),
        "must spawn `serve --http`: {argv}"
    );
    assert!(
        argv.contains("--project") && argv.contains(canonical.to_str().expect("utf8")),
        "must forward --project to the daemon: {argv}"
    );
    kill(spawned_pid(&pid_log).await);
}

/// A PROJECTLESS TUI must spawn a projectless daemon — `--project` is
/// omitted entirely rather than defaulting to the launch directory.
#[tokio::test]
async fn spawns_projectless_when_the_tui_is_projectless() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated(None);
    let server = healthy_daemon(None).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let argv_log = dir.path().join("argv");
    let pid_log = dir.path().join("pid");
    let stub = sleeping_stub(dir.path(), &argv_log, &pid_log);

    ensure_daemon_with(&reqwest::Client::new(), None, &stub, &server.uri())
        .await
        .expect("must spawn a daemon");

    let argv = std::fs::read_to_string(&argv_log).expect("stub must have recorded its argv");
    assert!(
        !argv.contains("--project"),
        "a projectless TUI must not bind the daemon to a project: {argv}"
    );
    kill(spawned_pid(&pid_log).await);
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

    // 1. A daemon we spawned ourselves.
    let our_pid = {
        let _env = EnvGuard::isolated(None);
        let server = healthy_daemon(None).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let argv_log = dir.path().join("argv");
        let pid_log = dir.path().join("pid");
        let stub = sleeping_stub(dir.path(), &argv_log, &pid_log);

        let url = ensure_daemon_with(&reqwest::Client::new(), None, &stub, &server.uri())
            .await
            .expect("must spawn a daemon");
        assert_eq!(url, server.uri());
        spawned_pid(&pid_log).await
        // Everything `ensure_daemon_with` produced is dropped here.
    };
    assert!(
        is_alive(our_pid),
        "a daemon the TUI spawned must OUTLIVE it (pid {our_pid} was killed)"
    );
    kill(our_pid);

    // 2. Somebody else's daemon, started outside `ensure_daemon_with`.
    let server = healthy_daemon(None).await;
    let _env = EnvGuard::isolated(Some(&server.uri()));
    let dir = tempfile::tempdir().expect("tempdir");
    let argv_log = dir.path().join("argv");
    let pid_log = dir.path().join("pid");
    let stub = sleeping_stub(dir.path(), &argv_log, &pid_log);
    let mut foreign = tokio::process::Command::new(&stub)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn foreign daemon");
    let foreign_pid = foreign.id().expect("foreign pid");

    ensure_daemon_with(
        &reqwest::Client::new(),
        None,
        &stub,
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
