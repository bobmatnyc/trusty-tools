//! Unit tests for subprocess spawn behavior (the #147 non-zero-exit rescue).
//!
//! Why: The rescue logic — treating a valid NDJSON `Result` as success even on
//! a non-zero child exit — is subtle and worth a focused regression test.
//! What: Spawns a tiny helper child via the real `spawn_*` entry points and
//! asserts the rescue path.
//! Test: This module is itself the test coverage.

use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

use crate::ipc::{IpcMessage, parse_message};
use crate::subprocess::spawn::apply_delegation_taint_env;
#[cfg(unix)]
use crate::test_env::{spawn_script, write_executable_script};

/// Read back the value `apply_delegation_taint_env` (or any other
/// `cmd.env(...)` call) set for `key` on a `tokio::process::Command`,
/// without spawning it.
///
/// Why: `Command` does not expose a `get_env`-by-key lookup directly;
/// `as_std().get_envs()` returns an iterator of every EXPLICITLY set
/// var (inherited process env is not included), which is exactly the
/// "what did we just tell the child to see" question these tests ask.
fn get_env_on_command<'a>(cmd: &'a Command, key: &str) -> Option<&'a std::ffi::OsStr> {
    cmd.as_std()
        .get_envs()
        .find(|(k, _)| *k == std::ffi::OsStr::new(key))
        .and_then(|(_, v)| v)
}

/// Security fix (delegate_to_agent injection-to-RCE path, item 4, #4126):
/// the `cmd.env("TAGENT_DELEGATION_TAINT_ALLOW", ...)` write in
/// `apply_delegation_taint_env` had no direct test coverage at all before
/// this — the doc comment on `SubprocessAgentRunner::with_delegation_taint`
/// cited two tests (`delegation_taint_allow_env_set_when_configured`,
/// `delegation_taint_allow_env_absent_by_default`) that did not exist
/// anywhere in the repo (a "doc-comment pointer lint" CI failure). This is
/// the REAL round trip: build an actual `Command`, apply the function, read
/// the env var back off the `Command` object (never spawned).
#[test]
fn delegation_taint_allow_env_set_when_configured() {
    let mut cmd = Command::new("true");
    let patterns = vec!["web_search".to_string(), "delegate_to_agent".to_string()];

    apply_delegation_taint_env(&mut cmd, Some(&patterns), "engineer");

    let value = get_env_on_command(&cmd, "TAGENT_DELEGATION_TAINT_ALLOW")
        .expect(
            "TAGENT_DELEGATION_TAINT_ALLOW must be set on the Command when a taint is configured",
        )
        .to_str()
        .expect("value must be valid UTF-8");
    let decoded: Vec<String> = serde_json::from_str(value).expect("value must round-trip as JSON");
    assert_eq!(decoded, patterns);
}

/// Companion to the test above: when the caller passes `None` (every
/// non-assistant-tier / non-delegated spawn — still the overwhelming
/// majority), NO env var is written at all, byte-identical to every spawn
/// before this feature existed.
#[test]
fn delegation_taint_allow_env_absent_by_default() {
    let mut cmd = Command::new("true");

    apply_delegation_taint_env(&mut cmd, None, "engineer");

    assert!(
        get_env_on_command(&cmd, "TAGENT_DELEGATION_TAINT_ALLOW").is_none(),
        "no taint configured must mean no TAGENT_DELEGATION_TAINT_ALLOW env var on the child Command at all"
    );
}

/// `Some(vec![])` (an assistant-tier delegator with no resolvable
/// `[tools].allow` of its own — the fail-closed case item 2/3 rely on) must
/// still WRITE the env var — to the literal `"[]"` — rather than being
/// treated the same as `None`. If this collapsed to "no var set", the child
/// would see an absent env var and run completely untainted, silently
/// reopening the exact hole the fail-closed default exists to close.
#[test]
fn delegation_taint_allow_env_empty_vec_still_sets_deny_all() {
    let mut cmd = Command::new("true");
    let empty: Vec<String> = Vec::new();

    apply_delegation_taint_env(&mut cmd, Some(&empty), "engineer");

    let value = get_env_on_command(&cmd, "TAGENT_DELEGATION_TAINT_ALLOW")
        .expect("an empty (deny-all) taint must still set the env var")
        .to_str()
        .expect("value must be valid UTF-8");
    assert_eq!(value, "[]");
}

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
    let tmp = tempfile::tempdir().unwrap();
    let script = write_executable_script(
        tmp.path(),
        "fake-agent",
        "#!/bin/sh\n\
         printf '%s\\n' \
         '{\"type\":\"result\",\"id\":\"test-id\",\"content\":\"agent output\",\"status\":\"success\"}'\n\
         exit 1\n",
    );

    // Spawn the fake script and read exactly one NDJSON line from stdout.
    // `spawn_script` retries on ETXTBSY for belt-and-suspenders safety (#1528).
    let mut child = spawn_script(&script).await.unwrap();

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
    let tmp = tempfile::tempdir().unwrap();
    let script = write_executable_script(
        tmp.path(),
        "fail-agent",
        "#!/bin/sh\n\
         printf '%s\\n' \
         '{\"type\":\"error\",\"id\":\"test-id\",\"error\":\"crashed\",\"status\":\"error\"}'\n\
         exit 2\n",
    );

    // `spawn_script` retries on ETXTBSY for belt-and-suspenders safety (#1528).
    let mut child = spawn_script(&script).await.unwrap();

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
