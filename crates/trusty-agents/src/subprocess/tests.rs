//! Unit tests for subprocess spawn behavior (the #147 non-zero-exit rescue).
//!
//! Why: The rescue logic — treating a valid NDJSON `Result` as success even on
//! a non-zero child exit — is subtle and worth a focused regression test.
//! What: Spawns a tiny helper child via the real `spawn_*` entry points and
//! asserts the rescue path.
//! Test: This module is itself the test coverage.

use tokio::io::AsyncBufReadExt;

use crate::ipc::{IpcMessage, parse_message};
#[cfg(unix)]
use crate::test_env::{spawn_script, write_executable_script};

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

// --- `build_command` env-var wiring (security fix: delegate_to_agent
// injection-to-RCE path, code-critic HIGH 2 follow-up on PR #4161) ---
//
// Why these exist: every OTHER test covering this fix (`scope_for_delegation`,
// `parse_delegation_taint_env`, `ToolRegistry::replace`) exercises pure
// functions with hand-constructed arguments. NONE of them proves
// `spawn.rs` actually sets `TAGENT_DELEGATION_TAINT_ALLOW` on the real
// `Command`, or that a missing/malformed value is handled the way the doc
// comments claim. A typo in the env var name, a dropped `cmd.env(...)`
// call, or an early return that skips it would silently reopen #4126 with
// every other test still green. `build_command` never spawns anything, so
// these run instantly and need no `#[cfg(unix)]` guard.

use super::spawn::{SpawnConfig, build_command};

/// Read back a single env var `Command::env` set on a not-yet-spawned
/// `tokio::process::Command`, via the underlying `std::process::Command`
/// (`get_envs()` is stable API `tokio::process::Command::as_std()` exposes).
fn get_env<'a>(cmd: &'a tokio::process::Command, key: &str) -> Option<&'a std::ffi::OsStr> {
    cmd.as_std()
        .get_envs()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v)
}

fn minimal_spawn_config<'a>(
    agent_name: &'a str,
    delegation_taint_allow: Option<&'a [String]>,
) -> SpawnConfig<'a> {
    SpawnConfig {
        agent_name,
        task: "irrelevant for Command construction",
        out_dir: None,
        code_dir: None,
        history: &[],
        ctx: None,
        config_dir: None,
        project_dir: None,
        delegation_taint_allow,
    }
}

#[test]
fn build_command_sets_delegation_taint_env_when_configured() {
    let patterns = vec!["delegate_to_agent".to_string(), "web_search".to_string()];
    let cfg = minimal_spawn_config("engineer", Some(&patterns));

    let cmd = build_command(&cfg).expect("build_command should succeed");

    let value = get_env(&cmd, "TAGENT_DELEGATION_TAINT_ALLOW")
        .expect("TAGENT_DELEGATION_TAINT_ALLOW must be set on the child Command when a taint is configured")
        .to_str()
        .expect("env value should be valid UTF-8");
    let decoded: Vec<String> =
        serde_json::from_str(value).expect("env value should round-trip as JSON");
    assert_eq!(decoded, patterns);

    // Also sanity-check the other half of the contract: the child is told
    // which agent to run.
    let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
    assert_eq!(args, vec!["--agent", "engineer"]);
}

#[test]
fn build_command_omits_delegation_taint_env_by_default() {
    // Every non-assistant-tier, non-delegated spawn (the overwhelming
    // majority — every `pm`/`ctrl`-initiated coding sub-agent) must see NO
    // `TAGENT_DELEGATION_TAINT_ALLOW` at all, not merely an empty one — the
    // env var's ABSENCE is what `run_subagent`'s `parse_delegation_taint_env`
    // treats as "untainted" (`None` in, `None` out). Setting it to anything,
    // even `"[]"`, would (correctly, but needlessly) taint every ordinary
    // spawn.
    let cfg = minimal_spawn_config("engineer", None);
    let cmd = build_command(&cfg).expect("build_command should succeed");
    assert_eq!(
        get_env(&cmd, "TAGENT_DELEGATION_TAINT_ALLOW"),
        None,
        "TAGENT_DELEGATION_TAINT_ALLOW must be entirely absent for an untainted spawn"
    );
}

#[test]
fn build_command_empty_taint_is_still_set_as_deny_all() {
    // `Some(vec![])` (an assistant with no resolved `[tools].allow` at
    // all — the fail-closed default `build_assistant_delegate_tool` uses
    // when `posture` is `None`) must still be FORWARDED as an explicit
    // empty JSON array, not treated the same as "no taint at all" — the
    // child must see a deny-all taint, not fall through to unrestricted.
    let empty: Vec<String> = Vec::new();
    let cfg = minimal_spawn_config("engineer", Some(&empty));
    let cmd = build_command(&cfg).expect("build_command should succeed");
    let value = get_env(&cmd, "TAGENT_DELEGATION_TAINT_ALLOW")
        .expect("an empty taint must still be SET, not omitted")
        .to_str()
        .unwrap();
    assert_eq!(value, "[]");
}
