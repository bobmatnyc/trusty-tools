//! End-to-end proof of `tm hook --completion-evidence` (#10575).
//!
//! Why: only the built binary proves the hook reads the transcript named by a
//! `Stop` payload, warns through `systemMessage` without a blocking
//! `decision`, and exits 0 on every arm.
//! What: spawns the hook with a scratch `$HOME` and pipes a `Stop` payload in.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_completion_evidence_10575::`.

use std::io::Write as _;
use std::process::Stdio;

use crate::common;

fn run(stdin: &str) -> (String, String) {
    let home = tempfile::tempdir().expect("home");
    let mut child = common::tm_command_in(home.path())
        .args(["hook", "--completion-evidence"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --completion-evidence`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the hook always exits 0: {out:?}");
    (
        String::from_utf8(out.stdout).expect("utf-8"),
        String::from_utf8(out.stderr).expect("utf-8"),
    )
}

fn stop_with(text: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.jsonl");
    let line = serde_json::json!({
        "message": { "role": "assistant", "id": "m1",
                     "content": [{ "type": "text", "text": text }] }
    });
    std::fs::write(&path, format!("{line}\n")).expect("transcript");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "transcript_path": path.display().to_string(),
    });
    (dir, payload.to_string())
}

#[test]
fn a_done_claim_without_an_evidence_map_warns_and_never_blocks() {
    let (_d, payload) = stop_with("Done: goals only applied.");
    let (stdout, stderr) = run(&payload);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert!(
        value["systemMessage"]
            .as_str()
            .expect("msg")
            .contains("MISSING_EVIDENCE_MAP")
    );
    assert!(value.get("decision").is_none(), "{stdout}");
    assert!(stderr.contains("MISSING_EVIDENCE_MAP"), "{stderr}");
}

#[test]
fn an_honest_incomplete_report_draws_no_warning() {
    let (_d, payload) = stop_with("Not finished: 12 of 24 rewritten, none checked yet.");
    let (stdout, _) = run(&payload);
    assert!(stdout.trim().is_empty(), "{stdout}");
}

#[test]
fn a_garbage_payload_prints_nothing_and_logs() {
    let (stdout, stderr) = run("not json");
    assert!(stdout.trim().is_empty(), "{stdout}");
    assert!(stderr.contains("no usable payload"), "{stderr}");
}
