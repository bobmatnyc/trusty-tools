//! Tests for [`super`] — `tcode tui`'s attach-or-spawn daemon policy
//! (#4512).
//!
//! Why a sibling file: `daemon_autospawn.rs` is a production file under the
//! 500-SLOC cap (issue #610); the same `#[cfg(test)] #[path = ...]` split
//! `tui_client/engine.rs` uses keeps these cases in a test-capped file.
//! What: every branch of [`super::ensure_daemon_with`] is exercised against
//! REAL child processes and a real `wiremock` health endpoint — no mocked
//! internals — so the spawn, the readiness gate, and the SIGTERM teardown
//! are all genuinely executed. Stub children stand in for the daemon: a
//! `sh` script that sleeps (a daemon that stays up), one that exits
//! immediately (a daemon that fails to bind), and one that touches a marker
//! file (to prove a branch never spawned anything).

use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// A `wiremock` server answering `GET /health` with 200 — i.e. a daemon that
/// looks alive to `lookup_daemon` and to `spin_until_ready`.
async fn healthy_daemon() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200))
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

/// A stub that records the argv it was called with, then sleeps — the
/// "daemon that comes up and stays up" case.
fn sleeping_stub(dir: &Path, argv_log: &Path) -> PathBuf {
    stub_binary(
        dir,
        "tcode-sleep",
        &format!("echo \"$@\" > {}\nsleep 300", argv_log.display()),
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

/// A LIVE daemon must be attached to, never re-spawned: the recorded
/// ownership is `Attached` and the binary is never executed at all.
#[tokio::test]
async fn attaches_to_a_live_daemon_without_spawning() {
    let _lock = ENV_LOCK.lock().await;
    let server = healthy_daemon().await;
    let _env = EnvGuard::isolated(Some(&server.uri()));

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("spawned");
    let stub = marker_stub(dir.path(), &marker);

    let session = ensure_daemon_with(
        &reqwest::Client::new(),
        None,
        &stub,
        "http://unused.invalid",
    )
    .await
    .expect("must attach to the live daemon");

    assert!(
        matches!(session.ownership, Ownership::Attached),
        "a pre-existing daemon must be recorded as Attached, not Spawned"
    );
    assert_eq!(session.url, server.uri().trim_end_matches('/'));
    assert!(
        !marker.exists(),
        "must not have spawned anything: {marker:?}"
    );
}

/// Shutting down an ATTACHED session must be a no-op — a daemon we did not
/// start has to survive our exit. Asserted against a real, independently
/// spawned process that stands in for "somebody else's daemon".
#[tokio::test]
async fn shutdown_leaves_a_pre_existing_daemon_running() {
    let _lock = ENV_LOCK.lock().await;
    let server = healthy_daemon().await;
    let _env = EnvGuard::isolated(Some(&server.uri()));

    let dir = tempfile::tempdir().expect("tempdir");
    let argv_log = dir.path().join("argv");
    let stub = sleeping_stub(dir.path(), &argv_log);
    // Somebody else's daemon: started outside `ensure_daemon_with` entirely.
    let mut foreign = tokio::process::Command::new(&stub)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn foreign daemon");
    let foreign_pid = foreign.id().expect("foreign pid");

    let session = ensure_daemon_with(
        &reqwest::Client::new(),
        None,
        &stub,
        "http://unused.invalid",
    )
    .await
    .expect("must attach");
    session.shutdown().await;

    assert!(
        is_alive(foreign_pid),
        "shutdown must not kill a daemon it did not spawn (pid {foreign_pid})"
    );
    foreign.kill().await.expect("clean up foreign daemon");
}

/// No candidate daemon at all: a daemon must be SPAWNED, waited for, and
/// recorded as owned — with `--project` forwarded through to it.
#[tokio::test]
async fn spawns_a_daemon_when_none_is_running() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated(None);
    let server = healthy_daemon().await;

    let dir = tempfile::tempdir().expect("tempdir");
    let argv_log = dir.path().join("argv");
    let stub = sleeping_stub(dir.path(), &argv_log);
    let project = dir.path().join("proj");
    std::fs::create_dir(&project).expect("mkdir project");

    let session = ensure_daemon_with(
        &reqwest::Client::new(),
        Some(project.as_path()),
        &stub,
        &server.uri(),
    )
    .await
    .expect("must spawn a daemon");

    let Ownership::Spawned(ref child) = session.ownership else {
        panic!("a daemon we started must be recorded as Spawned");
    };
    assert!(child.id().is_some(), "the spawned child must have a pid");

    let argv = std::fs::read_to_string(&argv_log).expect("stub must have recorded its argv");
    assert!(
        argv.contains("serve") && argv.contains("--http"),
        "must spawn `serve --http`: {argv}"
    );
    assert!(
        argv.contains("--project") && argv.contains(project.to_str().expect("utf8")),
        "must forward --project to the daemon: {argv}"
    );
    session.shutdown().await;
}

/// A PROJECTLESS TUI must spawn a projectless daemon — `--project` is
/// omitted entirely rather than defaulting to the launch directory.
#[tokio::test]
async fn spawns_projectless_when_the_tui_is_projectless() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated(None);
    let server = healthy_daemon().await;

    let dir = tempfile::tempdir().expect("tempdir");
    let argv_log = dir.path().join("argv");
    let stub = sleeping_stub(dir.path(), &argv_log);

    let session = ensure_daemon_with(&reqwest::Client::new(), None, &stub, &server.uri())
        .await
        .expect("must spawn a daemon");

    let argv = std::fs::read_to_string(&argv_log).expect("stub must have recorded its argv");
    assert!(
        !argv.contains("--project"),
        "a projectless TUI must not bind the daemon to a project: {argv}"
    );
    session.shutdown().await;
}

/// A daemon WE spawned must actually be stopped on shutdown — the other
/// half of the ownership rule.
#[tokio::test]
async fn shutdown_stops_a_daemon_we_spawned() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::isolated(None);
    let server = healthy_daemon().await;

    let dir = tempfile::tempdir().expect("tempdir");
    let argv_log = dir.path().join("argv");
    let stub = sleeping_stub(dir.path(), &argv_log);

    let session = ensure_daemon_with(&reqwest::Client::new(), None, &stub, &server.uri())
        .await
        .expect("must spawn a daemon");
    let Ownership::Spawned(ref child) = session.ownership else {
        panic!("must be Spawned");
    };
    let pid = child.id().expect("pid");
    assert!(is_alive(pid), "the spawned daemon must be running first");

    session.shutdown().await;

    assert!(
        !is_alive(pid),
        "a daemon we spawned must be stopped on shutdown (pid {pid} still alive)"
    );
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
    let server = healthy_daemon().await;

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

/// `terminate` on an already-exited child must reap it without panicking or
/// waiting out the grace period.
#[tokio::test]
async fn terminate_handles_an_already_exited_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = stub_binary(dir.path(), "tcode-quick", "exit 0");
    let mut child = tokio::process::Command::new(&stub)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    child.wait().await.expect("reap");

    // Must return promptly; a hang here would stall every TUI exit.
    tokio::time::timeout(Duration::from_secs(2), terminate(child, SHUTDOWN_GRACE))
        .await
        .expect("terminate must not hang on an already-exited child");
}
