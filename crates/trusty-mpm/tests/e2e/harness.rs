//! End-to-end test harness: spawn a real daemon on a loopback port.
//!
//! Why: the e2e suite must exercise the daemon the way a real client does —
//! over HTTP, against the live axum router and shared state — not by calling
//! handlers directly. Every test gets its own daemon bound to a random free
//! port with a `tempfile::TempDir`-scoped [`FrameworkPaths`] so no test touches
//! (or is influenced by) the operator's real `~/.trusty-mpm` install.
//! What: [`TestDaemon::spawn`] binds `127.0.0.1:0`, serves
//! `trusty_mpm::daemon::api::router` on a background task, polls `/health` until
//! it answers, and hands back a value whose `Drop` aborts the server task and
//! removes the temp directory.
//! Test: used by every `test_*.rs` scenario file in this suite.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::daemon::state::DaemonState;

/// A live daemon for one test, bound to a random loopback port.
///
/// Why: tests need a real HTTP endpoint and access to the temp-scoped
/// framework directory (to plant `optimizer.toml` / `overseer.toml` and to
/// inspect deployed agents). Bundling the `TempDir` and the server task handle
/// keeps both alive for the test's duration and reaped on drop.
/// What: the base URL, the bound address, the resolved [`FrameworkPaths`], the
/// temp directory, and the server task handle.
pub struct TestDaemon {
    /// Base URL of the daemon, e.g. `http://127.0.0.1:54123`.
    pub url: String,
    /// The address the daemon bound to. Part of the public harness surface;
    /// not every scenario reads it.
    #[allow(dead_code)]
    pub addr: SocketAddr,
    /// Framework paths scoped to this test's temp directory.
    pub paths: FrameworkPaths,
    /// Keeps the temp directory alive; dropped (and deleted) with the daemon.
    _tmpdir: TempDir,
    /// The background task serving the router; aborted on drop.
    handle: tokio::task::JoinHandle<()>,
}

impl TestDaemon {
    /// Spawn a daemon with a fresh temp framework directory and no config
    /// files (everything falls back to defaults).
    ///
    /// Why: the common case — most scenarios want a clean daemon and do not
    /// care about framework-managed policy files.
    /// What: delegates to [`TestDaemon::spawn_with`] with a no-op setup.
    /// Test: `test_health::health_returns_ok`.
    pub async fn spawn() -> Self {
        Self::spawn_with(|_paths| {}).await
    }

    /// Spawn a daemon after running `setup` against its temp framework dir.
    ///
    /// Why: the optimizer / overseer scenarios must plant policy files *before*
    /// `DaemonState::with_paths` reads them; `setup` runs at exactly that point.
    /// What: creates a `TempDir`, resolves [`FrameworkPaths::under`] it, creates
    /// the `framework/hooks` directory, calls `setup`, builds the state via
    /// [`DaemonState::with_paths`], binds `127.0.0.1:0`, serves the router on a
    /// background task, and polls `/health` until it answers (max ~2s).
    /// Test: `test_optimizer::optimizer_config_from_file`.
    pub async fn spawn_with(setup: impl FnOnce(&FrameworkPaths)) -> Self {
        let tmpdir = TempDir::new().expect("create temp dir");
        let paths = FrameworkPaths::under(tmpdir.path());
        // Make sure the hooks directory exists so `setup` can write into it.
        std::fs::create_dir_all(&paths.hooks).expect("create hooks dir");
        std::fs::create_dir_all(&paths.instructions).expect("create instructions dir");
        std::fs::create_dir_all(&paths.agents).expect("create agents dir");

        setup(&paths);

        let state = Arc::new(DaemonState::with_paths(&paths));
        let app = trusty_mpm::daemon::api::router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback port");
        let addr = listener.local_addr().expect("resolve bound addr");

        let handle = tokio::spawn(async move {
            // The server runs until the task is aborted on `Drop`.
            let _ = axum::serve(listener, app).await;
        });

        let url = format!("http://{addr}");
        wait_for_health(&url).await;

        Self {
            url,
            addr,
            paths,
            _tmpdir: tmpdir,
            handle,
        }
    }

    /// A plain `reqwest` client for talking to this daemon.
    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::new()
    }

    /// Build an absolute URL for `path` (which should start with `/`).
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }

    /// The temp directory's base path (parent of `.trusty-mpm`).
    ///
    /// Part of the public harness surface for scenarios that need to inspect
    /// the test's filesystem root directly.
    #[allow(dead_code)]
    pub fn base_dir(&self) -> &Path {
        self._tmpdir.path()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        // Stop serving so the port is released and the task does not leak.
        self.handle.abort();
    }
}

/// Poll `GET /health` until the daemon answers, or panic after ~2s.
///
/// Why: `axum::serve` accepts connections asynchronously; a test that fires a
/// request the instant after `spawn` returns can race the listener. Polling
/// `/health` gives a deterministic readiness signal.
/// What: issues `GET {base}/health` every 20ms for up to 2s, returning as soon
/// as a `200` arrives.
async fn wait_for_health(base: &str) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    let health = format!("{base}/health");
    loop {
        if let Ok(resp) = client.get(&health).send().await
            && resp.status().is_success()
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!("daemon at {base} did not become healthy within 2s");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Write `optimizer.toml` into a framework's `hooks` directory.
///
/// Why: several optimizer scenarios need to plant a policy file; centralising
/// the write keeps each test focused on its assertion.
/// What: writes `contents` to `paths.optimizer_config()`.
#[allow(dead_code)] // Used by test_optimizer.rs.
pub fn write_optimizer_toml(paths: &FrameworkPaths, contents: &str) {
    std::fs::write(paths.optimizer_config(), contents).expect("write optimizer.toml");
}

/// Write `overseer.toml` into a framework's `hooks` directory.
///
/// Why: the overseer scenarios plant an `enabled = true` policy before spawn.
/// What: writes `contents` to `paths.overseer_config()`.
#[allow(dead_code)] // Used by test_overseer.rs.
pub fn write_overseer_toml(paths: &FrameworkPaths, contents: &str) {
    std::fs::write(paths.overseer_config(), contents).expect("write overseer.toml");
}

/// A minimal three-level agent source set written into `dir`.
///
/// Why: the agent-deploy and instruction-pipeline scenarios need a real
/// `extends:` chain on disk. The bundled assets use uppercase filenames
/// (`BASE-AGENT.md`) while their `extends:` keys are lowercase — that mismatch
/// only resolves on case-insensitive filesystems, so the suite uses its own
/// lowercase-named sources to stay portable to case-sensitive CI hosts.
/// What: writes `base-agent.md`, `base-engineer.md`, and `engineer.md` forming
/// the chain `engineer -> base-engineer -> base-agent`.
/// Test: `test_agent_deploy.rs`, `test_instruction_pipeline.rs`.
#[allow(dead_code)] // Used by agent-deploy / pipeline scenarios.
pub fn write_agent_sources(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create source dir");
    std::fs::write(
        dir.join("base-agent.md"),
        "---\nname: base-agent\nrole: base\n---\n\n# Base Agent\n\nBASE-AGENT CONTENT\n",
    )
    .expect("write base-agent.md");
    std::fs::write(
        dir.join("base-engineer.md"),
        "---\nname: base-engineer\nrole: base-engineer\nextends: base-agent\n---\n\n\
         # Base Engineer\n\nBASE-ENGINEER CONTENT\n",
    )
    .expect("write base-engineer.md");
    std::fs::write(
        dir.join("engineer.md"),
        "---\nname: engineer\nrole: engineer\nextends: base-engineer\nmodel: sonnet\n---\n\n\
         # Engineer\n\nENGINEER CONTENT\n",
    )
    .expect("write engineer.md");
}
