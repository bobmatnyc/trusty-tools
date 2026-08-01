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
//! tempdir as cwd, and polls `/api/health` until the endpoint answers (see
//! [`READY_TIMEOUT`]) before returning. `submit_task` POSTs `/api/task`,
//! `wait_for_task` polls `/api/task/:id` until the response leaves `running`
//! or a 120s timeout elapses.
//! Test: Exercised by `tests/api_e2e.rs`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

/// Ceiling on how long [`ApiServer::spawn`] waits for `/api/health` to answer.
///
/// Why (#4488): this used to be 5s — a latency BUDGET, not a bound. The child
/// is a freshly `exec`'d debug binary that runs `runtime::startup::
/// run_startup_init` (deploying ~30 bundled agent files into its isolated
/// `$HOME`) before it ever binds the port, so 5s is a bet on the machine being
/// idle. It held on CI's single-tenant runner and lost locally at load 22-38,
/// where `api_e2e.rs` went red 3/3. The wait itself was already condition-based
/// polling; only the ceiling was a guess, so it is now sized as a genuine
/// "something is wrong" bound rather than an expected-latency one. Overshooting
/// costs nothing on the happy path: the loop returns the instant health answers,
/// and a child that dies fails immediately via `try_wait` rather than sitting
/// out the ceiling.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Interval between `/api/health` polls.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Per-attempt bound on one `/api/health` request.
///
/// Why: without it a single request that connects and then stalls would burn
/// the whole [`READY_TIMEOUT`] in one attempt, turning a bounded retry loop
/// back into a single fixed wait.
const READY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// One-shot test harness: a running `trusty-agents --api` child + its base URL.
pub struct ApiServer {
    /// Tempdir containing the bundled `.trusty-agents/` config. Held so it lives
    /// as long as the server child does.
    _root: TempDir,
    /// Isolated `$HOME` for the child process. Held so it lives as long as
    /// the server does.
    ///
    /// Why (issue #3429 code-critic HIGH follow-up): `runtime::startup::
    /// run_startup_init` now unconditionally deploys the bundled agent
    /// roster to `$HOME/.trusty-agents/agents/` before the `--api` mode
    /// check even runs. Without this override the spawned child inherited
    /// whatever `$HOME` the `cargo test` process itself ran under — on a
    /// real dev machine, that's the developer's actual home directory —
    /// so every `cargo test -p trusty-agents --test api_e2e` run was
    /// writing ~30 real files into it. Never referenced after construction;
    /// kept alive purely so the tempdir isn't cleaned up mid-run.
    _home: TempDir,
    port: u16,
    child: Option<Child>,
    base_url: String,
    /// Everything the child has written to stdout/stderr so far, line by line.
    ///
    /// Why (#4488): two reasons, both load-related. (1) The child's stdout and
    /// stderr are `piped()`; nothing used to read them, so a chatty startup
    /// could fill the ~64KB pipe buffer and block the child *before* it bound
    /// the port — a hang the old 5s wait reported only as an anonymous
    /// "did not become healthy". Draining continuously removes that failure
    /// mode entirely. (2) When readiness genuinely fails, the child's own
    /// output is the diagnosis; without it every failure looks identical.
    output: Arc<Mutex<String>>,
}

impl ApiServer {
    /// Spawn `trusty-agents --api --port <free_port>` in a tempdir with the
    /// repo-bundled `.trusty-agents/` config copied in, and wait for
    /// `/api/health` to return 200.
    ///
    /// Why: Tests need a real, isolated server they can hit over loopback.
    /// What: Picks a free port via the bind-to-0 trick, copies config,
    /// spawns the binary, drains its stdout/stderr, polls health.
    /// Test: Implicit — every e2e test calls this.
    pub async fn spawn() -> Result<Self> {
        let root = tempfile::tempdir().context("create tempdir")?;
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_cfg = manifest.join(".trusty-agents");
        let dst_cfg = root.path().join(".trusty-agents");
        copy_dir_recursive(&src_cfg, &dst_cfg).context("copy .trusty-agents")?;
        // Isolated `$HOME` — see the `_home` field doc for why this is
        // required, not optional.
        let home = tempfile::tempdir().context("create isolated HOME tempdir")?;

        let port = pick_free_port().context("pick free port")?;
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_tagent"));

        let mut child = Command::new(&binary)
            .current_dir(root.path())
            .env("HOME", home.path())
            .arg("--api")
            .arg("--port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {} --api", binary.display()))?;

        // #4488: drain both pipes so a chatty startup cannot block the child
        // on a full pipe buffer, and so a readiness failure carries the
        // child's own explanation.
        let output = Arc::new(Mutex::new(String::new()));
        if let Some(out) = child.stdout.take() {
            spawn_stdout_drain(out, Arc::clone(&output));
        }
        if let Some(err) = child.stderr.take() {
            spawn_stderr_drain(err, Arc::clone(&output));
        }

        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = Self {
            _root: root,
            _home: home,
            port,
            child: Some(child),
            base_url,
            output,
        };

        server.wait_for_health(READY_TIMEOUT).await?;
        Ok(server)
    }

    /// Snapshot of everything the child has printed so far.
    fn captured_output(&self) -> String {
        match self.output.lock() {
            Ok(buf) if buf.is_empty() => "<child produced no output>".to_string(),
            Ok(buf) => buf.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
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

    /// Poll `GET /api/health` until it returns 200, the child dies, or
    /// `timeout` elapses.
    ///
    /// Why: The child is spawned async; we need a real readiness SIGNAL before
    /// the test issues requests, otherwise tests race and fail intermittently.
    /// The condition being waited on is "the endpoint answers" — never "N
    /// seconds have passed" (#4488).
    /// What: Polls every [`READY_POLL_INTERVAL`] with one bounded client so
    /// DNS / pool state is not a confound. Each pass first asks `try_wait`
    /// whether the child is still alive: a child that exited can never become
    /// healthy, so that case returns immediately with the exit status and the
    /// child's captured output instead of waiting out the ceiling and
    /// reporting a bare timeout. The ceiling is therefore only ever reached by
    /// a child that is alive but wedged.
    /// Test: Implicit — used by `spawn()`.
    async fn wait_for_health(&mut self, timeout: Duration) -> Result<()> {
        let url = format!("{}/api/health", self.base_url);
        let client = reqwest::Client::builder()
            .timeout(READY_PROBE_TIMEOUT)
            .build()
            .context("build health-probe client")?;
        let start = Instant::now();
        loop {
            if let Some(child) = self.child.as_mut()
                && let Some(status) = child.try_wait().context("poll api server child")?
            {
                return Err(anyhow!(
                    "api server exited with {status} before answering {url} \
                     (after {:?})\n--- child output ---\n{}",
                    start.elapsed(),
                    self.captured_output()
                ));
            }
            if let Ok(resp) = client.get(&url).send().await
                && resp.status().is_success()
            {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(anyhow!(
                    "api server did not answer {url} within {timeout:?} (child \
                     still running)\n--- child output ---\n{}",
                    self.captured_output()
                ));
            }
            tokio::time::sleep(READY_POLL_INTERVAL).await;
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
    ///
    /// #4488: a single failed GET no longer aborts the wait. The loop's
    /// condition is "the task left `running`"; a transport hiccup on one poll
    /// (the server is busy running the task's subprocess, which is exactly
    /// when the machine is most loaded) says nothing about that condition, so
    /// it is retried like any other not-yet-satisfied poll. The last failure
    /// is carried into the timeout message so a genuinely broken server still
    /// reports why.
    pub async fn wait_for_task_with_timeout(&self, id: &str, timeout: Duration) -> Result<Value> {
        let url = format!("{}/api/task/{id}", self.base_url);
        let client = reqwest::Client::new();
        let start = Instant::now();
        // Assigned on every path through the loop body before the timeout
        // check reads it, so it needs no (dead) initial value.
        let mut last: String;
        loop {
            match poll_task_once(&client, &url).await {
                Ok(v) => {
                    if v["status"].as_str().unwrap_or("") != "running" {
                        return Ok(v);
                    }
                    last = format!("last body: {v}");
                }
                Err(e) => last = format!("last poll error: {e:#}"),
            }
            if start.elapsed() > timeout {
                return Err(anyhow!(
                    "task {id} did not finish within {timeout:?}; {last}"
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

/// One `GET /api/task/:id` attempt, decoded as JSON.
///
/// Why: factored out of [`ApiServer::wait_for_task_with_timeout`] so the
/// retry loop there has a single fallible unit to match on, instead of two
/// `?`s that each turn a transient hiccup into a hard test failure (#4488).
async fn poll_task_once(client: &reqwest::Client, url: &str) -> Result<Value> {
    let resp = client.get(url).send().await.context("GET /api/task/:id")?;
    resp.json().await.context("parse task body")
}

/// Continuously append the child's stdout lines to `sink`.
fn spawn_stdout_drain(pipe: ChildStdout, sink: Arc<Mutex<String>>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            append_line(&sink, "out", &line);
        }
    });
}

/// Continuously append the child's stderr lines to `sink`.
fn spawn_stderr_drain(pipe: ChildStderr, sink: Arc<Mutex<String>>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            append_line(&sink, "err", &line);
        }
    });
}

/// Append one `[stream] line` to the shared capture buffer.
///
/// A poisoned mutex is recovered from rather than propagated: a drain task
/// that panicked must not also blind the readiness failure message that is
/// the whole point of capturing this output.
fn append_line(sink: &Arc<Mutex<String>>, stream: &str, line: &str) {
    let mut buf = match sink.lock() {
        Ok(b) => b,
        Err(poisoned) => poisoned.into_inner(),
    };
    buf.push('[');
    buf.push_str(stream);
    buf.push_str("] ");
    buf.push_str(line);
    buf.push('\n');
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
