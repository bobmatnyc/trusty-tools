//! The `trusty-mcp <service>` binary, driven end to end (#6316).
//!
//! Why: the unit tests in `src/bin/trusty-mcp.rs` cover the service table as
//! values. What they cannot cover is the two contracts that only exist once the
//! process runs: stdout carries JSON-RPC frames and nothing else, and every
//! failure arm produces something the caller can match on rather than silence.
//! Both are Fail-Open Checks — a bridge that exits 0 with an empty stdout looks
//! to an MCP client exactly like a bridge that is still thinking.
//!
//! What: spawns the built binary with a temp data directory, so
//! `trusty_common::daemon_socket_path` resolves to a socket nothing is
//! listening on and no daemon on this machine is touched. Each test writes at
//! most one request, closes stdin, and reads the child to completion — nothing
//! outlives the test.
//!
//! Test: this file.

#![cfg(all(unix, feature = "daemon-bridge-json-rpc"))]

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

/// Run the binary with `args`, an isolated data directory, and `stdin` piped in.
///
/// `TRUSTY_DATA_DIR_OVERRIDE` is what makes this hermetic on macOS, where
/// `dirs::data_dir()` ignores `HOME` (see `trusty_common::data_dir`). Without
/// it the resolver would find the real trusty-memory socket on the developer's
/// machine and the dead-socket arm would depend on whether a daemon happened to
/// be running.
fn run(args: &[&str], stdin: &str) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_trusty-mcp"))
        .args(args)
        .env("TRUSTY_DATA_DIR_OVERRIDE", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the bridge");

    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(stdin.as_bytes())
        .expect("write the request");

    child.wait_with_output().expect("the child is reaped")
}

/// Why: Fail-Open Check, arm 1 — an unknown service must be an error the caller
/// can act on, and must not corrupt a stdio channel a client may already be
/// reading.
/// What: exit 2, empty stdout, and a stderr that names both the rejected
/// service and the ones that exist.
/// Test: this test.
#[test]
fn an_unknown_service_exits_two_with_nothing_on_stdout() {
    let out = run(&["review"], "");

    assert_eq!(out.status.code(), Some(2), "an unknown service exits 2");
    assert!(
        out.stdout.is_empty(),
        "stdout must stay clean: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("review"), "{stderr}");
    assert!(stderr.contains("memory"), "{stderr}");
    assert!(stderr.contains("search"), "{stderr}");
    assert!(stderr.contains("analyze"), "{stderr}");
}

/// Why: a bare invocation is the most likely mistake, and the least useful
/// outcome would be an empty exit 0.
/// What: no argument exits 2 with the usage text on stderr.
/// Test: this test.
#[test]
fn no_service_exits_two_with_the_usage_text() {
    let out = run(&[], "");

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Usage:"),
        "the usage text is printed"
    );
}

/// Why: `--help` is a question, not a failure — it must not take the exit-2
/// arm, and it must not write to the JSON-RPC channel either.
/// What: exit 0, usage on stderr, stdout empty.
/// Test: this test.
#[test]
fn help_exits_zero_and_still_writes_nothing_to_stdout() {
    let out = run(&["--help"], "");

    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "stdout is the JSON-RPC channel");
    assert!(String::from_utf8_lossy(&out.stderr).contains("trusty-mcp <service>"));
}

/// Why: the arm this binary exists to get right (#6309). Nothing is listening
/// on the resolved socket, and the client is waiting on an `initialize` it
/// matches by id — an id-less answer, or no answer, is a hang.
/// What: pipes one `initialize` request in, asserts stdout is exactly one
/// JSON-RPC error frame carrying that request's id and naming the daemon, and
/// that the process still exits 0 because stdin reached EOF.
/// Test: this test.
#[test]
fn a_dead_socket_answers_with_a_matchable_error() {
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let out = run(&["memory"], &format!("{request}\n"));

    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    let frames: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        frames.len(),
        1,
        "exactly one response per request: {stdout}"
    );

    let frame: Value = serde_json::from_str(frames[0]).expect("stdout carries a JSON-RPC frame");
    assert_eq!(frame["jsonrpc"], "2.0");
    assert_eq!(frame["id"], 1, "the error is matchable to the request");
    assert!(frame.get("result").is_none(), "a failure is never a result");

    let message = frame["error"]["message"]
        .as_str()
        .expect("the error names its cause");
    assert!(message.contains("trusty-memory"), "{message}");
    assert!(message.contains(".sock"), "{message}");

    assert_eq!(
        out.status.code(),
        Some(0),
        "EOF on stdin is how an MCP server is told to exit"
    );
}

/// Why: MCP §4.1 — replying to a notification puts a frame on the channel the
/// client is not expecting, which desynchronises every response after it.
/// What: a notification produces no frame at all, and the process still exits
/// cleanly on EOF.
/// Test: this test.
#[test]
fn a_notification_produces_no_frame() {
    let out = run(
        &["memory"],
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    );

    assert!(
        out.stdout.is_empty(),
        "a notification is answered with silence: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(out.status.code(), Some(0));
}

/// Why: a streaming method must be refused before the socket is dialled, and
/// the refusal must reach the client as an error rather than as a hang — the
/// #6286 case, seen from outside the process.
/// What: `memory.chat` wrapped in the `tools/call` envelope a real MCP client
/// sends comes back as an `INVALID_REQUEST` naming the method.
/// Test: this test.
#[test]
fn a_streaming_method_is_refused_by_the_binary() {
    let request =
        r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"memory.chat"}}"#;
    let out = run(&["memory"], &format!("{request}\n"));

    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    let frame: Value = serde_json::from_str(stdout.trim()).expect("one JSON-RPC frame");
    assert_eq!(frame["id"], 9);
    assert_eq!(frame["error"]["code"], -32600);
    assert!(
        frame["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("memory.chat")),
        "{frame}"
    );
}
