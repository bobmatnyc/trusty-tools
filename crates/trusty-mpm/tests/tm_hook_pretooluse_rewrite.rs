//! Integration test for `tm hook`'s `PreToolUse` Bash command-rewrite spike
//! (issue #1956, Option 0).
//!
//! Why: `commands::hook_rewrite`'s unit tests cover the rewrite/exclusion
//! decision logic in isolation; `commands::misc::hook`'s own tests cover the
//! sub-agent/disable-hooks guard branches. Neither exercises the actual
//! stdin-read → rewrite → stdout-print path end to end through the real
//! binary, which is the behavior Claude Code will actually depend on. This
//! file closes that gap.
//! What: Runs the built `tm` binary (`CARGO_BIN_EXE_tm`) as
//! `tm --url http://127.0.0.1:1 hook` with `CLAUDE_HOOK_EVENT=PreToolUse` and
//! a `{"tool_name":"Bash","tool_input":{"command":"..."}}` stdin payload,
//! then asserts the printed `hookSpecificOutput.updatedInput.command` matches
//! the expected rewrite (or that nothing is printed for excluded commands).
//! The daemon URL is pointed at an unreachable address so the best-effort
//! POST fails fast without a real daemon running.
//! Test: `cargo test -p trusty-mpm --test tm_hook_pretooluse_rewrite`.
//!
//! Note: the `hookSpecificOutput`/`updatedInput` JSON shape asserted against
//! below, and the stdin field names consumed (`hook_event_name`,
//! `tool_name`, `tool_input.command`), are confirmed against the live Claude
//! Code hooks reference (<https://code.claude.com/docs/en/hooks>, confirmed
//! 2026-07-03) — see `commands::hook_rewrite::build_pretooluse_rewrite_response`'s
//! doc comment for the citation. This module previously carried a "not
//! validated against a live Claude Code instance" caveat; that has been
//! resolved, though these tests still only prove our own code is internally
//! consistent with the documented protocol, not a substitute for testing
//! against a real running Claude Code session.

use std::io::Write;
use std::process::{Command, Stdio};

/// Spawn `tm hook` with the given `CLAUDE_HOOK_EVENT` and stdin JSON,
/// returning stdout as a string.
fn run_hook_with_stdin(event: &str, stdin_json: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut child = Command::new(bin)
        .args(["--url", "http://127.0.0.1:1", "hook"])
        .env("CLAUDE_HOOK_EVENT", event)
        .env("CLAUDE_SESSION_ID", "test-session")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `tm hook`");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait for tm hook");
    assert!(
        output.status.success(),
        "tm hook exited non-zero: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

#[test]
fn hook_rewrites_plain_bash_command_on_pretooluse() {
    let stdout = run_hook_with_stdin(
        "PreToolUse",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON when rewriting");
    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(
        parsed["hookSpecificOutput"]["updatedInput"]["command"],
        "cargo test | tm compress --tool \"cargo test\""
    );
}

#[test]
fn hook_stays_silent_for_orchestrator_command_on_pretooluse() {
    let stdout = run_hook_with_stdin(
        "PreToolUse",
        r#"{"tool_name":"Bash","tool_input":{"command":"make build"}}"#,
    );
    assert_eq!(
        stdout.trim(),
        "",
        "excluded orchestrator command must produce no stdout output, got: {stdout}"
    );
}

#[test]
fn hook_stays_silent_for_non_bash_tool_on_pretooluse() {
    let stdout = run_hook_with_stdin(
        "PreToolUse",
        r#"{"tool_name":"Read","tool_input":{"file_path":"/tmp/x"}}"#,
    );
    assert_eq!(stdout.trim(), "");
}

#[test]
fn hook_stays_silent_without_stdin_payload() {
    let stdout = run_hook_with_stdin("PreToolUse", "");
    assert_eq!(stdout.trim(), "");
}

#[test]
fn hook_rewrite_stdout_contains_only_the_json_object() {
    // Per the live Claude Code hooks protocol, "your hook's stdout must
    // contain only the JSON object" for a successful (exit 0) response —
    // this is a direct regression guard for the early `return Ok(())` after
    // printing in `misc::hook` (trusty-review finding, PR #1968): before the
    // fix, execution fell through to build and (best-effort) send an
    // observability POST after already printing to stdout for this branch.
    let stdout = run_hook_with_stdin(
        "PreToolUse",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one line of stdout (the JSON object), got: {stdout:?}"
    );
    serde_json::from_str::<serde_json::Value>(lines[0])
        .expect("the single stdout line must be valid JSON");
}

/// Spawn `tm hook` WITHOUT `CLAUDE_HOOK_EVENT`/`CLAUDE_SESSION_ID` set at
/// all, relying purely on the stdin JSON's `hook_event_name`/`session_id`
/// fields — this is what a real Claude Code invocation looks like per the
/// live hooks reference (confirmed 2026-07-03): Claude Code does not set
/// those as environment variables for hook subprocesses.
fn run_hook_with_stdin_only(stdin_json: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut child = Command::new(bin)
        .args(["--url", "http://127.0.0.1:1", "hook"])
        .env_remove("CLAUDE_HOOK_EVENT")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `tm hook`");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait for tm hook");
    assert!(
        output.status.success(),
        "tm hook exited non-zero: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

#[test]
fn hook_rewrites_using_stdin_hook_event_name_without_env_var() {
    // Regression test for a trusty-review-flagged bug (PR #1968): Claude
    // Code never sets `CLAUDE_HOOK_EVENT` as an environment variable for
    // hook subprocesses — `hook_event_name` is only ever delivered via the
    // stdin JSON payload. Before the fix, `event` always resolved to the
    // `"Unknown"` placeholder in this scenario and the entire PreToolUse
    // rewrite spike silently never fired against a real Claude Code
    // instance.
    let stdout = run_hook_with_stdin_only(
        r#"{"hook_event_name":"PreToolUse","session_id":"abc-123","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON when rewriting");
    assert_eq!(
        parsed["hookSpecificOutput"]["updatedInput"]["command"],
        "cargo test | tm compress --tool \"cargo test\""
    );
}
