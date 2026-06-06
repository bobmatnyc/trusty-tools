//! `ApiServer` — integration-test fixture that spawns the compiled
//! `trusty-agents --api` HTTP server in a tempdir and provides convenience
//! methods for submitting tasks + polling for results.
//!
//! Why: End-to-end tests need to exercise the real HTTP surface (routing,
//! request parsing, subprocess spawning, response storage) — not just the
//! axum router used by unit tests via `oneshot`. Centralising the
//! "pick a free port + spawn the binary + wait for /api/health" dance keeps
//! individual e2e tests trivial and avoids per-test boilerplate.
//! What: `ApiServer::spawn()` picks a free TCP port (by binding to port 0
//! and reading back the assignment), copies the repo-bundled `.trusty-agents/`
//! config into a tempdir, spawns `trusty-agents --api --port <port>` with that
//! tempdir as cwd, and polls `/api/health` for up to 5s before returning.
//! `submit_task` POSTs `/api/task`, `wait_for_task` polls `/api/task/:id`
//! until the response leaves `running` or a 120s timeout elapses.
//! Test: Exercised by `tests/api_e2e.rs`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tempfile::TempDir;
use tokio::process::{Child, Command};

/// One-shot test harness: a running `trusty-agents --api` child + its base URL.
pub struct ApiServer {
    /// Tempdir containing the bundled `.trusty-agents/` config. Held so it lives
    /// as long as the server child does.
    _root: TempDir,
    port: u16,
    child: Option<Child>,
    base_url: String,
}

impl ApiServer {
    /// Spawn `trusty-agents --api --port <free_port>` in a tempdir with the
    /// repo-bundled `.trusty-agents/` config copied in, and wait up to 5s for
    /// `/api/health` to return 200.
    ///
    /// Why: Tests need a real, isolated server they can hit over loopback.
    /// What: Picks a free port via the bind-to-0 trick, copies config,
    /// spawns the binary, polls health.
    /// Test: Implicit — every e2e test calls this.
    pub async fn spawn() -> Result<Self> {
        let root = tempfile::tempdir().context("create tempdir")?;
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_cfg = manifest.join(".trusty-agents");
        let dst_cfg = root.path().join(".trusty-agents");
        copy_dir_recursive(&src_cfg, &dst_cfg).context("copy .trusty-agents")?;

        let port = pick_free_port().context("pick free port")?;
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_tagent"));

        let child = Command::new(&binary)
            .current_dir(root.path())
            .arg("--api")
            .arg("--port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {} --api", binary.display()))?;

        let base_url = format!("http://127.0.0.1:{port}");
        let server = Self {
            _root: root,
            port,
            child: Some(child),
            base_url,
        };

        server.wait_for_health(Duration::from_secs(5)).await?;
        Ok(server)
    }

    /// Base URL of the running server, e.g. `http://127.0.0.1:54321`.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Listening port — exposed for tests that want to sanity-check it.
    #[allow(dead_code)]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Poll `GET /api/health` until it returns 200 or `timeout` elapses.
    ///
    /// Why: The child is spawned async; we need a deterministic readiness
    /// signal before the test issues real requests, otherwise tests race
    /// and fail intermittently.
    /// What: Polls every 50ms with a fresh `reqwest::Client` so DNS / pool
    /// state is not a confound.
    /// Test: Implicit — used by `spawn()`.
    async fn wait_for_health(&self, timeout: Duration) -> Result<()> {
        let url = format!("{}/api/health", self.base_url);
        let client = reqwest::Client::new();
        let start = Instant::now();
        loop {
            if let Ok(resp) = client.get(&url).send().await
                && resp.status().is_success()
            {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(anyhow!(
                    "api server did not become healthy at {url} within {:?}",
                    timeout
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// POST a `TaskRequest` body with just the `task` field set, returning
    /// the server-assigned task ID.
    pub async fn submit_task(&self, task: &str) -> Result<String> {
        self.submit_task_json(serde_json::json!({ "task": task }))
            .await
    }

    /// POST an arbitrary JSON body to `/api/task` and return the task id.
    ///
    /// Why: Lets individual tests exercise `agent`, `workflow`, `out_dir`,
    /// or `project_path` fields without bloating `submit_task`.
    pub async fn submit_task_json(&self, body: Value) -> Result<String> {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/api/task", self.base_url))
            .json(&body)
            .send()
            .await
            .context("POST /api/task")?;
        let status = resp.status();
        let v: Value = resp.json().await.context("parse POST /api/task body")?;
        if !status.is_success() && status.as_u16() != 202 {
            return Err(anyhow!("POST /api/task returned {status}: {v}"));
        }
        v["id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("POST /api/task body missing `id`: {v}"))
    }

    /// Poll `GET /api/task/:id` until status leaves `"running"` or
    /// `timeout` elapses (default 120s).
    ///
    /// Why: Workflow tasks run async; tests need a single helper that
    /// blocks until the background subprocess has emitted a terminal
    /// `PmResponse`.
    /// What: Polls every 250ms; returns the final JSON payload.
    pub async fn wait_for_task(&self, id: &str) -> Result<Value> {
        self.wait_for_task_with_timeout(id, Duration::from_secs(120))
            .await
    }

    /// As `wait_for_task` but with a caller-specified timeout.
    pub async fn wait_for_task_with_timeout(&self, id: &str, timeout: Duration) -> Result<Value> {
        let url = format!("{}/api/task/{id}", self.base_url);
        let client = reqwest::Client::new();
        let start = Instant::now();
        loop {
            let resp = client.get(&url).send().await.context("GET /api/task/:id")?;
            let v: Value = resp.json().await.context("parse task body")?;
            let status = v["status"].as_str().unwrap_or("");
            if status != "running" {
                return Ok(v);
            }
            if start.elapsed() > timeout {
                return Err(anyhow!(
                    "task {id} did not finish within {:?}; last body: {v}",
                    timeout
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Drop for ApiServer {
    /// Kill the child process so leftover servers don't pile up between
    /// tests or after a failing assertion.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // `kill_on_drop(true)` already handles this, but call start_kill
            // explicitly to be defensive in case tokio's drop ordering
            // changes.
            let _ = child.start_kill();
        }
    }
}

/// Bind a TCP listener on `127.0.0.1:0`, read the assigned port, and drop
/// the listener. The port is then likely free for a follow-on bind in the
/// child process.
///
/// Why: There's a small race window between dropping the listener and the
/// child binding, but in practice this is the standard test pattern (used
/// by countless Rust HTTP test harnesses) and far simpler than adding the
/// `portpicker` crate. We avoid the `49152..65535` random-pick approach
/// because it can collide with already-bound ports.
fn pick_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Recursive directory copy that skips runtime `state/` directories.
///
/// Why: Mirrors `tests/support/project.rs::copy_dir_recursive` rather than
/// pulling in `fs_extra`. Kept private to this module to avoid ordering
/// concerns with the sibling helper. The repo's bundled `.trusty-agents/` ships
/// with a populated `state/` (build.json, sessions, tasks.json from prior
/// runs) which must NOT leak into test fixtures — otherwise tests that
/// depend on a clean startup state (e.g. `test_tasks_list_starts_empty`)
/// observe persisted tasks left over from previous developer runs (#212).
/// What: Walks `src` with a manual stack, mirroring directories and copying
/// files into `dst`. Top-level entries named `state` are skipped so the
/// API server starts with no persisted task history.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        return Err(anyhow!("source config dir not found: {}", src.display()));
    }
    std::fs::create_dir_all(dst)?;
    let mut stack: Vec<(PathBuf, PathBuf)> = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((s, d)) = stack.pop() {
        // Only skip the top-level `state/` directory directly under `.trusty-agents/`.
        let is_top_level = s == src;
        for entry in std::fs::read_dir(&s)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let from = entry.path();
            let name = entry.file_name();
            if is_top_level && name == "state" {
                // Skip persisted runtime state; tests must start clean.
                continue;
            }
            let to = d.join(name);
            if ft.is_dir() {
                std::fs::create_dir_all(&to)?;
                stack.push((from, to));
            } else if ft.is_file() {
                std::fs::copy(&from, &to)?;
            }
            // Symlinks/other: skip.
        }
    }
    Ok(())
}
