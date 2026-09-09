//! Unit tests for [`super`] — the per-commit stats footer (#7074).

use super::*;

const FOOTER: &str = "🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools";

fn full_stats() -> CommitStats {
    CommitStats {
        tokens_in: Some(1_234_567),
        tokens_out: Some(8_901),
        savings_percent: Some(42),
        model_id: Some("claude-opus-4-1-20250805".to_string()),
    }
}

fn message_with_footer() -> String {
    format!(
        "feat(trusty-mpm): a thing (Refs #7074)\n\nWhat changed and why.\n\n{FOOTER}\nClaude-Session: https://claude.ai/code/session_01\n"
    )
}

/// Why (#7074): an unknown value must be omitted, never rendered as `0` — a
/// stated zero is a measurement that was never made, the same rule the `💸`
/// segment already follows.
/// Test: itself.
#[test]
fn render_omits_absent_fields() {
    let stats = CommitStats {
        tokens_in: Some(10),
        tokens_out: None,
        savings_percent: Some(7),
        model_id: None,
    };
    assert_eq!(
        render_trailers(&stats).as_deref(),
        Some("Tokens-In: 10\nSavings: 7%")
    );
}

/// Why: with nothing known there is no footer to write, and the caller must be
/// able to tell that from an empty string it would otherwise append.
/// Test: itself.
#[test]
fn render_is_none_when_nothing_is_known() {
    assert_eq!(render_trailers(&CommitStats::default()), None);
    assert!(CommitStats::default().is_empty());
}

/// Why: a model id carrying a newline would split the trailer block into two
/// paragraphs and silently drop every key above it.
/// Test: itself.
#[test]
fn render_rejects_a_multiline_model_id() {
    let stats = CommitStats {
        model_id: Some("claude-opus\nSavings: 99%".to_string()),
        ..CommitStats::default()
    };
    assert_eq!(render_trailers(&stats), None);
}

/// Why (#7074, acceptance criterion c): the whole point of the trailer shape is
/// that git can read it back. Verified against the real `git
/// interpret-trailers`, because git's block-detection rule — the last
/// paragraph, whose FIRST line must itself be a trailer — is exactly what a
/// hand-rolled assertion about the rendered string would miss: appending these
/// keys to the existing footer paragraph parses NOTHING.
/// Test: itself.
#[test]
fn git_interpret_trailers_parses_the_appended_block() {
    let trailers = render_trailers(&full_stats()).expect("trailers");
    let message = append_trailers(&message_with_footer(), &trailers);

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("COMMIT_EDITMSG");
    std::fs::write(&path, &message).expect("write message");

    let out = std::process::Command::new("git")
        .args(["interpret-trailers", "--parse"])
        .arg(&path)
        .output()
        .expect("run git interpret-trailers");
    assert!(out.status.success(), "git interpret-trailers failed");
    let parsed = String::from_utf8_lossy(&out.stdout);

    for expected in [
        "Tokens-In: 1234567",
        "Tokens-Out: 8901",
        "Savings: 42%",
        "Model: claude-opus-4-1-20250805",
    ] {
        assert!(
            parsed.lines().any(|line| line == expected),
            "git did not parse `{expected}` out of:\n{message}\n--- parsed ---\n{parsed}"
        );
    }
}

/// Why: `git commit` hands the hook a file whose tail is `#`-prefixed help
/// text. A block appended below it reads as part of that comment run and never
/// becomes a trailer.
/// Test: itself.
#[test]
fn append_goes_above_a_trailing_comment_block() {
    let message = format!(
        "feat: x\n\n{FOOTER}\n\n# Please enter the commit message for your changes.\n# On branch main\n"
    );
    let out = append_trailers(&message, "Tokens-In: 5");
    let lines: Vec<&str> = out.lines().collect();
    let trailer_at = lines
        .iter()
        .position(|l| *l == "Tokens-In: 5")
        .expect("trailer present");
    let comment_at = lines
        .iter()
        .position(|l| l.starts_with("# Please enter"))
        .expect("comment present");
    assert!(
        trailer_at < comment_at,
        "the trailer must sit above the comment block:\n{out}"
    );
    assert_eq!(lines[trailer_at - 1], "", "one blank line above the block");
}

/// Why: `git commit --verbose` appends a diff below a scissors line and
/// discards everything under it, so a trailer written there never reaches the
/// commit object.
/// Test: itself.
#[test]
fn append_stays_above_the_scissors_line() {
    let message = format!(
        "feat: x\n\n{FOOTER}\n\n{SCISSORS}\n# Do not modify or remove the line above.\ndiff --git a/x b/x\n+Tokens-In: 999\n"
    );
    let out = append_trailers(&message, "Tokens-In: 5");
    let lines: Vec<&str> = out.lines().collect();
    let trailer_at = lines
        .iter()
        .position(|l| *l == "Tokens-In: 5")
        .expect("trailer present");
    let scissors_at = lines
        .iter()
        .position(|l| *l == SCISSORS)
        .expect("scissors present");
    assert!(
        trailer_at < scissors_at,
        "trailer below the scissors:\n{out}"
    );
}

/// Why: `git commit --amend` re-runs `prepare-commit-msg` over a message the
/// hook already stamped; a second block would be the one git parses, and the
/// footer would grow on every amend.
/// Test: itself.
#[test]
fn append_is_idempotent() {
    let trailers = render_trailers(&full_stats()).expect("trailers");
    let once = append_trailers(&message_with_footer(), &trailers);
    let twice = append_trailers(&once, &trailers);
    assert_eq!(once, twice);
    assert_eq!(
        twice.matches("Tokens-In:").count(),
        1,
        "exactly one stats block"
    );
}

/// Why: the footer the harness writes is preserved verbatim — this feature adds
/// a paragraph, it does not rewrite the attribution line.
/// Test: itself.
#[test]
fn append_preserves_the_attribution_footer() {
    let trailers = render_trailers(&full_stats()).expect("trailers");
    let out = append_trailers(&message_with_footer(), &trailers);
    assert!(out.contains(FOOTER), "attribution footer lost:\n{out}");
    assert!(
        out.contains("Claude-Session: https://claude.ai/code/session_01"),
        "session link lost:\n{out}"
    );
}
