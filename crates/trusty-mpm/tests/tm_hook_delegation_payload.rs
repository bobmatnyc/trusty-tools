//! Integration test: `tm hook` forwards subagent correlation keys (#2864 S1).
//!
//! Why: the daemon can only correlate a `SubagentStop` back to the dispatch that
//! started it if the hook shim actually forwards the keys Claude Code provides.
//! The pure builder is unit-tested in `commands::hook_payload`, but nothing
//! proved those fields survive the real path — stdin JSON → `tm hook` → HTTP
//! POST body. A regression there would be invisible to the unit tests and would
//! silently disable delegation tracking in production, so it is asserted here
//! against the actual built binary.
//! What: stands up a one-shot loopback HTTP server, runs the built `tm` binary
//! as `tm --url <server> hook` with a real captured hook payload on stdin, and
//! asserts on the JSON body the daemon would have received. The payloads mirror
//! live Claude Code 2.1.220 captures (the #2864 Step-0 probe).
//! Test: `cargo test -p trusty-mpm --test tm_hook_delegation_payload`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};

/// Read one HTTP request off `stream` and return its body.
///
/// Why: the hook POSTs a single small JSON body; a full HTTP stack would be
/// overkill. Reading headers to find `Content-Length` and then exactly that many
/// bytes is deterministic and avoids relying on the peer closing the socket.
fn read_http_body(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    // Read until the end of the header block.
    let header_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) => break buf.windows(4).position(|w| w == b"\r\n\r\n"),
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(p);
                }
            }
            Err(e) => panic!("read failed: {e}"),
        }
    }
    .expect("request must contain a header terminator");

    let headers = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
    let len: usize = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .expect("hook POST must carry a content-length");

    let body_start = header_end + 4;
    while buf.len() < body_start + len {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => panic!("read failed: {e}"),
        }
    }
    String::from_utf8_lossy(&buf[body_start..body_start + len]).into_owned()
}

/// Run `tm hook` with `stdin_json` against a one-shot capture server and return
/// the POSTed body, the process's success flag, and its stdout.
fn capture_hook_post(stdin_json: &str) -> (serde_json::Value, bool, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("addr").port();

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let body = read_http_body(&mut stream);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
        let _ = stream.flush();
        body
    });

    let bin = env!("CARGO_BIN_EXE_tm");
    let mut child = Command::new(bin)
        .args(["--url", &format!("http://127.0.0.1:{port}"), "hook"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook`");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for tm hook");

    let body = server.join().expect("capture server panicked");
    let json: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("body was not JSON ({e}): {body}"));
    (
        json,
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn pre_tool_use_dispatch_forwards_tool_use_id_and_cwd() {
    // Verbatim shape of a live Claude Code 2.1.220 subagent dispatch.
    let stdin = serde_json::json!({
        "session_id": "d6066d66-8c7f-41f1-8914-8d0710d563aa",
        "transcript_path": "/tmp/proj/d6066d66.jsonl",
        "cwd": "/tmp/proj",
        "hook_event_name": "PreToolUse",
        "tool_name": "Agent",
        "tool_input": {
            "description": "probe alpha",
            "prompt": "Reply with ALPHA.",
            "subagent_type": "general-purpose"
        },
        "tool_use_id": "toolu_01DAvdgnCx8jYZQmn8NYpUge"
    })
    .to_string();

    let (body, ok, stdout) = capture_hook_post(&stdin);
    assert!(ok, "hook must exit 0 (fail-open)");
    // PreToolUse stdout-purity contract: no rewrite applies to a dispatch tool,
    // so the hook must print nothing at all.
    assert!(
        stdout.is_empty(),
        "PreToolUse must not write to stdout here, got: {stdout:?}"
    );

    assert_eq!(body["event"], "PreToolUse");
    assert_eq!(body["session_id"], "d6066d66-8c7f-41f1-8914-8d0710d563aa");

    let p = &body["payload"];
    // Pre-#2864 fields still present.
    assert_eq!(p["tool"], "Agent");
    assert_eq!(p["input"]["subagent_type"], "general-purpose");
    assert!(
        p["cwd"].as_str().is_some_and(|s| !s.is_empty()),
        "cwd must be forwarded (#1744)"
    );
    // The #2864 additions that make correlation possible.
    assert_eq!(p["tool_use_id"], "toolu_01DAvdgnCx8jYZQmn8NYpUge");
    assert_eq!(p["transcript_path"], "/tmp/proj/d6066d66.jsonl");
}

#[test]
fn post_tool_use_forwards_compacted_agent_id() {
    let stdin = serde_json::json!({
        "session_id": "d6066d66-8c7f-41f1-8914-8d0710d563aa",
        "hook_event_name": "PostToolUse",
        "tool_name": "Agent",
        "tool_use_id": "toolu_01DAvdgnCx8jYZQmn8NYpUge",
        "tool_response": {
            "isAsync": true,
            "status": "async_launched",
            "agentId": "a403cdbc078b5c474",
            "resolvedModel": "claude-haiku-4-5-20251001",
            "prompt": "Reply with ALPHA.",
            "outputFile": "/tmp/out.txt"
        }
    })
    .to_string();

    let (body, ok, _) = capture_hook_post(&stdin);
    assert!(ok);
    let p = &body["payload"];
    assert_eq!(p["tool_use_id"], "toolu_01DAvdgnCx8jYZQmn8NYpUge");
    assert_eq!(p["tool_response"]["agentId"], "a403cdbc078b5c474");
    assert_eq!(p["tool_response"]["status"], "async_launched");
    assert_eq!(p["tool_response"]["isAsync"], true);
    // Bulk fields are not relayed.
    assert!(p["tool_response"].get("prompt").is_none());
    assert!(p["tool_response"].get("outputFile").is_none());
}

#[test]
fn subagent_stop_forwards_agent_id_and_both_transcripts() {
    // `agent_id` is THE correlation key; `agent_transcript_path` is the
    // subagent's own transcript, distinct from the parent's `transcript_path`.
    let stdin = serde_json::json!({
        "session_id": "d6066d66-8c7f-41f1-8914-8d0710d563aa",
        "transcript_path": "/tmp/proj/d6066d66.jsonl",
        "hook_event_name": "SubagentStop",
        "agent_id": "a403cdbc078b5c474",
        "agent_type": "general-purpose",
        "agent_transcript_path": "/tmp/proj/d6066d66/subagents/agent-a403cdbc078b5c474.jsonl",
        "last_assistant_message": "ALPHA"
    })
    .to_string();

    let (body, ok, _) = capture_hook_post(&stdin);
    assert!(ok);
    let p = &body["payload"];
    assert_eq!(body["event"], "SubagentStop");
    assert_eq!(p["agent_id"], "a403cdbc078b5c474");
    assert_eq!(p["agent_type"], "general-purpose");
    assert_eq!(
        p["agent_transcript_path"],
        "/tmp/proj/d6066d66/subagents/agent-a403cdbc078b5c474.jsonl"
    );
    assert_eq!(p["transcript_path"], "/tmp/proj/d6066d66.jsonl");
}

#[test]
fn ordinary_tool_use_does_not_gain_a_tool_response() {
    // The bandwidth guard, end to end: a Bash PostToolUse must not relay its
    // (potentially enormous) tool_response.
    let stdin = serde_json::json!({
        "session_id": "d6066d66-8c7f-41f1-8914-8d0710d563aa",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_use_id": "toolu_bash",
        "tool_response": { "stdout": "x".repeat(100_000) }
    })
    .to_string();

    let (body, ok, _) = capture_hook_post(&stdin);
    assert!(ok);
    let p = &body["payload"];
    assert_eq!(p["tool"], "Bash");
    assert_eq!(p["tool_use_id"], "toolu_bash");
    assert!(
        p.get("tool_response").is_none(),
        "a non-dispatch tool_response must never be relayed"
    );
    assert!(body.to_string().len() < 2000, "relay must stay bounded");
}
