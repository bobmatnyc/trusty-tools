//! The claude.ai connectors are no longer blocked; the rest of the egress
//! guard is (#10735, owner ruling D6d: "Connector part only: keep taint, Bash
//! egress and audit denies").
//!
//! Why: a Slack, Gmail, Notion or Google Chat claude.ai connector call must
//! pass `tm hook --egress-guard`, and the Architect launch must neither deny
//! those families nor pass `disableClaudeAiConnectors` (#10725). The Bash
//! egress deny, the deny on a non-connector chat server and the fail-closed
//! payload deny stay.
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
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
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

/// A `PreToolUse` payload for `tool` with `input`.
fn payload(tool: &str, input: serde_json::Value) -> Vec<u8> {
    serde_json::json!({
        "session_id": "s-10735",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input,
    })
    .to_string()
    .into_bytes()
}

/// The deny decision `stdout` carries, or `None` for an allow.
fn decision(stdout: &str) -> Option<String> {
    let out: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    out["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .map(str::to_owned)
}

/// 🔴 #10735: a claude.ai connector tool call, and a resource tool naming a
/// connector server, is allowed: exit 0 and nothing on stdout.
#[test]
fn connector_tool_calls_are_no_longer_blocked() {
    let tools = [
        "mcp__claude_ai_Slack__slack_send_message",
        "mcp__claude_ai_Notion__notion-search",
        "mcp__claude_ai_Gmail__create_draft",
        "mcp__claude_ai_Google_Chat__send_message",
    ];
    for tool in tools {
        let (ok, stdout) = run_guard(&payload(tool, serde_json::json!({})));
        assert!(ok, "{tool}: exit 0");
        assert!(stdout.trim().is_empty(), "{tool} must be allowed: {stdout}");
    }
    let resource = payload(
        "ListMcpResourcesTool",
        serde_json::json!({"server": "claude.ai Notion"}),
    );
    let (ok, stdout) = run_guard(&resource);
    assert!(ok && stdout.trim().is_empty(), "resource allowed: {stdout}");
}

/// 🔴 #10735 (D6d keeps the rest): a non-connector chat server, a Bash command
/// naming a channel host or a mail program (an ANSI-C `$'…'` spelling
/// included), and an unreadable payload are each still denied.
#[test]
fn the_kept_egress_denies_still_hold() {
    let cases = [
        payload(
            "mcp__slack-mcp__slack_send_message",
            serde_json::json!({"channel": "C1", "text": "hi"}),
        ),
        payload("mcp__notion__notion-search", serde_json::json!({})),
        payload(
            "mcp__gworkspace-mcp__send_gmail_message",
            serde_json::json!({}),
        ),
        payload(
            "Bash",
            serde_json::json!({"command": "curl -X POST https://slack.com/api/chat.postMessage"}),
        ),
        payload(
            "Bash",
            serde_json::json!({"command": "curl https://gmail.googleapis.com/gmail/v1/users/me/messages/send"}),
        ),
        payload(
            "Bash",
            serde_json::json!({"command": "sendmail owner@example.com < note.txt"}),
        ),
        payload(
            "Bash",
            serde_json::json!({"command": "$'\\x73endmail' owner@example.com < note.txt"}),
        ),
        b"not json".to_vec(),
    ];
    for case in cases {
        let (ok, stdout) = run_guard(&case);
        let case = String::from_utf8_lossy(&case);
        assert!(ok, "{case}: exit 0");
        assert_eq!(
            decision(&stdout).as_deref(),
            Some("deny"),
            "{case}: {stdout}"
        );
    }
}

/// 🔴 #10735, #10725: the Architect launch denies no connector family and does
/// not set `disableClaudeAiConnectors`; the gworkspace messaging deny stays.
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
    for family in [
        "slack",
        "Slack",
        "notion",
        "Notion",
        "gmail",
        "Gmail",
        "googlechat",
        "gchat",
    ] {
        assert!(!joined.contains(family), "{family} not denied: {joined}");
    }
    assert!(joined.contains("mcp__*gworkspace*__*mail*"), "{joined}");
}
