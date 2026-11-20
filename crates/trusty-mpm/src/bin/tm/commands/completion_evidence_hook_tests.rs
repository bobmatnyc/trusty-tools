//! Tests for `tm hook --completion-evidence` (#10575).

use serde_json::json;

use super::respond;

/// A one-message transcript holding `text`; returns the tempdir and path.
fn transcript(text: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.jsonl");
    let line = json!({
        "message": { "role": "assistant", "id": "m1",
                     "content": [{ "type": "text", "text": text }] }
    });
    std::fs::write(&path, format!("{line}\n")).expect("write transcript");
    let p = path.display().to_string();
    (dir, p)
}

fn stop(path: &str) -> serde_json::Value {
    json!({ "hook_event_name": "Stop", "transcript_path": path })
}

#[test]
fn a_done_claim_without_a_map_warns_through_the_hook() {
    let (_d, path) = transcript("Done. The goals-only rewrite is complete.");
    let (out, logs) = respond(Some(&stop(&path)));
    let out = out.expect("a warning");
    let value: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(
        value["systemMessage"]
            .as_str()
            .expect("msg")
            .contains("MISSING_EVIDENCE_MAP")
    );
    assert!(
        value.get("decision").is_none(),
        "warn-only: never a decision"
    );
    assert_eq!(logs.len(), 1);
}

#[test]
fn an_invented_target_warns_through_the_hook() {
    let (_d, path) = transcript("Next: cut the rules to 600 words.");
    let (out, _) = respond(Some(&stop(&path)));
    assert!(out.expect("warning").contains("INVENTED_TARGET"));
}

#[test]
fn a_compliant_message_prints_nothing() {
    let text = "Done.\n## Evidence map\n- Verified: a\n- Helper-reported: b\n\
                - Unfinished: none\n- Untested: none\n";
    let (_d, path) = transcript(text);
    assert_eq!(respond(Some(&stop(&path))), (None, Vec::new()));
}

#[test]
fn each_failure_arm_logs_and_prints_nothing() {
    let (out, logs) = respond(None);
    assert!(out.is_none() && logs.len() == 1, "no payload");
    let (out, logs) = respond(Some(&json!({ "hook_event_name": "Stop" })));
    assert!(out.is_none() && logs.len() == 1, "no transcript path");
    let (out, logs) = respond(Some(&stop("/nonexistent/t.jsonl")));
    assert!(out.is_none() && logs.len() == 1, "unreadable transcript");
    let (out, logs) = respond(Some(&json!({ "hook_event_name": "SubagentStop" })));
    assert!(out.is_none() && logs.is_empty(), "not a Stop");
}
