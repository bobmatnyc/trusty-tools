//! Unit tests for the NDJSON IPC message protocol.
//!
//! Why: Message framing, parsing, and summary extraction are the wire contract
//! between PM and sub-agents; round-trip and edge-case coverage guards it.
//! What: serialize/parse round-trips, malformed-line handling, summary extraction.
//! Test: This module is itself the test coverage.

use super::*;

#[test]
fn task_roundtrip() {
    let msg = IpcMessage::Task {
        id: "abc".into(),
        task: "do stuff".into(),
        history: None,
        session_reset: None,
    };
    let wire = serialize_message(&msg).unwrap();
    assert!(wire.ends_with('\n'));
    let back = parse_message(&wire).unwrap();
    assert_eq!(msg, back);
}

#[test]
fn task_with_history_roundtrip() {
    let msg = IpcMessage::new_task_with_history(
        "go",
        vec![
            HistoryMessage::user("earlier-q"),
            HistoryMessage::assistant("earlier-a"),
        ],
    );
    let wire = serialize_message(&msg).unwrap();
    let back = parse_message(&wire).unwrap();
    assert_eq!(msg, back);
    match back {
        IpcMessage::Task { history, .. } => {
            let h = history.expect("history present");
            assert_eq!(h.len(), 2);
            assert_eq!(h[0].role, "user");
            assert_eq!(h[1].role, "assistant");
        }
        _ => panic!("expected Task"),
    }
}

#[test]
fn task_legacy_wire_parses_without_history() {
    // Pre-#51 wire format omits `history`/`session_reset`.
    let wire = r#"{"type":"task","id":"x","task":"y"}"#;
    let msg = parse_message(wire).unwrap();
    match msg {
        IpcMessage::Task {
            history,
            session_reset,
            ..
        } => {
            assert!(history.is_none());
            assert!(session_reset.is_none());
        }
        _ => panic!("expected Task"),
    }
}

#[test]
fn result_roundtrip() {
    let msg = IpcMessage::new_result("id1", "output");
    let wire = serialize_message(&msg).unwrap();
    let back = parse_message(&wire).unwrap();
    assert_eq!(msg, back);
}

#[test]
fn error_roundtrip() {
    let msg = IpcMessage::new_error("id1", "something broke");
    let wire = serialize_message(&msg).unwrap();
    let back = parse_message(&wire).unwrap();
    assert_eq!(msg, back);
}

#[test]
fn parse_rejects_garbage() {
    assert!(parse_message("not json").is_err());
}

#[test]
fn result_with_summary_roundtrip() {
    let msg = IpcMessage::new_result_with_summary(
        "id1",
        "full content body",
        Some("short summary".into()),
    );
    let wire = serialize_message(&msg).unwrap();
    let back = parse_message(&wire).unwrap();
    assert_eq!(msg, back);
    match back {
        IpcMessage::Result { summary, .. } => {
            assert_eq!(summary, Some("short summary".to_string()));
        }
        _ => panic!("expected Result"),
    }
}

#[test]
fn result_with_usage_roundtrip() {
    let usage = TokenUsage::new(100, 50, 10, 5);
    let msg = IpcMessage::new_result_full("id1", "content", Some("sum".into()), Some(usage));
    let wire = serialize_message(&msg).unwrap();
    let back = parse_message(&wire).unwrap();
    assert_eq!(msg, back);
    match back {
        IpcMessage::Result { usage, .. } => {
            let u = usage.expect("usage present");
            assert_eq!(u.prompt_tokens, 100);
            assert_eq!(u.completion_tokens, 50);
            assert_eq!(u.cache_read_tokens, 10);
            assert_eq!(u.cache_creation_tokens, 5);
        }
        _ => panic!("expected Result"),
    }
}

#[test]
fn result_without_summary_parses_old_wire_format() {
    // Prior-version wire format lacks `summary`.
    let wire = r#"{"type":"result","id":"x","content":"y","status":"success"}"#;
    let msg = parse_message(wire).unwrap();
    match msg {
        IpcMessage::Result { summary, .. } => assert!(summary.is_none()),
        _ => panic!("expected Result"),
    }
}

#[test]
fn extract_summary_finds_trailing_section() {
    let content =
        "Some code here\nmore stuff\n\n## Summary\nThis is the concise summary the agent wrote.";
    let s = extract_summary(content);
    assert_eq!(s, "This is the concise summary the agent wrote.");
}

#[test]
fn extract_summary_is_case_insensitive() {
    let content = "header\n\n## SUMMARY\nbody text here";
    let s = extract_summary(content);
    assert_eq!(s, "body text here");
}

#[test]
fn extract_summary_finds_last_summary_section() {
    // Agents may mention the word "summary" inline but only the trailing
    // ## Summary block counts.
    let content = "## Summary of Research Findings (inline)\nignored\n\n## Summary\nreal summary";
    let s = extract_summary(content);
    assert_eq!(s, "real summary");
}

#[test]
fn extract_summary_fallback_uses_prefix() {
    let long = "a".repeat(2000);
    let s = extract_summary(&long);
    assert_eq!(s.chars().count(), 500);
}

#[test]
fn extract_summary_fallback_short_content_verbatim() {
    let content = "short output";
    let s = extract_summary(content);
    assert_eq!(s, "short output");
}

#[test]
fn extract_files_from_content_parses_multiple_files() {
    let content = "Intro text.\n\n## File: app/main.py\n```python\nprint(\"hi\")\n```\n\n## File: app/util.py\n```python\ndef f():\n    return 1\n```\n";
    let files = extract_files_from_content(content);
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].0, PathBuf::from("app/main.py"));
    assert!(files[0].1.contains("print(\"hi\")"));
    assert!(files[0].1.ends_with('\n'));
    assert_eq!(files[1].0, PathBuf::from("app/util.py"));
    assert!(files[1].1.contains("def f():"));
}

#[test]
fn extract_files_from_content_returns_empty_when_no_markers() {
    let files = extract_files_from_content("Just prose, no file sections.");
    assert!(files.is_empty());
}

#[test]
fn extract_files_from_content_handles_backtick_header() {
    let content = "### `scripts/go.sh`\n```bash\necho hi\n```\n";
    let files = extract_files_from_content(content);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, PathBuf::from("scripts/go.sh"));
    assert!(files[0].1.contains("echo hi"));
}
