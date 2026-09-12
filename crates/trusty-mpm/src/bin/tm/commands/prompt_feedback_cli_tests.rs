//! Rendering tests for `tm prompt-feedback` (#7688).

use super::*;

fn row(agent: &str, ts: &str, feedback: &str) -> FeedbackRow {
    FeedbackRow {
        ts: ts.to_string(),
        session_id: Some("sess-1".to_string()),
        agent_type: agent.to_string(),
        prompt_digest: Some("0123456789abcdef0123".to_string()),
        feedback: feedback.to_string(),
    }
}

#[test]
fn renders_a_note_when_nothing_is_captured() {
    assert_eq!(render(&[], false), "no prompt feedback captured yet\n");
    assert_eq!(render(&[], true), "no prompt feedback captured yet\n");
}

#[test]
fn renders_rows_newest_first() {
    // `read_rows` already ordered them; the renderer must not reorder.
    let rows = vec![
        row("engineer", "2026-09-12T11:00:00Z", "second"),
        row("qa", "2026-09-12T10:00:00Z", "first"),
    ];
    let out = render(&rows, false);
    let second = out.find("second").expect("second present");
    let first = out.find("first").expect("first present");
    assert!(second < first, "the newest row must render first");
    assert!(out.contains("session=sess-1"));
}

/// The digest is elided to 12 chars so a row stays on one line.
#[test]
fn renders_a_short_prompt_digest() {
    let out = render(&[row("engineer", "2026-09-12T10:00:00Z", "x")], false);
    assert!(out.contains("prompt=0123456789ab"), "got {out}");
    assert!(!out.contains("0123456789abcdef0123"));
}

/// A row with no digest renders a placeholder, never an empty column.
#[test]
fn renders_a_placeholder_for_a_missing_digest() {
    let mut r = row("engineer", "2026-09-12T10:00:00Z", "x");
    r.prompt_digest = None;
    r.session_id = None;
    let out = render(&[r], false);
    assert!(out.contains("prompt=-"), "got {out}");
    assert!(out.contains("session=-"), "got {out}");
}

#[test]
fn renders_a_summary() {
    let rows = vec![
        row("engineer", "2026-09-12T10:00:00Z", "a"),
        row("engineer", "2026-09-12T11:00:00Z", "b"),
        row("qa", "2026-09-12T12:00:00Z", "c"),
    ];
    let out = render(&rows, true);
    assert!(out.contains("    2  engineer"), "got {out}");
    assert!(out.contains("    1  qa"), "got {out}");
    assert!(out.contains("    3  total"), "got {out}");
    // The summary counts; it does not print the bodies.
    assert!(!out.contains('a'.to_string().as_str()) || !out.contains("    a"));
}

/// The feedback body is indented so a multi-line critique reads as one block.
#[test]
fn indents_a_multi_line_body() {
    let out = render(
        &[row(
            "engineer",
            "2026-09-12T10:00:00Z",
            "line one\nline two",
        )],
        false,
    );
    assert!(out.contains("    line one\n"), "got {out}");
    assert!(out.contains("    line two\n"), "got {out}");
}

// ── The clap surface ────────────────────────────────────────────────────────

/// Why: the three filters and `--summary` are the whole operator surface, and a
/// clap-level regression (a renamed flag, a lost `Option`) is invisible to the
/// rendering tests above — they construct `FeedbackRow`s directly and never go
/// through the parser.
/// What: the defaults with no flags, then every filter supplied at once.
#[test]
fn cli_parses_prompt_feedback() {
    use clap::Parser;

    let cli = crate::cli::Cli::try_parse_from(["trusty-mpm", "prompt-feedback"]).unwrap();
    match cli.command.unwrap() {
        crate::cli::Command::PromptFeedback(args) => {
            assert_eq!(args.session, None);
            assert_eq!(args.agent, None);
            assert_eq!(args.limit, 20, "the documented default");
            assert!(!args.summary);
        }
        other => panic!("expected PromptFeedback, got {other:?}"),
    }

    let cli = crate::cli::Cli::try_parse_from([
        "trusty-mpm",
        "prompt-feedback",
        "--session",
        "sess-1",
        "--agent",
        "rust-engineer",
        "--limit",
        "3",
    ])
    .unwrap();
    match cli.command.unwrap() {
        crate::cli::Command::PromptFeedback(args) => {
            assert_eq!(args.session.as_deref(), Some("sess-1"));
            assert_eq!(args.agent.as_deref(), Some("rust-engineer"));
            assert_eq!(args.limit, 3);
        }
        other => panic!("expected PromptFeedback, got {other:?}"),
    }
}

/// `--summary` changes the RENDERING, not the selection, so it must compose
/// with a filter rather than conflict with one.
#[test]
fn cli_parses_prompt_feedback_summary() {
    use clap::Parser;

    let cli = crate::cli::Cli::try_parse_from([
        "trusty-mpm",
        "prompt-feedback",
        "--summary",
        "--agent",
        "qa",
    ])
    .unwrap();
    match cli.command.unwrap() {
        crate::cli::Command::PromptFeedback(args) => {
            assert!(args.summary);
            assert_eq!(args.agent.as_deref(), Some("qa"));
        }
        other => panic!("expected PromptFeedback, got {other:?}"),
    }
}
