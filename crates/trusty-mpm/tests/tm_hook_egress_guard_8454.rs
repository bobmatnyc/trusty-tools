//! `tm hook --egress-guard` through the real binary (#8454 G3, slice S1).
//!
//! Why: the guard is the backstop that keeps a tm-launched session — PM or
//! subagent — from calling a Slack, Gmail, Notion or Google Chat MCP tool. Its
//! two claims are only true of the binary as Claude Code runs it: no bypass
//! variable and no subagent marker lifts the deny, and every payload it cannot
//! read denies with exit 0 rather than failing open.
//! What: pipes `PreToolUse` payloads into `tm hook --egress-guard` with the data
//! directory pinned to a temp dir. Family tools, subagent payloads, both bypass
//! variables and each malformed shape must print one `deny` object and exit 0;
//! unrelated tools must print nothing. The audit test reads the denial store
//! back through `list_recent_errors`' aggregation, then blocks the store and
//! checks the deny still goes out.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_egress_guard_8454`.

use crate::common;

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use trusty_mpm::daemon::bug_report::{aggregate_errors_from_paths, store_paths_under};

/// Nothing listens on port 1, so the deny path's audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// The env vars that lift `tm hook --pm-guard`; the egress guard ignores them.
const BYPASS_ENV: &[&str] = &[
    "TRUSTY_MPM_DISABLE_HOOKS",
    "TRUSTY_MPM_PM_UNRESTRICTED",
    "CLAUDE_MPM_SUB_AGENT",
];

/// A `tm hook --egress-guard` command with its data directory at `data`, every
/// bypass variable stripped, then `env` applied on top.
fn guard_command(data: &Path, env: &[(&str, &str)]) -> Command {
    let mut command = common::tm_command_in(common::tm_spawn_home());
    command
        .args(["--url", UNREACHABLE_DAEMON, "hook", "--egress-guard"])
        .env("TRUSTY_DATA_DIR_OVERRIDE", data)
        .env_remove("TRUSTY_NO_BUG_CAPTURE")
        .current_dir(data)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in BYPASS_ENV {
        command.env_remove(key);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

/// Run the guard over `stdin` with `data` as its data directory.
fn run_in(data: &Path, stdin: &[u8], env: &[(&str, &str)]) -> Output {
    let mut child = guard_command(data, env)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn tm hook --egress-guard");
    let mut pipe = child.stdin.take().expect("child stdin");
    // A guard that exits before reading closes the pipe; that is its verdict
    // to report, not a test failure.
    let _ = pipe.write_all(stdin);
    drop(pipe);
    child.wait_with_output().expect("wait for the guard")
}

/// [`run_in`] with a fresh, writable data directory.
fn run(stdin: &[u8], env: &[(&str, &str)]) -> Output {
    let data = tempfile::tempdir().expect("data dir");
    run_in(data.path(), stdin, env)
}

/// A `PreToolUse` payload for `tool` with an empty object `tool_input`.
fn payload(tool: &str) -> Vec<u8> {
    serde_json::json!({
        "session_id": "s-8454",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": {},
    })
    .to_string()
    .into_bytes()
}

/// Assert `out` is exit 0 with exactly one `deny` object on stdout.
fn assert_denied(out: &Output, case: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "{case}: exit 0 always, got {:?}; stderr={stderr}",
        out.status
    );
    let line = stdout.trim();
    assert!(
        !line.contains('\n'),
        "{case}: stdout must be one object: {stdout}"
    );
    let value: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
        panic!("{case}: stdout is not JSON ({e}): {stdout:?}; stderr={stderr}")
    });
    assert_eq!(
        value["hookSpecificOutput"]["permissionDecision"], "deny",
        "{case}: {value}"
    );
    let reason = value["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    assert!(
        reason.contains("#8454"),
        "{case}: reason names the issue: {reason}"
    );
}

/// Assert `out` is exit 0 with nothing on stdout — Claude Code's ALLOW.
fn assert_allowed(out: &Output, case: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "{case}: exit 0, got {:?}; stderr={stderr}",
        out.status
    );
    assert!(stdout.trim().is_empty(), "{case}: must allow: {stdout}");
}

/// One tool of every family, read/search/list tools included (Bob ruling 4),
/// and lookalike casings and separators of each server segment.
const FAMILY_TOOLS: &[&str] = &[
    "mcp__slack-mcp__slack_send_message",
    "mcp__slack-mcp__slack_list_channels",
    "mcp__claude_ai_Slack__slack_read_channel",
    "mcp__SLACK__search_messages",
    "mcp__Slack.Mcp__slack_get_thread",
    "mcp__claude_ai_Gmail__gmail_search_messages",
    "mcp__gmail__list_labels",
    "mcp__G-Mail__send",
    "mcp__notion__notion-search",
    "mcp__claude_ai_Notion__notion-fetch",
    "mcp__NoTion_MCP__create_page",
    "mcp__googlechat__list_spaces",
    "mcp__Google-Chat__send_message",
    "mcp__claude_ai_Google_Chat__read_space",
    "mcp__gchat__send",
    "mcp__G_CHAT__list",
    "mcp__gworkspace-mcp__search_gmail_messages",
    "mcp__gworkspace-mcp__compose_email",
    "mcp__gworkspace-mcp__list_message_attachments",
    "mcp__GWorkspace__send_chat_message",
    "MCP__Slack__send",
];

/// 🔴 #8454 S1: every family tool is denied, whatever its casing or separators.
#[test]
fn egress_denies_each_family_tool_name() {
    for tool in FAMILY_TOOLS {
        assert_denied(&run(&payload(tool), &[]), tool);
    }
}

/// 🔴 #8454 S1 (Bob ruling 3): a subagent is denied like the PM — an
/// `agent_id` in the payload and `CLAUDE_MPM_SUB_AGENT` in the env lift nothing.
#[test]
fn egress_denies_subagent_payload() {
    let subagent = serde_json::json!({
        "session_id": "s-8454",
        "agent_id": "agent-1",
        "agent_type": "engineer",
        "tool_name": "mcp__slack-mcp__slack_send_message",
        "tool_input": {"channel": "C1", "text": "hi"},
    })
    .to_string();
    let out = run(subagent.as_bytes(), &[("CLAUDE_MPM_SUB_AGENT", "1")]);
    assert_denied(&out, "subagent payload");
}

/// 🔴 #8454 S1 (fail-open audit 1): both pm-guard bypass variables set, plus
/// the subagent marker, lift neither the family deny nor the fail-closed deny.
#[test]
fn egress_denies_under_unrestricted_and_disable_hooks() {
    let bypassed = [
        ("TRUSTY_MPM_PM_UNRESTRICTED", "1"),
        ("TRUSTY_MPM_DISABLE_HOOKS", "1"),
        ("CLAUDE_MPM_SUB_AGENT", "1"),
    ];
    for tool in [
        "mcp__claude_ai_Slack__slack_send_message",
        "mcp__notion__notion-search",
    ] {
        assert_denied(&run(&payload(tool), &bypassed), tool);
    }
    assert_denied(&run(b"", &bypassed), "empty stdin under both bypasses");
    assert_denied(&run(b"[]", &bypassed), "array stdin under both bypasses");
}

/// 🔴 #8454 S1 (§4 fail closed): every payload the guard cannot read or
/// classify denies with exit 0.
#[test]
fn egress_denies_malformed_payloads() {
    let raw: &[(&str, &[u8])] = &[
        ("empty stdin", b""),
        ("whitespace stdin", b"  \n\t "),
        ("non-JSON stdin", b"not json"),
        // A read error: `read_to_string` refuses bytes that are not UTF-8.
        ("non-UTF-8 stdin", &[0xff, 0xfe, 0x7b]),
        ("truncated JSON", br#"{"tool_name":"Bash""#),
        ("array JSON", br#"["mcp__slack__send"]"#),
        ("string JSON", br#""mcp__slack__send""#),
        ("number JSON", b"42"),
        ("tool_name missing", br#"{"tool_input":{}}"#),
        ("tool_name empty", br#"{"tool_name":"","tool_input":{}}"#),
        ("tool_name number", br#"{"tool_name":7,"tool_input":{}}"#),
        ("tool_name null", br#"{"tool_name":null,"tool_input":{}}"#),
        (
            "tool_name array",
            br#"{"tool_name":["Bash"],"tool_input":{}}"#,
        ),
        ("tool_input missing", br#"{"tool_name":"Bash"}"#),
        (
            "tool_input string",
            br#"{"tool_name":"Bash","tool_input":"ls"}"#,
        ),
        (
            "tool_input array",
            br#"{"tool_name":"Read","tool_input":[]}"#,
        ),
        (
            "tool_input null",
            br#"{"tool_name":"Read","tool_input":null}"#,
        ),
    ];
    for (case, stdin) in raw {
        assert_denied(&run(stdin, &[]), case);
    }
    for tool in ["mcp__slack", "mcp__", "mcp____send", "mcp__github__"] {
        assert_denied(&run(&payload(tool), &[]), tool);
    }
}

/// 🔴 #8454 S1 (§4): a stdin still open at the read deadline denies, and the
/// guard exits with that deny while stdin stays open — Claude Code discards
/// the output of a hook it cancels.
#[test]
fn egress_denies_a_stdin_left_open_past_the_deadline() {
    let data = tempfile::tempdir().expect("data dir");
    let mut child = guard_command(data.path(), &[])
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn tm hook --egress-guard");
    let mut stdin = child.stdin.take().expect("child stdin");
    stdin
        .write_all(br#"{"tool_name":"mcp__slack__send""#)
        .expect("write stdin");
    stdin.flush().expect("flush stdin");
    let mut child_stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = child_stdout.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    // Past the 5 s read deadline, short of the 10 s hook timeout.
    let stdout = rx.recv_timeout(std::time::Duration::from_secs(9));
    drop(stdin);
    let status = child.wait().expect("wait for the guard");
    let stdout = stdout.expect("the guard must exit while stdin is still open");
    let out = Output {
        status,
        stdout,
        stderr: Vec::new(),
    };
    assert_denied(&out, "stdin held open");
}

/// #8454 S1 control: tools outside the families — including non-mail
/// gworkspace tools and family names outside an `mcp__` name — are allowed.
#[test]
fn egress_allows_unrelated_tools() {
    for tool in [
        "Bash",
        "Read",
        "Write",
        "Task",
        "WebFetch",
        "slack_send_message",
        "mcp__github__create_issue",
        "mcp__trusty-search__search",
        "mcp__claude_ai_Vercel__list_projects",
        "mcp__gworkspace-mcp__create_document",
        "mcp__gworkspace-mcp__search_drive_files",
        "mcp__gworkspace-mcp__manage_events",
    ] {
        assert_allowed(&run(&payload(tool), &[]), tool);
    }
}

/// 🔴 #8454 S1 (fail-open audit 8): a deny is recorded where
/// `list_recent_errors` reads, and a store that cannot be written loses the
/// record, never the deny.
#[test]
fn egress_deny_is_audited_without_changing_verdict_when_log_unwritable() {
    let tool = "mcp__slack-mcp__slack_send_message";

    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).expect("mkdir data");
    assert_denied(&run_in(&data, &payload(tool), &[]), "writable store");
    let errors = aggregate_errors_from_paths(&store_paths_under(&data), 100);
    let deny = errors
        .iter()
        .find(|e| e.record.message.contains(tool))
        .unwrap_or_else(|| panic!("no egress record among {errors:#?}"));
    for part in ["check=egress-family", "session=s-8454"] {
        assert!(
            deny.record.fields.contains(part),
            "{part} missing from {}",
            deny.record.fields
        );
    }

    let blocked = tempfile::tempdir().expect("tempdir");
    let data = blocked.path().join("data");
    // A directory where the denial store file belongs blocks every append.
    std::fs::create_dir_all(data.join("trusty-mpm").join("pm-guard-denials.jsonl"))
        .expect("block the store");
    let out = run_in(&data, &payload(tool), &[]);
    assert_denied(&out, "unwritable store");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not record deny `egress-family`"),
        "the lost record must be reported: {stderr}"
    );
}
