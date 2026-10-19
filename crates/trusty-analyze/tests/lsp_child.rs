//! Subprocess tests for the LSP child manager (#9078, L2).
//!
//! Why: spawn, crash, hang, idle reap and process leaks only exist across a real
//! process boundary, and CI has no `rust-analyzer`. Every test here drives a
//! scripted fake server (`support/fake_lsp_server.rs`) that runs inside this
//! very test binary, re-executed through `/bin/sh`.
//! What: each test builds an enabled [`LspManager`] whose `rust-analyzer`
//! command is the fake in some mode, then asserts on the typed outcome and, for
//! every exit path, that the child's pid is gone.
//! Test: this file is the test.
#![cfg(unix)]

#[path = "support/fake_lsp_server.rs"]
mod fake_lsp_server;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use trusty_analyze::lsp::{LspCommand, LspConfig, LspError, LspLanguage, LspManager};

const RUST: LspLanguage = LspLanguage::Rust;

/// The entry point the fake server runs under; a no-op in a normal test run.
#[test]
fn fake_lsp_server_entry() {
    fake_lsp_server::run_if_requested();
}

/// A command that re-executes this binary as the fake server in `mode`.
fn fake(mode: &str, pidfile: &Path) -> LspCommand {
    let exe = std::env::current_exe().expect("current_exe");
    let script = format!(
        "{}=\"$1\" {}=\"$2\" exec \"$0\" --exact fake_lsp_server_entry --nocapture \
         --test-threads=1 3>&1 1>&2",
        fake_lsp_server::MODE_ENV,
        fake_lsp_server::PIDFILE_ENV
    );
    LspCommand {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            script,
            exe.display().to_string(),
            mode.into(),
            pidfile.display().to_string(),
        ],
    }
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn root(&self) -> PathBuf {
        let root = self.dir.path().join("ws");
        std::fs::create_dir_all(&root).expect("root");
        root
    }

    fn pidfile(&self) -> PathBuf {
        self.dir.path().join("grandchild.pid")
    }

    fn config(&self, mode: &str) -> LspConfig {
        LspConfig {
            enabled: true,
            rust_analyzer: fake(mode, &self.pidfile()),
            idle_timeout: Duration::from_secs(600),
            request_timeout: Duration::from_secs(10),
            initialize_timeout: Duration::from_secs(10),
            shutdown_grace: Duration::from_millis(500),
            ..LspConfig::default()
        }
    }

    fn manager(&self, mode: &str) -> LspManager {
        LspManager::new(self.config(mode))
    }

    async fn grandchild_pid(&self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(raw) = std::fs::read_to_string(self.pidfile()) {
                return raw.trim().parse().expect("pidfile holds a pid");
            }
            assert!(Instant::now() < deadline, "grandchild never wrote its pid");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes for existence; it delivers nothing.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

async fn gone_within(pid: u32, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    !alive(pid)
}

const GONE: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread")]
async fn spawned_server_initializes_and_answers() {
    let fx = Fixture::new();
    let root = fx.root();
    let file = root.join("lib.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let manager = fx.manager("normal");

    let lease = manager.session(&root, RUST).await.expect("session");
    let pid = lease.pid();
    assert!(alive(pid));
    let doc = lease.client().sync_document(&file, "rust").await.unwrap();
    assert_eq!(doc.version, 1);
    let refs = lease
        .request(
            "textDocument/references",
            json!({"textDocument": {"uri": doc.uri}, "position": {"line": 0, "character": 3},
                   "context": {"includeDeclaration": true}}),
        )
        .await
        .expect("references");
    assert_eq!(refs[0]["range"]["start"]["line"], 1);
    let diags = lease
        .client()
        .wait_for_diagnostics(&doc.uri, 1, Duration::from_secs(10))
        .await
        .expect("diagnostics");
    assert_eq!(diags.diagnostics.len(), 1);

    // One child per root: a second session reuses it.
    let again = manager.session(&root, RUST).await.expect("second session");
    assert_eq!(again.pid(), pid);
    assert_eq!(manager.live_pids().await, vec![pid]);

    drop((lease, again));
    manager.shutdown_all().await;
    assert!(!alive(pid), "shutdown_all must reap the child");
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_mid_request_is_an_error_not_a_hang() {
    let fx = Fixture::new();
    let root = fx.root();
    let manager = fx.manager("crash");
    let lease = manager.session(&root, RUST).await.expect("session");
    let pid = lease.pid();

    let res = tokio::time::timeout(
        Duration::from_secs(5),
        lease.request("x/anything", json!({})),
    )
    .await
    .expect("a crashed server must fail the call, not hang it");
    assert!(matches!(res, Err(LspError::ServerExited { .. })), "{res:?}");
    drop(lease);
    assert!(gone_within(pid, GONE).await, "the crashed child is reaped");

    // Reported, and not silently respawned: the next call gets the recorded
    // failure (with the exit status) and spawns nothing.
    match manager.session(&root, RUST).await {
        Err(LspError::ServerExited { detail }) => assert!(detail.contains('3'), "{detail}"),
        Err(other) => panic!("expected ServerExited, got {other:?}"),
        Ok(lease) => panic!("respawned pid {} inside the cool-off", lease.pid()),
    }
    assert!(manager.live_pids().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_binary_is_not_installed() {
    let fx = Fixture::new();
    let root = fx.root();
    for program in [
        "trusty-no-such-language-server",
        "/nonexistent/rust-analyzer",
    ] {
        let manager = LspManager::new(LspConfig {
            enabled: true,
            rust_analyzer: LspCommand {
                program: program.into(),
                args: vec![],
            },
            ..LspConfig::default()
        });
        let started = Instant::now();
        let res = manager.session(&root, RUST).await;
        assert!(
            matches!(&res, Err(LspError::NotInstalled { program: p }) if p == program),
            "{program}: {:?}",
            res.err()
        );
        assert!(started.elapsed() < Duration::from_secs(5), "never a hang");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_initialize_carries_the_stderr_tail() {
    let fx = Fixture::new();
    let res = fx.manager("fail_init").session(&fx.root(), RUST).await;
    match res {
        Err(LspError::InitializeFailed { stderr_tail, .. }) => assert!(
            stderr_tail.contains(fake_lsp_server::FAIL_INIT_STDERR),
            "{stderr_tail}"
        ),
        other => panic!("expected InitializeFailed, got {:?}", other.err()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_frame_during_initialize_fails_fast() {
    let fx = Fixture::new();
    let started = Instant::now();
    let res = fx.manager("partial_frame").session(&fx.root(), RUST).await;
    match res {
        Err(LspError::InitializeFailed { reason, .. }) => {
            assert!(reason.contains("mid-frame"), "{reason}")
        }
        other => panic!("expected InitializeFailed, got {:?}", other.err()),
    }
    assert!(started.elapsed() < Duration::from_secs(5), "never a hang");
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_requests_time_out_then_the_hung_child_is_reaped() {
    let fx = Fixture::new();
    let root = fx.root();
    let manager = LspManager::new(LspConfig {
        request_timeout: Duration::from_millis(200),
        max_consecutive_timeouts: 2,
        ..fx.config("stall")
    });
    let lease = manager.session(&root, RUST).await.expect("session");
    let pid = lease.pid();

    let first = lease.request("x/slow", json!({})).await;
    assert!(matches!(first, Err(LspError::Timeout { .. })), "{first:?}");
    assert!(alive(pid), "one timeout leaves the child serving");
    let second = lease.request("x/slow", json!({})).await;
    assert_eq!(second, Err(LspError::Hung { consecutive: 2 }));
    assert!(gone_within(pid, GONE).await, "a hung child is killed");
    drop(lease);

    let next = manager.session(&root, RUST).await;
    assert_eq!(next.err(), Some(LspError::Hung { consecutive: 2 }));
    assert!(manager.live_pids().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_child_is_reaped_and_the_next_call_respawns() {
    let fx = Fixture::new();
    let root = fx.root();
    let manager = LspManager::new(LspConfig {
        idle_timeout: Duration::from_millis(300),
        ..fx.config("normal")
    });
    let lease = manager.session(&root, RUST).await.expect("session");
    let pid = lease.pid();
    // A held lease is in flight: the idle window must not reap it.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(alive(pid), "an in-flight lease holds the child");
    drop(lease);

    assert!(gone_within(pid, GONE).await, "the idle child is reaped");
    assert!(manager.live_pids().await.is_empty());
    let next = manager
        .session(&root, RUST)
        .await
        .expect("respawn after idle");
    assert_ne!(next.pid(), pid);
    let new_pid = next.pid();
    drop(next);
    manager.shutdown_all().await;
    assert!(!alive(new_pid));
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_flag_spawns_nothing() {
    let fx = Fixture::new();
    let manager = LspManager::new(LspConfig {
        enabled: false,
        ..fx.config("grandchild")
    });
    let res = manager.session(&fx.root(), RUST).await;
    assert_eq!(res.err(), Some(LspError::Disabled));
    assert_eq!(LspError::Disabled.code(), "lsp_disabled");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!fx.pidfile().exists(), "nothing was spawned");
    assert!(manager.live_pids().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_manager_kills_the_child_and_its_descendants() {
    let fx = Fixture::new();
    let manager = fx.manager("grandchild");
    let lease = manager.session(&fx.root(), RUST).await.expect("session");
    let pid = lease.pid();
    let grandchild = fx.grandchild_pid().await;
    assert!(alive(grandchild));

    drop(lease);
    drop(manager);
    assert!(gone_within(pid, GONE).await, "child {pid} leaked past drop");
    assert!(
        gone_within(grandchild, GONE).await,
        "grandchild {grandchild} leaked past drop"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_all_kills_a_child_that_ignores_exit() {
    let fx = Fixture::new();
    let manager = fx.manager("ignore_exit");
    let lease = manager.session(&fx.root(), RUST).await.expect("session");
    let pid = lease.pid();
    drop(lease);
    let started = Instant::now();
    manager.shutdown_all().await;
    assert!(!alive(pid), "a child that ignores `exit` is killed");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_owner_still_kills_the_child() {
    let fx = Fixture::new();
    let manager = fx.manager("grandchild");
    let lease = manager.session(&fx.root(), RUST).await.expect("session");
    let pid = lease.pid();
    let grandchild = fx.grandchild_pid().await;

    let owner = tokio::spawn(async move {
        let _owned = (manager, lease);
        panic!("owner panics while holding the manager");
    });
    assert!(owner.await.expect_err("the owner panicked").is_panic());
    assert!(
        gone_within(pid, GONE).await,
        "child {pid} leaked past a panic"
    );
    assert!(
        gone_within(grandchild, GONE).await,
        "grandchild leaked past a panic"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_idle_exit_kills_the_child() {
    use trusty_analyze::core::{FactStore, ScipOverlayStore, TrustySearchClient};
    use trusty_analyze::service::events::AnalyzerAppState;

    let fx = Fixture::new();
    let data = fx.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let manager = Arc::new(fx.manager("grandchild"));
    let state = AnalyzerAppState::new(
        TrustySearchClient::new(&data.join("absent-search.sock")),
        FactStore::open(&data.join("facts.redb")).unwrap(),
        ScipOverlayStore::open(&data.join("scip.redb")).unwrap(),
    )
    .with_lsp(Arc::clone(&manager));

    let lease = manager.session(&fx.root(), RUST).await.expect("session");
    let pid = lease.pid();
    let grandchild = fx.grandchild_pid().await;
    drop(lease);

    let socket = fx.dir.path().join("a.sock");
    trusty_analyze::service::rpc::serve_with_idle(
        state,
        &socket,
        std::future::pending::<()>(),
        Some(Duration::from_millis(300)),
    )
    .await
    .expect("serve exits on idle");
    // The test still holds `manager`, so only the idle-exit path can have
    // killed the child.
    assert!(!alive(pid), "child {pid} outlived the daemon's idle exit");
    assert!(
        gone_within(grandchild, GONE).await,
        "grandchild outlived idle exit"
    );
    assert!(manager.live_pids().await.is_empty());
}
