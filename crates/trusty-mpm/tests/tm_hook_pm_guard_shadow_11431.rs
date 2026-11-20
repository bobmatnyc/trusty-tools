//! End-to-end proof of `tm hook --pm-guard-shadow` (#11431).
//!
//! Why: only the built binary proves the shadow guard never blocks: exit 0
//! and empty stdout on every input, a would-refuse in the log instead.
//! What: spawns the hook with a scratch `$HOME` and pipes `PreToolUse`
//! payloads in.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_shadow_11431::`.

use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;

use serde_json::json;

use crate::common;

/// Run the hook in `home` on `stdin`; assert exit 0 and empty stdout.
fn run(home: &Path, stdin: &[u8]) {
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
    assert!(
        out.stdout.is_empty(),
        "no decision is ever printed: {out:?}"
    );
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

#[test]
fn malformed_huge_and_unwritable_inputs_exit_zero() {
    let home = tempfile::tempdir().expect("home");
    run(home.path(), b"{not json");
    run(home.path(), b"");
    let huge = format!("rm -rf ~ {}", "a".repeat(3 << 20));
    run(home.path(), &bash(&huge));
    assert!(
        log_lines(home.path()).is_empty(),
        "an oversized payload is skipped"
    );
    // `.trusty-mpm/guard` is a file, so the log directory cannot be created.
    let base = home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&base).expect("base");
    std::fs::write(base.join("guard"), "not a dir").expect("blocker");
    run(home.path(), &bash("rm -rf ~"));
    assert!(base.join("guard").is_file(), "the blocker is untouched");
}
