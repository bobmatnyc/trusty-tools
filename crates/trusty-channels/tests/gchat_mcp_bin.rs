//! The built `gchat-mcp` binary: one server per project, and stdout carries
//! JSON-RPC only (#9448 S2b).
//!
//! Why: the lock refusal and the stdout/stderr split are properties of the
//! process, so they are asserted on a spawned binary.
//! What: each test uses a temp project with no routes file, so the server
//! opens with zero routes and makes no network call.
//! Test: this file is the test.

#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Stdio};

use trusty_channels::gchat::GchatChannel;

fn gchat_mcp() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gchat-mcp"));
    cmd.env_remove("TRUSTY_CHANNELS_PROJECT_DIR")
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

#[test]
fn second_server_on_a_locked_project_fails_with_the_lock_message() {
    let project = tempfile::tempdir().expect("project dir");
    let _first = GchatChannel::open(project.path()).expect("first server's channel");

    let out = gchat_mcp()
        .arg("--project-dir")
        .arg(project.path())
        .stdin(Stdio::null())
        .output()
        .expect("run gchat-mcp");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a second server must not start: {stderr}"
    );
    assert!(
        stderr.contains("another gchat-mcp already serves"),
        "{stderr}"
    );
    assert!(
        stderr.contains("gchat.lock"),
        "the message names the lock: {stderr}"
    );
    assert!(out.stdout.is_empty(), "nothing on stdout");
}

#[test]
fn stdout_carries_only_json_rpc_and_logs_go_to_stderr() {
    let project = tempfile::tempdir().expect("project dir");
    let mut child = gchat_mcp()
        .arg("--project-dir")
        .arg(project.path())
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn gchat-mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        let requests = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"gchat_ask","arguments":{"to":"nobody@example.com","text":"hi"}}}"#,
        ];
        for r in requests {
            writeln!(stdin, "{r}").expect("write request");
        }
    }
    let out = child.wait_with_output().expect("wait gchat-mcp");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");

    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| {
            serde_json::from_str(l).unwrap_or_else(|e| panic!("non-JSON stdout line {l:?}: {e}"))
        })
        .collect();
    assert_eq!(
        lines.len(),
        3,
        "one response per request with an id: {stdout}"
    );
    for (line, id) in lines.iter().zip(1..) {
        assert_eq!(line["jsonrpc"], "2.0", "{line}");
        assert_eq!(line["id"], id, "{line}");
    }
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "gchat-mcp");
    assert_eq!(
        lines[1]["result"]["tools"].as_array().map(Vec::len),
        Some(4)
    );
    assert_eq!(lines[2]["result"]["isError"], true, "{}", lines[2]);
    assert!(
        stderr.contains("gchat-mcp serving"),
        "the log line is on stderr: {stderr}"
    );
}
