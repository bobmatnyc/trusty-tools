//! End-to-end proof of `tm hook --pm-guard-shadow` (#11431).
//!
//! Why: only the built binary proves the shadow guard's contract: exit 0 on
//! every input, empty stdout and a log line for an observe-only rule, and a
//! printed deny for rule 1 or a call rule 1 cannot judge (#10690, ruling D0u).
//! What: spawns the hook with a scratch `$HOME` and pipes `PreToolUse`
//! payloads in.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_shadow_11431::`.

use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;

use serde_json::json;

use crate::common;

/// Run the hook in `home` on `stdin`; assert exit 0 and return stdout.
fn hook(home: &Path, stdin: &[u8]) -> String {
    let mut child = common::tm_command_in(home)
        .args(["hook", "--pm-guard-shadow"])
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard-shadow`");
    // A huge payload may be cut off by the bounded read; a broken pipe is fine.
    let _ = child.stdin.take().expect("stdin").write_all(stdin);
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "the shadow hook always exits 0: {out:?}"
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// Run the hook; assert it allowed (empty stdout).
fn run(home: &Path, stdin: &[u8]) {
    let out = hook(home, stdin);
    assert!(out.is_empty(), "no decision is printed: {out}");
}

/// Run the hook; assert it printed the trust-anchor deny.
fn refused(home: &Path, stdin: &[u8]) {
    let out = hook(home, stdin);
    let deny: serde_json::Value = serde_json::from_str(out.trim()).expect("one JSON deny");
    let body = &deny["hookSpecificOutput"];
    assert_eq!(body["permissionDecision"], "deny", "{out}");
    let reason = body["permissionDecisionReason"].as_str().unwrap_or("");
    assert!(reason.starts_with("tm pm-guard: rule 1"), "{reason}");
}

fn bash(cmd: &str) -> Vec<u8> {
    json!({ "session_id": "s-e2e", "tool_name": "Bash", "tool_input": { "command": cmd } })
        .to_string()
        .into_bytes()
}

fn log_lines(home: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(home.join(".trusty-mpm/guard/shadow.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("log line is JSON"))
        .collect()
}

#[test]
fn a_would_refuse_is_logged_and_never_blocks() {
    let home = tempfile::tempdir().expect("home");
    run(
        home.path(),
        &bash("cargo test -p trusty-mpm --no-fail-fast"),
    );
    assert!(log_lines(home.path()).is_empty(), "ordinary work is silent");
    run(home.path(), &bash("rm -rf ~"));
    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["rule"], "R2");
    assert_eq!(lines[0]["session_id"], "s-e2e");
}

/// #10690 (ruling D0u): rule 1 refuses and records in shadow mode, and a call
/// rule 1 cannot judge is refused; an unwritable log changes no decision.
#[test]
fn an_r1_write_is_refused_and_a_malformed_call_too() {
    let home = tempfile::tempdir().expect("home");
    let anchor = home.path().join(".claude/settings.json");
    let write = json!({ "session_id": "s-e2e", "tool_name": "Write",
        "tool_input": { "file_path": anchor, "content": "{}" } });
    refused(home.path(), write.to_string().as_bytes());
    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["rule"], "R1");
    assert_eq!(lines[0]["enforced"], true);
    refused(home.path(), b"{not json");
    refused(home.path(), b"");
    let huge = format!("rm -rf ~ {}", "a".repeat(3 << 20));
    refused(home.path(), &bash(&huge));
    assert_eq!(log_lines(home.path()).len(), 4, "each refusal is recorded");
    // `.trusty-mpm/guard` is a file, so the log directory cannot be created.
    let base = home.path().join(".trusty-mpm");
    std::fs::remove_dir_all(&base).expect("clear");
    std::fs::create_dir_all(&base).expect("base");
    std::fs::write(base.join("guard"), "not a dir").expect("blocker");
    run(home.path(), &bash("rm -rf ~"));
    refused(home.path(), write.to_string().as_bytes());
    assert!(base.join("guard").is_file(), "the blocker is untouched");
}
