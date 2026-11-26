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

/// The hook's guard directory under `home`, named through install_paths.
fn guard_dir(home: &Path) -> std::path::PathBuf {
    home.join(trusty_common::install_paths::MPM_DIR_NAME)
        .join("guard")
}

fn log_lines(home: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(guard_dir(home).join("shadow.jsonl"))
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
    let base = home.path().join(trusty_common::install_paths::MPM_DIR_NAME);
    std::fs::remove_dir_all(&base).expect("clear");
    std::fs::create_dir_all(&base).expect("base");
    std::fs::write(base.join("guard"), "not a dir").expect("blocker");
    run(home.path(), &bash("rm -rf ~"));
    refused(home.path(), write.to_string().as_bytes());
    assert!(base.join("guard").is_file(), "the blocker is untouched");
}

/// #10690 (critic HIGH 1): a FIFO planted at the log path does not delay the
/// deny; the hook prints it and exits within the bound.
#[test]
fn a_fifo_log_path_still_gets_the_deny() {
    let home = tempfile::tempdir().expect("home");
    let log = guard_dir(home.path()).join("shadow.jsonl");
    std::fs::create_dir_all(log.parent().expect("dir")).expect("dir");
    let made = std::process::Command::new("mkfifo")
        .arg(&log)
        .status()
        .expect("mkfifo");
    assert!(made.success());
    let anchor = home.path().join(".claude/settings.json");
    let write =
        json!({ "tool_name": "Write", "tool_input": { "file_path": anchor, "content": "{}" } });
    let mut child = common::tm_command_in(home.path())
        .args(["hook", "--pm-guard-shadow"])
        .current_dir(home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(write.to_string().as_bytes())
        .expect("write");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while child.try_wait().expect("try_wait").is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the hook blocked on a FIFO log path");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().expect("output");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\"permissionDecision\":\"deny\""), "{text}");
}
