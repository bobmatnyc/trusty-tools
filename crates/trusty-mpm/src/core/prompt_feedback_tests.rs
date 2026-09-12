//! Extraction, append, read-back and summary tests for the #7688 ledger.

use super::*;
use tempfile::TempDir;

fn row(agent: &str, feedback: &str) -> FeedbackRow {
    FeedbackRow {
        ts: "2026-09-12T10:00:00Z".to_string(),
        session_id: Some("sess-1".to_string()),
        agent_type: agent.to_string(),
        prompt_digest: Some("abc123".to_string()),
        feedback: feedback.to_string(),
    }
}

#[test]
fn ledger_path_is_a_root_level_jsonl_file() {
    let path = ledger_path(Path::new("/tmp/root"));
    assert_eq!(path, Path::new("/tmp/root/prompt-feedback.jsonl"));
}

// ── extraction ──────────────────────────────────────────────────────────────

#[test]
fn extracts_the_section_body() {
    let message = "Here is the work.\n\n## Prompt feedback\n\nThe scope was clear.\nThe CI note was unnecessary.\n";
    let got = extract_feedback(message).expect("a section");
    assert_eq!(got, "The scope was clear.\nThe CI note was unnecessary.");
}

/// A response that QUOTES the instruction before complying must not capture the
/// quote — the extractor takes the LAST heading.
#[test]
fn extracts_the_last_section() {
    let message = "\
I was asked to end with a section.

## Prompt feedback

first

## Notes

middle

## Prompt feedback

second
";
    assert_eq!(extract_feedback(message).expect("a section"), "second");
}

#[test]
fn stops_at_the_next_heading() {
    let message = "## Prompt feedback\n\nkeep this\n\n## Something else\n\ndrop this\n";
    assert_eq!(extract_feedback(message).expect("a section"), "keep this");
}

/// A `###` is DEEPER than the section heading, so it belongs to the feedback.
#[test]
fn a_deeper_heading_stays_inside_the_section() {
    let message = "## Prompt feedback\n\nunclear:\n\n### scope\n\nthe rung was ambiguous\n";
    let got = extract_feedback(message).expect("a section");
    assert!(got.contains("### scope"), "got {got:?}");
    assert!(got.contains("the rung was ambiguous"));
}

#[test]
fn absent_heading_extracts_nothing() {
    assert!(extract_feedback("just the work, no section").is_none());
}

/// A mention inside a sentence is not a section.
#[test]
fn an_inline_mention_is_not_a_section() {
    assert!(extract_feedback("I will add a ## Prompt feedback block next time.").is_none());
}

#[test]
fn an_empty_section_extracts_nothing() {
    assert!(extract_feedback("work\n\n## Prompt feedback\n\n").is_none());
}

#[test]
fn an_oversized_feedback_section_is_truncated() {
    let huge = "x".repeat(20_000);
    let message = format!("## Prompt feedback\n\n{huge}\n");
    let got = extract_feedback(&message).expect("a section");
    assert!(got.len() <= 4096, "stored {} bytes", got.len());
}

/// Truncation must never split a multi-byte character.
#[test]
fn truncation_lands_on_a_char_boundary() {
    let huge = "é".repeat(10_000);
    let message = format!("## Prompt feedback\n\n{huge}\n");
    let got = extract_feedback(&message).expect("a section");
    assert!(got.len() <= 4096);
    assert!(std::str::from_utf8(got.as_bytes()).is_ok());
}

// ── append / read ───────────────────────────────────────────────────────────

#[test]
fn append_then_read_round_trips_a_row() {
    let tmp = TempDir::new().expect("tempdir");
    let written = row("engineer", "the rung was ambiguous");
    assert!(append_row(tmp.path(), &written));

    let back = read_rows(tmp.path(), &ReadFilter::default());
    assert_eq!(back, vec![written]);
}

#[test]
fn read_of_an_absent_ledger_is_empty() {
    let tmp = TempDir::new().expect("tempdir");
    assert!(read_rows(tmp.path(), &ReadFilter::default()).is_empty());
}

#[test]
fn read_rows_is_newest_first() {
    let tmp = TempDir::new().expect("tempdir");
    append_row(tmp.path(), &row("a", "first"));
    append_row(tmp.path(), &row("b", "second"));

    let back = read_rows(tmp.path(), &ReadFilter::default());
    assert_eq!(back[0].feedback, "second", "newest must come first");
    assert_eq!(back[1].feedback, "first");
}

#[test]
fn read_rows_skips_a_malformed_line() {
    let tmp = TempDir::new().expect("tempdir");
    append_row(tmp.path(), &row("a", "good"));
    std::fs::write(
        ledger_path(tmp.path()),
        format!(
            "{}\nnot json at all\n",
            serde_json::to_string(&row("a", "good")).expect("serialise")
        ),
    )
    .expect("write ledger");

    let back = read_rows(tmp.path(), &ReadFilter::default());
    assert_eq!(back.len(), 1, "the valid line must survive the bad one");
}

#[test]
fn read_rows_filters_by_session() {
    let tmp = TempDir::new().expect("tempdir");
    append_row(tmp.path(), &row("a", "keep"));
    let mut other = row("a", "drop");
    other.session_id = Some("sess-2".to_string());
    append_row(tmp.path(), &other);

    let back = read_rows(
        tmp.path(),
        &ReadFilter {
            session: Some("sess-1".to_string()),
            ..ReadFilter::default()
        },
    );
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].feedback, "keep");
}

/// 🔴 #7702: Claude Code mints a new `session_id` on every restart, so
/// `--session <today's id>` must still find the rows the same managed session
/// captured under its earlier id. Folds through
/// `session_links::linked_claude_ids` — the statusline's own #7617 call, not a
/// second sibling lookup. Fails on 96efd6138, which matched by equality alone
/// and returned one row.
#[test]
fn read_rows_folds_a_linked_sibling_session() {
    let tmp = TempDir::new().expect("tempdir");
    // Two ids of one managed session, and a third that belongs to another.
    crate::core::session_links::record_link(tmp.path(), "managed-1", "sess-1");
    crate::core::session_links::record_link(tmp.path(), "managed-1", "sess-2");

    append_row(tmp.path(), &row("pm", "before the restart"));
    let mut after = row("pm", "after the restart");
    after.session_id = Some("sess-2".to_string());
    append_row(tmp.path(), &after);
    let mut stranger = row("pm", "a different session");
    stranger.session_id = Some("sess-9".to_string());
    append_row(tmp.path(), &stranger);

    let back = read_rows(
        tmp.path(),
        &ReadFilter {
            session: Some("sess-2".to_string()),
            ..ReadFilter::default()
        },
    );
    assert_eq!(back.len(), 2, "both ids of the managed session must fold");
    assert_eq!(back[0].feedback, "after the restart");
    assert_eq!(back[1].feedback, "before the restart");
    assert!(
        !back.iter().any(|r| r.feedback == "a different session"),
        "an unlinked session's rows must never fold in"
    );
}

/// The fold never widens an UNLINKED id: with no link store at all, the filter
/// is still exact equality.
#[test]
fn read_rows_without_a_link_store_matches_the_id_alone() {
    let tmp = TempDir::new().expect("tempdir");
    append_row(tmp.path(), &row("pm", "keep"));
    let mut other = row("pm", "drop");
    other.session_id = Some("sess-2".to_string());
    append_row(tmp.path(), &other);

    let back = read_rows(
        tmp.path(),
        &ReadFilter {
            session: Some("sess-1".to_string()),
            ..ReadFilter::default()
        },
    );
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].feedback, "keep");
}

#[test]
fn read_rows_filters_by_agent() {
    let tmp = TempDir::new().expect("tempdir");
    append_row(tmp.path(), &row("engineer", "keep"));
    append_row(tmp.path(), &row("qa", "drop"));

    let back = read_rows(
        tmp.path(),
        &ReadFilter {
            agent: Some("engineer".to_string()),
            ..ReadFilter::default()
        },
    );
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].feedback, "keep");
}

#[test]
fn read_rows_applies_the_limit_after_filtering() {
    let tmp = TempDir::new().expect("tempdir");
    append_row(tmp.path(), &row("engineer", "one"));
    append_row(tmp.path(), &row("qa", "two"));
    append_row(tmp.path(), &row("engineer", "three"));

    let back = read_rows(
        tmp.path(),
        &ReadFilter {
            agent: Some("engineer".to_string()),
            limit: Some(1),
            ..ReadFilter::default()
        },
    );
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].feedback, "three", "the limit keeps the NEWEST");
}

/// 🔴 The fail-open contract. A ledger that cannot be written must not be an
/// error — its one caller is a hook whose non-zero exit breaks the session.
#[test]
fn append_row_on_an_unwritable_ledger_is_not_an_error() {
    let tmp = TempDir::new().expect("tempdir");
    // A FILE where the ledger directory should be: `create_dir_all` fails.
    let blocked = tmp.path().join("blocked");
    std::fs::write(&blocked, "not a directory").expect("write blocker");

    let landed = append_row(&blocked, &row("engineer", "anything"));
    assert!(!landed, "the row must report that it did not land");
    // The point of the assertion: no panic, no `Err` — control returned here.
}

// ── summary ─────────────────────────────────────────────────────────────────

#[test]
fn summarize_counts_by_agent_type() {
    let rows = vec![row("engineer", "a"), row("qa", "b"), row("engineer", "c")];
    let got = summarize(&rows);
    assert_eq!(got, vec![("engineer".into(), 2), ("qa".into(), 1)]);
}

#[test]
fn summarize_orders_by_count_then_name() {
    let rows = vec![row("zed", "a"), row("abe", "b")];
    // Equal counts fall back to name order, so the output is stable.
    assert_eq!(summarize(&rows), vec![("abe".into(), 1), ("zed".into(), 1)]);
}

#[test]
fn summarize_of_nothing_is_empty() {
    assert!(summarize(&[]).is_empty());
}
