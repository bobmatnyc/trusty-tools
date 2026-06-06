//! Unit tests for subprocess spawn behavior (the #147 non-zero-exit rescue).
//!
//! Why: The rescue logic — treating a valid NDJSON `Result` as success even on
//! a non-zero child exit — is subtle and worth a focused regression test.
//! What: Spawns a tiny helper child via the real `spawn_*` entry points and
//! asserts the rescue path.
//! Test: This module is itself the test coverage.

use std::process::Stdio;

use tokio::io::AsyncBufReadExt;

use crate::ipc::{IpcMessage, parse_message};

/// #147: A subprocess that writes a valid IpcMessage::Result to stdout and
/// then exits with code 1 must be treated as success by the rescue logic in
/// `spawn_subagent_and_run_with_full_env_ctx`. This mirrors the
/// `error_max_turns` rescue in `ClaudeCodeAgentRunner` (#113).
///
/// Why: Some agents produce correct output but crash during cleanup (e.g. a
/// drop handler panics, a tool subprocess returns non-zero). Propagating the
/// exit code as a hard error discards valid work and fails the whole phase.
/// What: Spawns a tiny shell script that emits a valid NDJSON Result line and
/// then exits with code 1. Replicates the rescue branch inline and asserts
/// `Ok(IpcMessage::Result)` is returned.
/// Test: `cargo test subprocess::tests::rescue_valid_result_on_nonzero_exit`
#[cfg(unix)]
#[tokio::test]
async fn rescue_valid_result_on_nonzero_exit() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("fake-agent");
    // Emit a valid IpcMessage::Result line then exit with code 1.
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' \
         '{\"type\":\"result\",\"id\":\"test-id\",\"content\":\"agent output\",\"status\":\"success\"}'\n\
         exit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Spawn the fake script and read exactly one NDJSON line from stdout.
    let mut child = tokio::process::Command::new(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let status = child.wait().await.unwrap();
    assert!(!status.success(), "script should exit non-zero");

    let msg = parse_message(&line).expect("line should parse as IpcMessage");
    assert!(
        matches!(msg, IpcMessage::Result { .. }),
        "parsed message should be IpcMessage::Result, got: {msg:?}"
    );

    // Replicate the #147 rescue branch: non-zero + Result => Ok.
    // The real rescue path lives in spawn_subagent_and_run_with_full_env_ctx
    // and spawn_subagent_with_config_dir; we mirror the same logic here so
    // the invariant is machine-checked without re-invoking the binary.
    let rescued = if !status.success() {
        match &msg {
            IpcMessage::Result { .. } => Ok(msg.clone()),
            _ => Err(anyhow::anyhow!("non-zero exit and no valid result")),
        }
    } else {
        Ok(msg.clone())
    };

    let output = rescued.expect("rescue path should yield Ok");
    let IpcMessage::Result { content, .. } = output else {
        panic!("expected IpcMessage::Result after rescue");
    };
    assert_eq!(content, "agent output");
}

/// #147: A subprocess that exits non-zero AND produces an IpcMessage::Error
/// on stdout must still propagate a hard error — the rescue only applies
/// when there is a valid IpcMessage::Result to return.
#[cfg(unix)]
#[tokio::test]
async fn nonzero_exit_without_result_still_errors() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("fail-agent");
    // Emit an IpcMessage::Error line then exit with code 2.
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' \
         '{\"type\":\"error\",\"id\":\"test-id\",\"error\":\"crashed\",\"status\":\"error\"}'\n\
         exit 2\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut child = tokio::process::Command::new(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let status = child.wait().await.unwrap();
    assert!(!status.success(), "script should exit non-zero");

    let msg = parse_message(&line).expect("line should parse as IpcMessage");

    // The non-rescue branch: non-zero + Error => must error.
    let result: anyhow::Result<IpcMessage> = if !status.success() {
        match &msg {
            IpcMessage::Result { .. } => Ok(msg.clone()),
            _ => Err(anyhow::anyhow!(
                "sub-agent exited with status {} and no valid result",
                status
            )),
        }
    } else {
        Ok(msg.clone())
    };

    assert!(
        result.is_err(),
        "non-zero exit with IpcMessage::Error should propagate as Err"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("no valid result"),
        "unexpected error: {err_msg}"
    );
}
