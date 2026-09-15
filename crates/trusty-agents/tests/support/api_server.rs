//! `ApiServer` — integration-test fixture that spawns the compiled
//! `trusty-agents --api` HTTP server in a tempdir and provides convenience
//! methods for submitting tasks + polling for results.
//!
//! Why: End-to-end tests need to exercise the real HTTP surface (routing,
//! request parsing, subprocess spawning, response storage) — not just the
//! axum router used by unit tests via `oneshot`. Centralising the
//! "spawn the binary + discover its port + wait for /api/health" dance keeps
//! individual e2e tests trivial and avoids per-test boilerplate.
//! What: `ApiServer::spawn()` launches `trusty-agents --api --port 0` so the
//! kernel assigns the port inside the child's own `bind()` call (#7442: this
//! leaves no separate probe-then-bind step for anything else to race),
//! copies the repo-bundled `.trusty-agents/` config into a tempdir, then
//! reads the actual bound port back from the child's `http_addr` discovery
//! file (written by `api::server::routes::serve_with_config` right after it
//! binds) before polling `/api/health` until the endpoint answers (see
//! [`READY_TIMEOUT`]) and returning. `submit_task` POSTs `/api/task`,
//! `wait_for_task` polls `/api/task/:id` until the response leaves `running`
//! or a 120s timeout elapses.
//! Test: Exercised by `tests/api_e2e.rs`;
//! `assigned_port_is_never_free_after_it_is_reported` proves the port-pick
//! race is gone by construction.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

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

/// Interval between `/api/task/:id` polls.
const TASK_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Per-attempt bound on one `/api/task/:id` poll.
///
/// Why (#4488 review, M2): same reasoning as [`READY_PROBE_TIMEOUT`], which
/// the first pass applied only to the readiness loop. A status read that has
/// not answered in 30s is not slow, it is stuck, and leaving it unbounded lets
/// one poll consume the caller's entire multi-minute ceiling — the failure was
/// measured running past ten minutes at this site. Generous relative to the
/// work (a status read while the server runs a subprocess), tiny relative to
/// the 120-240s ceilings it protects, and a failed poll is retried anyway.
const TASK_POLL_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Raw bytes the child has written to stdout so far.
    ///
    /// Why (#4488): two reasons, both load-related. (1) The child's stdout and
    /// stderr are `piped()`; nothing used to read them, so a chatty startup
    /// could fill the ~64KB pipe buffer and block the child *before* it bound
    /// the port — a hang the old 5s wait reported only as an anonymous
    /// "did not become healthy". Draining continuously removes that failure
    /// mode. (2) When readiness genuinely fails, the child's own output is the
    /// diagnosis; without it every failure looks identical.
    ///
    /// Held as bytes, and kept separate from [`Self::stderr_buf`], so neither
    /// a multi-byte character split across two reads nor interleaving between
    /// the two streams can corrupt or misattribute the capture. Decoding
    /// happens once, lossily, at render time.
    stdout_buf: Arc<Mutex<Vec<u8>>>,
    /// Raw bytes the child has written to stderr so far. See
    /// [`Self::stdout_buf`].
    stderr_buf: Arc<Mutex<Vec<u8>>>,
}

impl ApiServer {
    /// Spawn `trusty-agents --api --port 0` in a tempdir with the
    /// repo-bundled `.trusty-agents/` config copied in, and wait for
    /// `/api/health` to return 200.
    ///
    /// Why: Tests need a real, isolated server they can hit over loopback.
    /// What: Copies config, spawns the binary with `--port 0` so the child
    /// itself picks the port (#7442), drains its stdout/stderr, reads the
    /// assigned port back from its `http_addr` discovery file, polls health.
    /// Test: Implicit — every e2e test calls this.
    pub async fn spawn() -> Result<Self> {
        let root = tempfile::tempdir().context("create tempdir")?;
        let dst_cfg = root.path().join(".trusty-agents");
        std::fs::create_dir_all(&dst_cfg)?;
        // Isolated `$HOME` — see the `_home` field doc for why this is
        // required, not optional.
        let home = tempfile::tempdir().context("create isolated HOME tempdir")?;

        let data_dir = home.path().join("data");
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_tagent"));

        // #7442: `--port 0` hands port assignment to the kernel inside the
        // child's own `TcpListener::bind` (see `api::server::routes::
        // serve_with_config`) instead of this fixture probing a number and
        // handing it to a second, independent bind — the gap between those
        // two binds was the TOCTOU. The actual port is read back below from
        // the `http_addr` discovery file the child writes right after it
        // binds.
        let mut child = Command::new(&binary)
            .current_dir(root.path())
            .env("HOME", home.path())
            .env("TAGENT_PROJECT_DIR", root.path())
            .env("TAGENT_CONFIG_DIR", &dst_cfg)
            .env("TAGENT_ASSISTANTS_DIR", home.path().join("assistants"))
            .env("TRUSTY_DATA_DIR_OVERRIDE", &data_dir)
            .env(
                "TRUSTY_MEMORY_SOCKET",
                home.path().join("unavailable-memory.sock"),
            )
            .env_remove("OPEN_MPM_PROJECT_DIR")
            .env_remove("OPEN_MPM_CONFIG_DIR")
            .arg("--api")
            .arg("--port")
            .arg("0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {} --api", binary.display()))?;

        // #4488: drain both pipes so a chatty startup cannot block the child
        // on a full pipe buffer, and so a readiness failure carries the
        // child's own explanation.
        let stdout_buf = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::new()));
        if let Some(out) = child.stdout.take() {
            spawn_drain(out, Arc::clone(&stdout_buf));
        }
        if let Some(err) = child.stderr.take() {
            spawn_drain(err, Arc::clone(&stderr_buf));
        }

        let http_addr_path = data_dir.join("trusty-agents").join("http_addr");
        let port = wait_for_assigned_port(
            &mut child,
            &http_addr_path,
            READY_TIMEOUT,
            &stdout_buf,
            &stderr_buf,
        )
        .await?;

        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = Self {
            _root: root,
            _home: home,
            port,
            child: Some(child),
            base_url,
            stdout_buf,
            stderr_buf,
        };

        server.wait_for_health(READY_TIMEOUT).await?;
        Ok(server)
    }

    /// Snapshot of everything the child has printed so far, both streams.
    fn captured_output(&self) -> String {
        render_captured(&self.stdout_buf, &self.stderr_buf)
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
                     (after {:?})\n{}",
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
                     still running)\n{}",
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
    /// What: Polls every [`TASK_POLL_INTERVAL`]; returns the final JSON payload.
    #[allow(dead_code)]
    pub async fn wait_for_task(&mut self, id: &str) -> Result<Value> {
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
    ///
    /// #4488 review (M2, LOW): this loop gets the same two guarantees
    /// [`Self::wait_for_health`] has, because the reasoning is identical.
    /// Each poll is individually bounded by [`TASK_POLL_TIMEOUT`] — an
    /// unbounded client here was measured hanging past ten minutes, which
    /// silently converts a bounded retry loop back into one unbounded wait —
    /// and a child that has died is detected via `try_wait` and reported at
    /// once, rather than burning the caller's full multi-minute ceiling
    /// polling a socket nobody is listening on.
    pub async fn wait_for_task_with_timeout(
        &mut self,
        id: &str,
        timeout: Duration,
    ) -> Result<Value> {
        let url = format!("{}/api/task/{id}", self.base_url);
        let client = reqwest::Client::builder()
            .timeout(TASK_POLL_TIMEOUT)
            .build()
            .context("build task-poll client")?;
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
            if let Some(child) = self.child.as_mut()
                && let Some(status) = child.try_wait().context("poll api server child")?
            {
                return Err(anyhow!(
                    "api server exited with {status} while task {id} was still \
                     running (after {:?}); {last}\n{}",
                    start.elapsed(),
                    self.captured_output()
                ));
            }
            if start.elapsed() > timeout {
                return Err(anyhow!(
                    "task {id} did not finish within {timeout:?}; {last}"
                ));
            }
            tokio::time::sleep(TASK_POLL_INTERVAL).await;
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

/// Continuously drain one child pipe into `sink` until EOF.
///
/// Why (#4488 review, M1): the first version of this drain read LINES —
/// `while let Ok(Some(line)) = lines.next_line().await` — which ends the loop
/// silently on any `io::Error`. tokio yields
/// `Err(InvalidData, "stream did not contain valid UTF-8")` for a single
/// non-UTF-8 byte (`tokio/src/io/util/read_line.rs`), after which the pipe
/// would never be read again. That re-armed the exact full-buffer block this
/// drain exists to prevent, and truncated the captured diagnostic with no sign
/// that truncation had happened — a short log that reads as a complete one is
/// worse than no log.
///
/// Draining BYTES removes the failure mode at its source rather than handling
/// it: there is no such thing as malformed input for a byte read, so the only
/// exits from this loop are real EOF and a real I/O error. The bytes are
/// decoded lossily only at render time ([`render_stream`]), so a multi-byte
/// character split across two reads is reassembled rather than mangled. An
/// I/O error still stops the loop — nothing useful follows a broken pipe — but
/// it appends a visible marker first, so a truncated capture always says so.
fn spawn_drain<R>(pipe: R, sink: Arc<Mutex<Vec<u8>>>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut pipe = pipe;
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => lock_recover(&sink).extend_from_slice(&chunk[..n]),
                Err(e) => {
                    let marker = format!("\n<<< drain stopped: {e}; capture truncated here >>>\n");
                    lock_recover(&sink).extend_from_slice(marker.as_bytes());
                    break;
                }
            }
        }
    });
}

/// Lock a capture buffer, recovering from poisoning.
///
/// A drain task that panicked must not also blind the readiness failure
/// message that is the whole point of capturing this output.
fn lock_recover(sink: &Arc<Mutex<Vec<u8>>>) -> std::sync::MutexGuard<'_, Vec<u8>> {
    match sink.lock() {
        Ok(b) => b,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Render one captured stream as `--- child <label> ---` plus its (lossily
/// decoded) contents, or a one-line note when the stream produced nothing.
fn render_stream(label: &str, sink: &Arc<Mutex<Vec<u8>>>) -> String {
    let buf = lock_recover(sink);
    if buf.is_empty() {
        return format!("--- child {label}: empty ---\n");
    }
    format!(
        "--- child {label} ---\n{}\n",
        String::from_utf8_lossy(&buf).trim_end()
    )
}

/// Render both captured child streams as one string — shared by
/// [`ApiServer::captured_output`] and [`wait_for_assigned_port`], neither of
/// which has a full `&ApiServer` to call the other's method through before
/// the port (or the health check) is known. See [`render_stream`] for the
/// per-stream, empty-vs-content shape.
fn render_captured(stdout_buf: &Arc<Mutex<Vec<u8>>>, stderr_buf: &Arc<Mutex<Vec<u8>>>) -> String {
    format!(
        "{}{}",
        render_stream("stdout", stdout_buf),
        render_stream("stderr", stderr_buf)
    )
}

/// Poll the child's `http_addr` discovery file (written by
/// `api::server::routes::serve_with_config` right after it binds) until it
/// appears, then parse the OS-assigned port back out of it.
///
/// Why (#7442): replaces the retired `pick_free_port`, which bound port 0,
/// read the number back, and then DROPPED the listener before handing the
/// bare number to a second, independent bind in the child — the drop-then-
/// rebind gap was a real TOCTOU window in which anything else (another test,
/// another process) could steal the exact same port number before the child
/// claimed it, surfacing as `Address already in use (os error 48)` under
/// load. Reading the port back from the child's OWN still-live listener
/// removes the window by construction: the number is never handed out
/// before something is already bound to it, and nothing drops that listener
/// until the server shuts down.
/// What: polls every [`READY_POLL_INTERVAL`] for `path` to exist and parse
/// as `host:port`; mirrors [`ApiServer::wait_for_health`] in reporting a
/// child that exited early via `try_wait` immediately, rather than waiting
/// out the full `timeout`.
/// Test: `assigned_port_is_never_free_after_it_is_reported`.
async fn wait_for_assigned_port(
    child: &mut Child,
    path: &Path,
    timeout: Duration,
    stdout_buf: &Arc<Mutex<Vec<u8>>>,
    stderr_buf: &Arc<Mutex<Vec<u8>>>,
) -> Result<u16> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("poll api server child")? {
            return Err(anyhow!(
                "api server exited with {status} before writing its http_addr \
                 discovery file (after {:?})\n{}",
                start.elapsed(),
                render_captured(stdout_buf, stderr_buf)
            ));
        }
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Some(port) = contents
                .trim()
                .rsplit(':')
                .next()
                .and_then(|p| p.parse::<u16>().ok())
        {
            return Ok(port);
        }
        if start.elapsed() > timeout {
            return Err(anyhow!(
                "api server did not write {} within {timeout:?} (child still \
                 running)\n{}",
                path.display(),
                render_captured(stdout_buf, stderr_buf)
            ));
        }
        tokio::time::sleep(READY_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;

    /// #4488 review (M1): the drain must survive a non-UTF-8 byte in the
    /// child's output and keep reading past it.
    ///
    /// This is the exact regression the review caught. The first version read
    /// LINES, and tokio's `next_line` returns
    /// `Err(InvalidData, "stream did not contain valid UTF-8")` for the `\xfe`
    /// below — silently ending the drain, so `after-the-bad-byte` never
    /// reached the capture and, worse, the pipe was never read again. Asserting
    /// on the text AFTER the invalid byte is what distinguishes a drain that
    /// survived from one that stopped: a line-based drain passes the `before`
    /// half of this test and fails the `after` half.
    #[tokio::test]
    async fn drain_survives_invalid_utf8_and_keeps_reading() {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"printf 'before-the-bad-byte\n\376\377\n'; printf 'after-the-bad-byte\n'"#)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn /bin/sh");

        let sink = Arc::new(Mutex::new(Vec::new()));
        spawn_drain(
            child.stdout.take().expect("piped stdout"),
            Arc::clone(&sink),
        );
        child.wait().await.expect("child exits");

        // The drain runs in its own task, so wait for the CONDITION (the text
        // after the bad byte has landed) rather than guessing at a delay — but
        // under a deadline, so a drain that stopped FAILS instead of spinning.
        // The first draft of this test omitted the deadline and hung the suite
        // for 60s+ against the line-based drain, which is the same
        // wait-forever defect in miniature that #4488 is about.
        let deadline = Instant::now() + Duration::from_secs(10);
        let rendered = loop {
            let rendered = render_stream("stdout", &sink);
            if rendered.contains("after-the-bad-byte") {
                break rendered;
            }
            assert!(
                Instant::now() < deadline,
                "drain stopped at the invalid byte instead of reading past it — \
                 nothing after it was ever captured: {rendered}"
            );
            tokio::task::yield_now().await;
        };

        assert!(
            rendered.contains("before-the-bad-byte"),
            "output before the invalid byte must be captured: {rendered}"
        );
        assert!(
            rendered.contains('\u{fffd}'),
            "the invalid byte must survive as a replacement char, not abort the \
             drain: {rendered}"
        );
    }

    /// An empty stream renders as an explicit note, never as blank space that
    /// could be mistaken for "the child said nothing useful".
    #[tokio::test]
    async fn render_stream_marks_an_empty_capture() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            render_stream("stdout", &sink),
            "--- child stdout: empty ---\n"
        );
    }
}

#[cfg(test)]
mod port_toctou_tests {
    use super::*;

    /// #7442: proves the port-selection race the retired `pick_free_port`
    /// left open is gone by construction, run N=20 times to rule out one
    /// lucky iteration rather than trusting a single pass.
    ///
    /// Each iteration stands in for the real child exactly as `spawn()` uses
    /// it: bind port 0, keep that listener alive (never drop it, matching
    /// `serve_with_config` which serves on the same listener it bound), and
    /// publish the port through a discovery file — then reads it back with
    /// the production [`wait_for_assigned_port`] helper.
    ///
    /// The assertion that actually proves the race is gone: immediately
    /// after the port is reported, a fresh, independent bind attempt at that
    /// exact port MUST fail with `AddrInUse`, because the stand-in listener
    /// above is still holding it. That is the by-construction guarantee —
    /// the port is never handed out before something is bound to it, and
    /// nothing frees it out from under a caller.
    ///
    /// How a wrong implementation fails this: the old `pick_free_port`
    /// pattern (bind 0, read the number, DROP the listener, hand the bare
    /// number to a second bind) leaves the number genuinely free the instant
    /// it is reported. Reintroducing that pattern into
    /// [`wait_for_assigned_port`]'s source — dropping `held` before writing
    /// the discovery file — turns the `rebind.is_err()` assertion false:
    /// the independent bind below would succeed, because nothing would still
    /// hold the port.
    /// Test: itself.
    #[tokio::test]
    async fn assigned_port_is_never_free_after_it_is_reported() {
        for iteration in 0..20 {
            let tmp = tempfile::tempdir().expect("tempdir for discovery file");
            let http_addr_path = tmp.path().join("http_addr");

            // Stand-in for the real child: binds 0 itself and keeps the
            // listener alive for the rest of the iteration, exactly the
            // order of operations `serve_with_config` uses (bind, then
            // publish, then keep serving).
            let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stand-in listener");
            let held_port = held.local_addr().expect("local_addr").port();
            std::fs::write(&http_addr_path, format!("127.0.0.1:{held_port}"))
                .expect("write discovery file");

            // A real (harmless) child process so `wait_for_assigned_port`'s
            // `try_wait` guard has something to poll against.
            let mut child = Command::new("sleep")
                .arg("5")
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep stand-in");
            let stdout_buf = Arc::new(Mutex::new(Vec::new()));
            let stderr_buf = Arc::new(Mutex::new(Vec::new()));

            let reported = wait_for_assigned_port(
                &mut child,
                &http_addr_path,
                Duration::from_secs(5),
                &stdout_buf,
                &stderr_buf,
            )
            .await
            .unwrap_or_else(|e| panic!("iteration {iteration}: {e}"));
            assert_eq!(
                reported, held_port,
                "iteration {iteration}: reported the wrong port"
            );

            let rebind = std::net::TcpListener::bind(("127.0.0.1", reported));
            assert!(
                rebind.is_err(),
                "iteration {iteration}: port {reported} was rebindable immediately \
                 after being reported — it was not actually held, reproducing the \
                 #7442 TOCTOU"
            );

            drop(held);
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}
