//! Process/protocol plumbing shared by `tests/session_e2e.rs`.
//!
//! Why: driving the REAL `tcode` binary over its real stdio/HTTP surface
//! needs a bit of scaffolding (spawn, NDJSON line I/O with timeouts, parse
//! helpers for responses vs. server-initiated notifications, SSE body
//! reading) that has nothing to do with the session-lifecycle assertions
//! themselves. Keeping it in `tests/support/mod.rs` (not a top-level
//! `tests/*.rs` file, so cargo does not treat it as its own test binary)
//! keeps `session_e2e.rs` readable as a pure black-box script.
//! What: [`StdioSession`] (spawn + NDJSON request/response/notification
//! I/O over real pipes), [`HttpDaemon`]/[`spawn_http_daemon`] (spawn +
//! discover the bound HTTP address from stderr), [`find_response`]/
//! [`find_session_event`] (classify a raw NDJSON line), [`open_sse`]/
//! [`read_sse_until`] (read one never-terminating SSE connection in
//! stages), [`parse_sse_frames`] (split an SSE body into its JSON `data:`
//! frames), and [`assert_envelopes_contiguous`] (#2055: the shared
//! seq/field-presence check both the STDIO and HTTP/SSE e2e scenarios run
//! against the SAME `SessionEventEnvelope` shape).

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
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tcode"))
            .args(["serve", "--project", ".", "--stdio"])
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
