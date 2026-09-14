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

/// 🔴 #7702: a `SubagentStop` that names no `agent_type` must NOT be booked as
/// the PM. `tm prompt-feedback --summary` answers "which agent type complains
/// most", so folding an untyped subagent into `pm` is a wrong answer to the
/// only question it asks. Payload shape per `hook_payload.rs`'s live-captured
/// `SubagentStop` fixture, with `agent_type` dropped. Fails on 96efd6138,
/// which recorded `pm`.
#[test]
fn a_subagent_stop_without_an_agent_type_is_not_the_pm() {
    let tmp = TempDir::new().expect("tempdir");
    let transcript = transcript_with(tmp.path(), "## Prompt feedback\n\nuntyped subagent\n");
    let root = tmp.path().join("root");
    let payload = serde_json::json!({
        "hook_event_name": "SubagentStop",
        "session_id": "sess-1",
        "agent_id": "a403cdbc078b5c474",
        "agent_transcript_path": transcript.to_string_lossy(),
    });

    assert!(capture_into(&root, Some(&payload)));

    let rows = trusty_mpm::core::prompt_feedback::read_rows(
        &root,
        &trusty_mpm::core::prompt_feedback::ReadFilter::default(),
    );
    // The LITERAL, not the constant: this string is the ledger's stored
    // vocabulary, and `--agent unknown-subagent` is what an operator types.
    assert_eq!(rows[0].agent_type, "unknown-subagent");
    assert_ne!(
        rows[0].agent_type, PM_AGENT_TYPE,
        "an untyped subagent stop must never be counted as the PM"
    );
}

/// An unrecognised or absent `hook_event_name` is not a `Stop` either, so it
/// takes the sentinel rather than the PM's own bucket.
#[test]
fn an_event_with_no_name_is_not_the_pm() {
    let tmp = TempDir::new().expect("tempdir");
    let transcript = transcript_with(tmp.path(), "## Prompt feedback\n\nno event name\n");
    let root = tmp.path().join("root");
    let payload = serde_json::json!({
        "session_id": "sess-1",
        "transcript_path": transcript.to_string_lossy(),
    });

    assert!(capture_into(&root, Some(&payload)));

    let rows = trusty_mpm::core::prompt_feedback::read_rows(
        &root,
        &trusty_mpm::core::prompt_feedback::ReadFilter::default(),
    );
    assert_eq!(rows[0].agent_type, "unknown-subagent");
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

// ── Report-length measurement (owner ruling 2026-09-14) ──────────────────────

/// `n` distinct prose words carrying no failure marker.
fn clean_words(n: usize) -> String {
    (0..n)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `SubagentStop` payload for `transcript`, typed `rust-engineer`.
fn subagent_stop_payload(transcript: &Path) -> Value {
    serde_json::json!({
        "hook_event_name": "SubagentStop",
        "session_id": "sess-1",
        "agent_type": "rust-engineer",
        "agent_transcript_path": transcript.to_string_lossy(),
    })
}

fn ledger_rows(root: &Path) -> Vec<trusty_mpm::core::prompt_feedback::FeedbackRow> {
    trusty_mpm::core::prompt_feedback::read_rows(
        root,
        &trusty_mpm::core::prompt_feedback::ReadFilter::default(),
    )
}

#[test]
fn an_over_cap_clean_report_warns_and_records_the_row() {
    let tmp = TempDir::new().expect("tempdir");
    let transcript = transcript_with(tmp.path(), &clean_words(350));
    let root = tmp.path().join("root");

    capture_into(&root, Some(&subagent_stop_payload(&transcript)));

    let rows = ledger_rows(&root);
    assert_eq!(rows.len(), 1, "one report-length observation");
    let warning = &rows[0].feedback;
    for needle in ["rust-engineer", "350", "300-word cap"] {
        assert!(
            warning.contains(needle),
            "the warning must name {needle:?}; got {warning:?}"
        );
    }
    assert_eq!(
        warning.lines().count(),
        1,
        "the warning is ONE line; got {warning:?}"
    );
}

#[test]
fn a_fenced_block_does_not_count_toward_the_report_cap() {
    let tmp = TempDir::new().expect("tempdir");
    // 250 prose words is under the 300 cap; the 900-word fenced block is raw
    // gate output, which BASE-AGENT.md exempts by name.
    let message = format!("{}\n\n```\n{}\n```\n", clean_words(250), clean_words(900));
    let transcript = transcript_with(tmp.path(), &message);
    let root = tmp.path().join("root");

    capture_into(&root, Some(&subagent_stop_payload(&transcript)));

    assert!(
        ledger_rows(&root).is_empty(),
        "fenced output must not push a compliant report over the cap"
    );
}

#[test]
fn a_failure_report_is_measured_against_the_higher_cap() {
    let tmp = TempDir::new().expect("tempdir");
    let message = format!("The clippy gate failed.\n\n{}", clean_words(350));
    let transcript = transcript_with(tmp.path(), &message);
    let root = tmp.path().join("root");

    capture_into(&root, Some(&subagent_stop_payload(&transcript)));

    assert!(
        ledger_rows(&root).is_empty(),
        "a report naming a failure is capped at 600 words, not 300"
    );
}

#[test]
fn a_malformed_payload_produces_no_length_warning() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");

    // Fail-open is the correct arm: a `SubagentStop` hook that fails is
    // surfaced to the operator as a broken session, and a length observation
    // can never be worth blocking a stop over.
    assert!(!capture_into(&root, None));
    let no_transcript = serde_json::json!({"hook_event_name": "SubagentStop"});
    assert!(!capture_into(&root, Some(&no_transcript)));

    assert!(
        ledger_rows(&root).is_empty(),
        "a malformed payload writes nothing at all"
    );
}

#[test]
fn a_pm_stop_is_not_measured_against_the_hand_back_cap() {
    let tmp = TempDir::new().expect("tempdir");
    let transcript = transcript_with(tmp.path(), &clean_words(350));
    let root = tmp.path().join("root");

    capture_into(&root, Some(&stop_payload(&transcript)));

    assert!(
        ledger_rows(&root).is_empty(),
        "the PM's own turn is not a hand-back report"
    );
}
