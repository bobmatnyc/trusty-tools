//! The claude.ai connectors are no longer blocked (#10735, owner ruling D19).
//!
//! Why: the owner reversed #8454. A Slack, Gmail, Notion or Google Chat tool
//! call must pass `tm hook --egress-guard` (still present only so a stale
//! settings entry cannot error), and the Architect launch must neither deny
//! those tool families nor pass `disableClaudeAiConnectors` (#10725).
//! What: pipes `PreToolUse` payloads into the real binary and reads the
//! Architect `--settings` argv from the shipped lockdown.
//! Test: `cargo test -p trusty-mpm --test integration connectors_allowed_10735`.

use crate::common;

use std::io::Write;
use std::process::Stdio;

use trusty_mpm::core::architect_lockdown::Lockdown;
use trusty_mpm::core::session_profile::SessionProfile;

/// Pipe `stdin` into `tm hook --egress-guard`; returns (exit ok, stdout).
fn run_guard(stdin: &[u8]) -> (bool, String) {
    let data = tempfile::tempdir().expect("data dir");
    let mut child = common::tm_command_in(common::tm_spawn_home())
        .args(["--url", "http://127.0.0.1:1", "hook", "--egress-guard"])
        .env("TRUSTY_DATA_DIR_OVERRIDE", data.path())
        .current_dir(data.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tm hook --egress-guard");
    let mut pipe = child.stdin.take().expect("child stdin");
    let _ = pipe.write_all(stdin);
    drop(pipe);
    let out = child.wait_with_output().expect("wait");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// #10735: a connector tool call, or any stale-settings input, is allowed:
/// exit 0 and nothing on stdout.
#[test]
fn connector_tool_calls_are_no_longer_blocked() {
    let tools = [
        "mcp__claude_ai_Slack__slack_send_message",
        "mcp__slack-mcp__slack_send_message",
        "mcp__claude_ai_Notion__notion-search",
        "mcp__claude_ai_Gmail__create_draft",
        "mcp__claude_ai_Google_Chat__send_message",
    ];
    for tool in tools {
        let payload = serde_json::json!({
            "session_id": "s-10735",
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "tool_input": {},
        })
        .to_string();
        let (ok, stdout) = run_guard(payload.as_bytes());
        assert!(ok, "{tool}: exit 0");
        assert!(stdout.trim().is_empty(), "{tool} must be allowed: {stdout}");
    }
    // A stale settings entry must not error on empty or garbage stdin.
    for junk in [&b""[..], b"not json"] {
        let (ok, stdout) = run_guard(junk);
        assert!(ok, "junk stdin exits 0");
        assert!(stdout.trim().is_empty(), "junk stdin allowed: {stdout}");
    }
}

/// #10735, #10725: the Architect launch denies no connector family and does
/// not set `disableClaudeAiConnectors`.
#[test]
fn the_architect_launch_keeps_the_claude_ai_connectors() {
    let argv = Lockdown::PRODUCTION
        .settings_argv(SessionProfile::Architect, None)
        .expect("argv");
    let joined = argv.join(" ");
    assert!(
        !joined.contains("disableClaudeAiConnectors"),
        "connectors must load: {joined}"
    );
    for family in ["slack", "Slack", "notion", "Notion", "gmail", "googlechat"] {
        assert!(!joined.contains(family), "{family} not denied: {joined}");
    }
}
