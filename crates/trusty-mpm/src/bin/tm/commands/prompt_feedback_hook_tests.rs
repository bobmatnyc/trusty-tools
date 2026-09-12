//! Capture-hook tests for #7688 — transcript read, row build, and fail-open.

use super::*;
use tempfile::TempDir;

/// One JSON-Lines transcript line for an assistant text block.
fn assistant_line(id: &str, text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": { "role": "assistant", "id": id, "content": [{"type": "text", "text": text}] }
    })
    .to_string()
}

/// Write a transcript holding one assistant message with `text`.
fn transcript_with(dir: &Path, text: &str) -> std::path::PathBuf {
    let path = dir.join("transcript.jsonl");
    std::fs::write(&path, format!("{}\n", assistant_line("msg-1", text))).expect("write");
    path
}

fn stop_payload(transcript: &Path) -> Value {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "sess-1",
        "cwd": "/tmp/proj",
        "transcript_path": transcript.to_string_lossy(),
    })
}

#[test]
fn a_stop_payload_writes_a_pm_row() {
    let tmp = TempDir::new().expect("tempdir");
    let transcript = transcript_with(
        tmp.path(),
        "Done.\n\n## Prompt feedback\n\nThe rung was ambiguous.\n",
    );
    let root = tmp.path().join("root");

    assert!(capture_into(&root, Some(&stop_payload(&transcript))));

    let rows = trusty_mpm::core::prompt_feedback::read_rows(
        &root,
        &trusty_mpm::core::prompt_feedback::ReadFilter::default(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].agent_type, PM_AGENT_TYPE, "a Stop is the PM's own");
    assert_eq!(rows[0].session_id.as_deref(), Some("sess-1"));
    assert_eq!(rows[0].feedback, "The rung was ambiguous.");
}

#[test]
fn a_subagent_stop_payload_records_its_type() {
    let tmp = TempDir::new().expect("tempdir");
    let transcript = transcript_with(tmp.path(), "## Prompt feedback\n\nToo many gates named.\n");
    let root = tmp.path().join("root");
    let payload = serde_json::json!({
        "hook_event_name": "SubagentStop",
        "session_id": "sess-1",
        "agent_id": "a403cdbc",
        "agent_type": "rust-engineer",
        "agent_transcript_path": transcript.to_string_lossy(),
    });

    assert!(capture_into(&root, Some(&payload)));

    let rows = trusty_mpm::core::prompt_feedback::read_rows(
        &root,
        &trusty_mpm::core::prompt_feedback::ReadFilter::default(),
    );
    assert_eq!(rows[0].agent_type, "rust-engineer");
}

/// `SubagentStop` carries BOTH transcripts. Reading the parent's would
/// attribute the parent's last message to the subagent's type.
#[test]
fn a_subagent_stop_reads_its_own_transcript_not_the_parents() {
    let tmp = TempDir::new().expect("tempdir");
    let parent = tmp.path().join("parent.jsonl");
    std::fs::write(
        &parent,
        format!(
            "{}\n",
            assistant_line("p", "## Prompt feedback\n\nPARENT SAID THIS\n")
        ),
    )
    .expect("write parent");
    let child = tmp.path().join("child.jsonl");
    std::fs::write(
        &child,
        format!(
            "{}\n",
            assistant_line("c", "## Prompt feedback\n\nCHILD SAID THIS\n")
        ),
    )
    .expect("write child");

    let root = tmp.path().join("root");
    let payload = serde_json::json!({
        "hook_event_name": "SubagentStop",
        "agent_type": "qa",
        "transcript_path": parent.to_string_lossy(),
        "agent_transcript_path": child.to_string_lossy(),
    });
    assert!(capture_into(&root, Some(&payload)));

    let rows = trusty_mpm::core::prompt_feedback::read_rows(
        &root,
        &trusty_mpm::core::prompt_feedback::ReadFilter::default(),
    );
    assert_eq!(rows[0].feedback, "CHILD SAID THIS");
}

/// 🔴 THE FAIL-OPEN ARM. An unreadable transcript writes nothing and, above
/// all, does not panic or propagate — `capture_into` returns and the caller
/// exits 0.
#[test]
fn an_unreadable_transcript_writes_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    let missing = tmp.path().join("does-not-exist.jsonl");

    let landed = capture_into(&root, Some(&stop_payload(&missing)));

    assert!(!landed, "an unreadable transcript must capture nothing");
    assert!(
        !trusty_mpm::core::prompt_feedback::ledger_path(&root).exists(),
        "no ledger may be created for a capture that found nothing"
    );
}

#[test]
fn a_payload_naming_no_transcript_writes_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    let payload = serde_json::json!({ "hook_event_name": "Stop", "session_id": "s" });
    assert!(!capture_into(&root, Some(&payload)));
}

#[test]
fn no_payload_at_all_writes_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    assert!(!capture_into(&tmp.path().join("root"), None));
}

/// The common case: a turn that simply did not emit the section.
#[test]
fn a_message_without_the_section_writes_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let transcript = transcript_with(tmp.path(), "Just the work, no addendum.");
    let root = tmp.path().join("root");
    assert!(!capture_into(&root, Some(&stop_payload(&transcript))));
}

#[test]
fn joins_multi_block_assistant_messages() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("t.jsonl");
    // Claude Code writes ONE LINE PER CONTENT BLOCK; the section can straddle
    // two of them, so taking only the last line would miss it.
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            assistant_line("msg-1", "## Prompt feedback"),
            assistant_line("msg-1", "\nsplit across blocks\n")
        ),
    )
    .expect("write");

    let got = final_assistant_message(&path).expect("a message");
    assert_eq!(
        trusty_mpm::core::prompt_feedback::extract_feedback(&got).as_deref(),
        Some("split across blocks")
    );
}

/// Blocks of an EARLIER message must not be joined onto the final one.
#[test]
fn earlier_messages_are_not_joined_onto_the_final_one() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("t.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            assistant_line("msg-1", "OLD TURN"),
            assistant_line("msg-2", "NEW TURN")
        ),
    )
    .expect("write");

    let got = final_assistant_message(&path).expect("a message");
    assert_eq!(got, "NEW TURN");
}

#[test]
fn reads_the_final_message_from_the_tail_of_a_large_transcript() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("big.jsonl");
    let mut body = String::new();
    // Comfortably past TRANSCRIPT_TAIL_BYTES so the seek path is exercised.
    for i in 0..12_000 {
        body.push_str(&assistant_line(&format!("old-{i}"), &"filler ".repeat(40)));
        body.push('\n');
    }
    body.push_str(&assistant_line(
        "final",
        "## Prompt feedback\n\nthe tail was read\n",
    ));
    body.push('\n');
    std::fs::write(&path, &body).expect("write");
    assert!(
        body.len() as u64 > TRANSCRIPT_TAIL_BYTES,
        "the fixture must exceed the cap to test the seek"
    );

    let got = final_assistant_message(&path).expect("a message");
    assert_eq!(
        trusty_mpm::core::prompt_feedback::extract_feedback(&got).as_deref(),
        Some("the tail was read")
    );
}

#[test]
fn a_transcript_with_no_assistant_turn_yields_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("t.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
    )
    .expect("write");
    assert!(final_assistant_message(&path).is_none());
}

#[test]
fn records_no_digest_when_the_compiled_prompt_is_absent() {
    let tmp = TempDir::new().expect("tempdir");
    let transcript = transcript_with(tmp.path(), "## Prompt feedback\n\nno digest\n");
    let root = tmp.path().join("root");
    assert!(capture_into(&root, Some(&stop_payload(&transcript))));

    let rows = trusty_mpm::core::prompt_feedback::read_rows(
        &root,
        &trusty_mpm::core::prompt_feedback::ReadFilter::default(),
    );
    assert!(
        rows[0].prompt_digest.is_none(),
        "an unreadable compiled prompt is a null digest, not a failure"
    );
}

#[test]
fn records_the_compiled_prompt_digest_when_one_exists() {
    let tmp = TempDir::new().expect("tempdir");
    let project = tmp.path().join("proj");
    let compiled = trusty_mpm::core::instruction_pipeline::compiled_prompt_path(&project, "sess-1");
    std::fs::create_dir_all(compiled.parent().expect("parent")).expect("mkdir");
    std::fs::write(&compiled, "COMPILED PROMPT BYTES").expect("write compiled");

    let transcript = transcript_with(tmp.path(), "## Prompt feedback\n\nwith digest\n");
    let root = tmp.path().join("root");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "sess-1",
        "cwd": project.to_string_lossy(),
        "transcript_path": transcript.to_string_lossy(),
    });
    assert!(capture_into(&root, Some(&payload)));

    let rows = trusty_mpm::core::prompt_feedback::read_rows(
        &root,
        &trusty_mpm::core::prompt_feedback::ReadFilter::default(),
    );
    let digest = rows[0].prompt_digest.as_deref().expect("a digest");
    assert_eq!(digest, sha256_hex(b"COMPILED PROMPT BYTES"));
    assert_eq!(digest.len(), 64, "sha256 renders as 64 hex chars");
}
