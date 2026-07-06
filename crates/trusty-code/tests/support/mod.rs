//! Process/protocol plumbing shared by `tests/session_e2e.rs` and (#2056)
//! `tests/task_e2e.rs`.
//!
//! Why: driving the REAL `tcode` binary over its real stdio/HTTP surface
//! needs a bit of scaffolding (spawn, NDJSON line I/O with timeouts, parse
//! helpers for responses vs. server-initiated notifications, SSE body
//! reading) that has nothing to do with the session-lifecycle assertions
//! themselves. Keeping it in `tests/support/mod.rs` (not a top-level
//! `tests/*.rs` file, so cargo does not treat it as its own test binary)
//! keeps `session_e2e.rs`/`task_e2e.rs` readable as pure black-box scripts.
//! What: [`StdioSession`] (spawn + NDJSON request/response/notification
//! I/O over real pipes — [`StdioSession::spawn_with_mock_llm`] additionally
//! roots the daemon at a given project dir and sets `TCODE_MOCK_LLM=echo`
//! for #2056's offline task-execution coverage), [`HttpDaemon`]/
//! [`spawn_http_daemon`] (spawn + discover the bound HTTP address from
//! stderr), [`find_response`]/[`find_session_event`] (classify a raw NDJSON
//! line), [`open_sse`]/[`read_sse_until`] (read one never-terminating SSE
//! connection in stages), [`parse_sse_frames`] (split an SSE body into its
//! JSON `data:` frames), and [`assert_envelopes_contiguous`] (#2055: the
//! shared seq/field-presence check both the STDIO and HTTP/SSE e2e
//! scenarios run against the SAME `SessionEventEnvelope` shape).
//!
//! `#![allow(dead_code)]`: each `tests/*.rs` file compiles `mod support;` as
//! its OWN independent crate (cargo's per-integration-test-binary model), so
//! any helper one file doesn't use (e.g. `task_e2e.rs` never touches the
//! HTTP/SSE helpers `session_e2e.rs` needs) is flagged dead in THAT binary —
//! a false positive from splitting one shared file across two consumers,
//! not a real unused-code issue.
#![allow(dead_code)]

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

/// Per-read/response timeout used throughout — generous enough for a debug
/// build under CI load, tight enough that a genuine protocol bug fails the
/// test in seconds rather than hanging the suite.
const WIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Provision a throwaway project with `.claude/agents/{pm,python-engineer}.toml`.
///
/// Why: shared by `tests/task_e2e.rs` (drives the daemon directly) and
/// (#2060) `tests/cli_e2e.rs` (drives the `tcode` CLI, which spawns its own
/// nested daemon) — both need the SAME minimal two-agent fixture for
/// `TCODE_MOCK_LLM=echo`'s scripted PM -> engineer delegation to resolve.
pub fn project_with_agents() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("project tempdir");
    let agents = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir agents");
    std::fs::write(
        agents.join("pm.toml"),
        "[agent]\nname = \"pm\"\nmodel = \"openai/gpt-4o-mini\"\n[system_prompt]\ncontent = \"You are the PM. Delegate work to python-engineer.\"\n",
    )
    .expect("write pm.toml");
    std::fs::write(
        agents.join("python-engineer.toml"),
        "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n[system_prompt]\ncontent = \"You are a Python engineer.\"\n",
    )
    .expect("write python-engineer.toml");
    tmp
}

/// A running `tcode serve --stdio` subprocess with line-buffered stdin/stdout.
///
/// Why: the STDIO half of the API-driven e2e coverage — every call in
/// `session_lifecycle_over_stdio` goes through this, writing/reading actual
/// NDJSON bytes over the child's real pipes.
pub struct StdioSession {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl StdioSession {
    /// Spawn the real `tcode` binary in `--stdio` mode.
    ///
    /// Why: `env!("CARGO_BIN_EXE_tcode")` resolves to the freshly-built
    /// binary for this exact test run — no separate install step needed.
    /// What: `--project .` (any existing directory works; `session.*` does
    /// not require a `.claude/` root), stdin/stdout piped, stderr discarded
    /// (logs aren't asserted on here), `kill_on_drop` so a panicking test
    /// still reaps the child instead of leaking a process.
    pub fn spawn() -> Self {
        Self::spawn_inner(std::path::Path::new("."), false)
    }

    /// Spawn `tcode serve --stdio` rooted at `project`, with
    /// `TCODE_MOCK_LLM=echo` set in the child's environment so `task.run`
    /// drives #2056's deterministic offline `EchoLlmClient` instead of
    /// requiring a live `OPENROUTER_API_KEY` — the mandatory offline
    /// black-box path for `tests/task_e2e.rs`.
    pub fn spawn_with_mock_llm(project: &std::path::Path) -> Self {
        Self::spawn_inner(project, true)
    }

    fn spawn_inner(project: &std::path::Path, mock_llm: bool) -> Self {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_tcode"));
        cmd.arg("serve")
            .arg("--project")
            .arg(project)
            .arg("--stdio");
        if mock_llm {
            cmd.env(
                trusty_code::task::mock_llm::MOCK_LLM_ENV,
                trusty_code::task::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `tcode serve --stdio`");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let lines = BufReader::new(stdout).lines();
        Self {
            child,
            stdin,
            lines,
        }
    }

    /// Write one JSON-RPC 2.0 request line; does not read a response.
    ///
    /// Why: `session.send`'s response and its streamed notification can
    /// arrive in either order on the wire (see `session_e2e.rs`), so that
    /// call site needs to write first and read separately via
    /// `read_lines`, rather than going through [`Self::call`].
    pub async fn write_request(&mut self, id: i64, method: &str, params: Value) {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_string(&req).expect("serialise request");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("write request line");
        self.stdin.flush().await.expect("flush stdin");
    }

    /// Write a request, then read lines (skipping any interleaved
    /// notifications) until the matching response arrives.
    ///
    /// Why: the common case — one request, one response, no observable
    /// side-stream traffic to correlate.
    /// What: panics after [`WIRE_TIMEOUT`] if no matching response shows up.
    pub async fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.write_request(id, method, params).await;
        timeout(WIRE_TIMEOUT, async {
            loop {
                let line = self.next_line().await;
                if let Some(resp) = find_response(&line, id) {
                    return resp;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for a response to id={id} ({method})"))
    }

    /// Read raw NDJSON lines until `max_lines` have been collected or
    /// [`WIRE_TIMEOUT`] elapses, whichever comes first.
    ///
    /// Why: `session.send`'s test needs to observe BOTH its response and a
    /// streamed notification, which may arrive in either order — reading a
    /// bounded batch and classifying each line afterward (via
    /// [`find_response`]/[`find_session_event`]) is simpler and more robust
    /// than trying to predict the interleaving.
    pub async fn read_lines(&mut self, max_lines: usize) -> Vec<String> {
        let mut collected = Vec::new();
        let _ = timeout(WIRE_TIMEOUT, async {
            while collected.len() < max_lines {
                collected.push(self.next_line().await);
            }
        })
        .await;
        collected
    }

    /// Read exactly one NDJSON line, panicking on EOF or an I/O error.
    async fn next_line(&mut self) -> String {
        self.lines
            .next_line()
            .await
            .expect("read stdout line")
            .expect("tcode serve --stdio closed stdout unexpectedly")
    }

    /// Close stdin (EOF) and assert the daemon exits cleanly.
    ///
    /// Why: proves the STDIO transport's documented EOF-triggers-shutdown
    /// behaviour end-to-end, not just via the offline `transport::tests`.
    pub async fn shutdown_via_eof_and_assert_clean_exit(mut self) {
        drop(self.stdin);
        let status = timeout(WIRE_TIMEOUT, self.child.wait())
            .await
            .expect("timed out waiting for the daemon to exit after stdin EOF")
            .expect("wait() failed");
        assert!(
            status.success(),
            "daemon must exit 0 on stdin EOF, got {status:?}"
        );
    }
}

/// A running `tcode serve --http` subprocess plus its discovered base URL.
pub struct HttpDaemon {
    child: Child,
    pub base_url: String,
}

/// Spawn `tcode serve --http --port 0` and discover its ephemeral bound
/// address from stderr.
///
/// Why: `--port 0` avoids test-suite port collisions; the daemon logs the
/// real bound `host:port` to stderr (never stdout) on startup, which this
/// helper parses.
/// What: spawns the child, reads stderr lines until one contains
/// `"listening on http://"`, extracts the address, and spawns a background
/// task to keep draining stderr for the rest of the daemon's life (so its
/// stderr pipe never fills and blocks the process).
pub async fn spawn_http_daemon() -> HttpDaemon {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args(["serve", "--project", ".", "--http", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn `tcode serve --http`");

    let stderr = child.stderr.take().expect("child stderr");
    let mut lines = BufReader::new(stderr).lines();

    let base_url = timeout(WIRE_TIMEOUT, async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("read stderr")
                .expect("tcode serve --http closed stderr before reporting its address");
            if let Some(addr) = line.split("listening on http://").nth(1) {
                return format!("http://{}", addr.trim());
            }
        }
    })
    .await
    .expect("timed out waiting for the daemon to report its bound address");

    // Keep draining stderr for the rest of the daemon's life so the pipe
    // never fills and blocks it; log content isn't asserted on beyond the
    // startup line already consumed above.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

    HttpDaemon { child, base_url }
}

impl HttpDaemon {
    /// Send SIGTERM and assert the daemon exits cleanly (issue #534
    /// connection-safe daemon-restart convention, exercised here against
    /// the real process rather than only `trusty_common::shutdown_signal`'s
    /// offline unit tests).
    pub async fn shutdown_via_sigterm_and_assert_clean_exit(mut self) {
        let pid = self.child.id().expect("child has a pid") as i32;
        // Safety: `kill(2)` with a valid pid and the standard termination
        // signal; no memory is touched, only a signal is delivered.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let status = timeout(WIRE_TIMEOUT, self.child.wait())
            .await
            .expect("timed out waiting for the daemon to exit after SIGTERM")
            .expect("wait() failed");
        assert!(
            status.success(),
            "daemon must exit 0 after graceful SIGTERM shutdown, got {status:?}"
        );
    }
}

/// Parse `line` as JSON and return it if it's a response whose `id` matches.
///
/// Why: `session.send`'s response and a streamed notification can arrive
/// interleaved with each other (and, in principle, with earlier
/// still-in-flight lines); this is the "is this line the response I'm
/// waiting for" classifier.
pub fn find_response(line: &str, id: i64) -> Option<Value> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("id").and_then(|i| i.as_i64()) == Some(id) {
        Some(v)
    } else {
        None
    }
}

/// Parse `line` as JSON and return its `params` if it's a `session.event`
/// notification for `session_id`.
///
/// Why: the classifier for the OTHER half of an interleaved
/// response/notification pair — a server-initiated notification has no
/// `id`, a `method` field, and this crate's wire shape nests the actual
/// `crate::events::Event` under `params.event`.
pub fn find_session_event(line: &str, session_id: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("method").and_then(|m| m.as_str()) == Some("session.event")
        && v["params"]["session_id"].as_str() == Some(session_id)
    {
        Some(v["params"].clone())
    } else {
        None
    }
}

/// Drive `task.run` -> `session.attach` -> read-until-`session_done` over an
/// already-spawned [`StdioSession`], returning the session id once the run
/// has reached a terminal state.
///
/// Why: (#2058) both `tests/task_e2e.rs`'s live-tool-event assertions and its
/// `session.get_transcript` assertions need "run a task to completion" as a
/// precondition — `get_transcript` in particular must not be called before
/// `task::executor::run_and_record` has stored the outcome via
/// `SessionRegistry::set_run_outcome`, or the transcript would still be
/// empty. Factored out so both call sites share the exact same
/// read-until-terminal logic rather than copy-pasting the polling loop.
/// What: calls `task.run` (id 1) and `session.attach` (id 2) — these two ids
/// are reserved by this helper, so callers must start their OWN subsequent
/// request ids at 3 — then reads NDJSON lines, classifying each as a
/// `session.event` for this session, until `session_done` is observed (or
/// panics after 20 read rounds, mirroring the bounded wait every other e2e
/// polling loop in this crate uses). Returns the session id.
pub async fn run_task_to_completion(daemon: &mut StdioSession, task_description: &str) -> String {
    let run_resp = daemon
        .call(1, "task.run", json!({"task_description": task_description}))
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let mut kinds: Vec<String> = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array")
        .iter()
        .map(|e| {
            e["kind"]
                .as_str()
                .expect("kind must be a string")
                .to_string()
        })
        .collect();

    let mut iterations = 0;
    while !kinds.iter().any(|k| k == "session_done") {
        iterations += 1;
        assert!(
            iterations < 20,
            "gave up waiting for session_done after {iterations} read rounds; kinds so far: {kinds:?}"
        );
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "timed out waiting for more events; kinds so far: {kinds:?}"
        );
        for line in &lines {
            if let Some(envelope) = find_session_event(line, &session_id) {
                kinds.push(
                    envelope["kind"]
                        .as_str()
                        .expect("kind must be a string")
                        .to_string(),
                );
            }
        }
    }

    session_id
}

/// Open `url` as a Server-Sent Events GET, asserting a successful status.
///
/// Why: the #2055 replay->live continuity check needs to read ONE SSE
/// connection in two stages (first the replay burst, then a live event
/// published while still connected) — unlike [`http_get_prefix`], which
/// opens a fresh connection per call and only proves replay content, not
/// continuity within a single stream.
/// What: returns the live `reqwest::Response`; pair with
/// [`read_sse_until`] to pull further chunks from the SAME connection.
pub async fn open_sse(client: &reqwest::Client, url: &str) -> reqwest::Response {
    let resp = client.get(url).send().await.expect("GET request");
    assert!(
        resp.status().is_success(),
        "GET {url} returned {}",
        resp.status()
    );
    resp
}

/// Read more chunks from `resp` into `buffer` until it contains
/// `until_substr` or `total_timeout` elapses.
///
/// Why: paired with [`open_sse`] so a test can read a SINGLE SSE connection
/// in stages (replay, then a later live event) instead of opening a fresh
/// GET per stage — which is what actually proves replay -> live continuity
/// on one stream, per the #2055 requirement.
/// What: appends to `buffer` in place (so a second call continues from
/// where the first left off) via `Response::chunk()` — no `Stream`/`Bytes`
/// naming needed at the call site.
pub async fn read_sse_until(
    resp: &mut reqwest::Response,
    buffer: &mut Vec<u8>,
    until_substr: &str,
    total_timeout: Duration,
) {
    if String::from_utf8_lossy(buffer).contains(until_substr) {
        return;
    }
    let read = timeout(total_timeout, async {
        while !String::from_utf8_lossy(buffer).contains(until_substr) {
            match resp.chunk().await {
                Ok(Some(chunk)) => buffer.extend_from_slice(&chunk),
                _ => break,
            }
        }
    })
    .await;
    assert!(
        read.is_ok(),
        "timed out waiting for {until_substr:?} in the SSE stream"
    );
}

/// Parse an SSE body (one or more `data: {...}` lines, possibly interleaved
/// with blank lines or `:`-comment keep-alives) into the JSON objects it
/// carries.
///
/// Why: the #2055 envelope assertions need to inspect individual event
/// objects (`seq`, `kind`, `session_id`, ...), not just substring-match the
/// raw SSE text the way [`http_get_prefix`]'s callers otherwise would.
/// What: for each line starting with `data:`, strips the prefix and parses
/// the remainder as JSON; lines that aren't `data:` or don't parse are
/// skipped (SSE comment lines, a partially-read trailing frame at the
/// prefix's read boundary).
pub fn parse_sse_frames(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|rest| serde_json::from_str::<Value>(rest.trim()).ok())
        .collect()
}

/// Assert that `envelopes` (in receipt order) each carry every
/// `SessionEventEnvelope` field and form ONE gap-free, strictly increasing
/// `seq` sequence; returns `(first_seq, last_seq)`.
///
/// Why: the #2055 correctness property this whole e2e suite exists to
/// prove — factored out so the STDIO and HTTP/SSE scenarios run the exact
/// same check against their respective wire representations of the same
/// envelope shape.
/// What: panics with the offending envelope on the first missing field or
/// any `seq` that isn't exactly `previous + 1`. Panics on empty input too —
/// every call site here always has at least the replay burst to check.
pub fn assert_envelopes_contiguous(envelopes: &[Value]) -> (u64, u64) {
    assert!(
        !envelopes.is_empty(),
        "expected at least one envelope to check"
    );
    let mut previous: Option<u64> = None;
    for envelope in envelopes {
        for field in ["session_id", "seq", "at", "kind", "event"] {
            assert!(
                envelope.get(field).is_some(),
                "envelope missing required field {field:?}: {envelope}"
            );
        }
        let seq = envelope["seq"].as_u64().expect("seq must be a u64");
        if let Some(prev) = previous {
            assert_eq!(
                seq,
                prev + 1,
                "seq must increase by exactly 1 with no gaps or duplicates: {envelope}"
            );
        }
        previous = Some(seq);
    }
    let first = envelopes[0]["seq"].as_u64().unwrap();
    let last = envelopes[envelopes.len() - 1]["seq"].as_u64().unwrap();
    (first, last)
}
