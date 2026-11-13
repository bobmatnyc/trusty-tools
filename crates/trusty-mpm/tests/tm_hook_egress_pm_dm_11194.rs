//! `tm hook --egress-guard` lets a PM main thread send Slack to the owner's
//! DM, and nothing else, through the real binary (#11194).
//!
//! Why: the allow is only as narrow as the binary Claude Code runs: one send
//! tool to one exact channel id, one canvas tool with no channel or that id,
//! main threads only, and an audit line or a deny.
//! What: each case pipes one `PreToolUse` payload into the guard with a fresh
//! data directory and checks the verdict and the audit file under it. Ids
//! other than the owner's DM are synthetic.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_egress_pm_dm_11194`.

use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};

use crate::common;
use crate::tm_hook_egress_guard_8454::{assert_allowed, assert_denied, guard_command_in};

/// The owner's DM, the one target the allow names.
const OWNER_DM: &str = "D0AJ5P0PRTK";

/// The slack-mcp send and canvas tools.
const SEND: &str = "mcp__slack-mcp__slack_send_message";
const CANVAS: &str = "mcp__slack-mcp__slack_create_canvas";

/// Run the guard over `payload` with `data` as `TRUSTY_DATA_DIR_OVERRIDE`.
fn run_in(data: &Path, payload: &serde_json::Value, env: &[(&str, &str)]) -> Output {
    let mut child = guard_command_in(common::tm_spawn_home(), data, env)
        .env("TRUSTY_MPM_SESSION_PROFILE", "pm")
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn tm hook --egress-guard");
    let mut pipe = child.stdin.take().expect("child stdin");
    let _ = pipe.write_all(payload.to_string().as_bytes());
    drop(pipe);
    child.wait_with_output().expect("wait for the guard")
}

/// A main-thread payload for `tool` with `input`.
fn call(tool: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "session_id": "s-11194",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input,
    })
}

/// The audit file under `data`, empty when absent.
fn audit(data: &Path) -> String {
    let path = data.join("trusty-mpm").join("egress-architect-allowed.log");
    std::fs::read_to_string(path).unwrap_or_default()
}

/// 🔴 #11194: a PM main thread's send to the owner's DM and a canvas with no
/// `channel_id` are allowed, each with one audit line and no message body.
#[test]
fn pm_main_thread_dm_send_and_bare_canvas_are_allowed_and_audited() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let send = call(
        SEND,
        serde_json::json!({ "channel": OWNER_DM, "text": "BODY-11194" }),
    );
    assert_allowed(&run_in(tmp.path(), &send, &[]), "DM send");
    let canvas = call(
        CANVAS,
        serde_json::json!({ "title": "t", "markdown": "BODY-11194" }),
    );
    assert_allowed(&run_in(tmp.path(), &canvas, &[]), "bare canvas");

    let text = audit(tmp.path());
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "one line per allow: {text}");
    assert!(
        lines[0].starts_with("egress-pm-dm-allowed time=")
            && lines[0].ends_with(&format!(
                "session=s-11194 family=slack tool={SEND} channel={OWNER_DM}"
            )),
        "{text}"
    );
    assert!(
        lines[1].ends_with(&format!("tool={CANVAS} channel=-")),
        "{text}"
    );
    assert!(!text.contains("BODY-11194"), "no body in the audit: {text}");
}

/// 🔴 #11194: another channel, a missing `channel`, a canvas naming another
/// channel, and a subagent's DM send all stay denied with no audit line.
#[test]
fn pm_dm_allow_refuses_every_other_target_and_subagents() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let other = call(
        SEND,
        serde_json::json!({ "channel": "C0SYNTH0001", "text": "x" }),
    );
    assert_denied(&run_in(tmp.path(), &other, &[]), "other channel");
    let missing = call(SEND, serde_json::json!({ "text": "x" }));
    assert_denied(&run_in(tmp.path(), &missing, &[]), "missing channel");
    let canvas = call(CANVAS, serde_json::json!({ "channel_id": "C0SYNTH0001" }));
    assert_denied(&run_in(tmp.path(), &canvas, &[]), "canvas other channel");
    let dm = serde_json::json!({ "channel": OWNER_DM, "text": "x" });
    let mut sub = call(SEND, dm.clone());
    sub["agent_id"] = serde_json::json!("agent-synth-1");
    assert_denied(&run_in(tmp.path(), &sub, &[]), "subagent agent_id");
    let main = call(SEND, dm);
    let env = [("CLAUDE_MPM_SUB_AGENT", "1")];
    assert_denied(&run_in(tmp.path(), &main, &env), "subagent env");
    assert_eq!(audit(tmp.path()), "", "no deny writes an allow line");
}

/// 🔴 #11194: an allowed DM send whose audit line cannot be written denies.
#[test]
fn pm_dm_send_with_unwritable_audit_is_denied() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // A file where the trusty-mpm data directory must go.
    std::fs::write(tmp.path().join("trusty-mpm"), "x").expect("write blocker");
    let send = call(
        SEND,
        serde_json::json!({ "channel": OWNER_DM, "text": "x" }),
    );
    let out = run_in(tmp.path(), &send, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is one JSON object ({e}): {stdout:?}"));
    let output = &value["hookSpecificOutput"];
    assert_eq!(output["permissionDecision"], "deny", "{value}");
    let reason = output["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    assert!(reason.contains("#11194"), "{reason}");
}
