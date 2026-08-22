//! Tests for the verifiable-evidence guardrail (#2357).
//!
//! Why: this guardrail is why a repo-evidence report can be trusted — it must
//! accept faithful quotes (correcting the line), reject fabricated evidence and
//! phantom files, tolerate whitespace differences, and pass greens as bare topics.
//! What: exercises accept/line-correction, missing-file reject, fabricated-quote
//! reject, whitespace-insensitive match, and the green title-only path.
//! Test: included as `#[cfg(test)] mod tests` from `verify.rs`.

use super::*;
use crate::report::investigate::select::{SelectedFile, Selection};

fn selection(path: &str, content: &str) -> Selection {
    Selection {
        files: vec![SelectedFile {
            path: path.to_string(),
            content: content.to_string(),
            truncated: false,
            dimensions: vec![],
            selected_by: None,
            hotspot: None,
        }],
        total_files: 1,
        skipped: 0,
        bytes_sent: content.len(),
        dimensions_covered: vec![],
        dimensions_absent: vec![],
        per_dimension: vec![],
        attributed_files: 0,
        attributed_only: false,
    }
}

fn raw(severity: &str, file: &str, quote: &str, line: Option<u64>) -> RawFinding {
    RawFinding {
        title: "Finding".to_string(),
        severity: severity.to_string(),
        dimension: "authentication & secrets".to_string(),
        file: file.to_string(),
        line,
        evidence_quote: quote.to_string(),
        description: "desc".to_string(),
        business_impact: "impact".to_string(),
        remediation: "fix".to_string(),
        cost_effort: "low".to_string(),
    }
}

/// Why: a faithful quote is accepted and its line corrected from the real match.
/// What: the quote is on line 3; the LLM claimed line 99 — verification fixes it.
/// Test: this test itself.
#[test]
fn accepts_and_corrects_line() {
    let content = "line one\nline two\nlet token = read_secret();\nline four\n";
    let sel = selection("auth.rs", content);
    let out = verify_findings(
        vec![raw(
            "red",
            "auth.rs",
            "let token = read_secret();",
            Some(99),
        )],
        &sel,
    );
    assert_eq!(out.verified.len(), 1);
    assert_eq!(out.rejected, 0);
    assert_eq!(
        out.verified[0].line,
        Some(3),
        "line corrected from the match"
    );
    assert_eq!(out.verified[0].severity, Severity::Red);
}

/// Why: a finding citing a file that was not inspected must be rejected.
/// What: cites `ghost.rs`; asserts rejection with a note.
/// Test: this test itself.
#[test]
fn rejects_missing_file() {
    let sel = selection("auth.rs", "let x = 1;\n");
    let out = verify_findings(vec![raw("amber", "ghost.rs", "let x = 1;", None)], &sel);
    assert!(out.verified.is_empty());
    assert_eq!(out.rejected, 1);
    assert!(out.notes[0].contains("not in the inspected set"));
}

/// Why: fabricated evidence is the core threat and must be rejected.
/// What: the quote does not appear in the file; asserts rejection.
/// Test: this test itself.
#[test]
fn rejects_fabricated_quote() {
    let sel = selection("auth.rs", "let token = read_secret();\n");
    let out = verify_findings(
        vec![raw(
            "red",
            "auth.rs",
            "eval(userInput) // never in file",
            None,
        )],
        &sel,
    );
    assert!(out.verified.is_empty());
    assert_eq!(out.rejected, 1);
    assert!(out.notes[0].contains("evidence quote not found"));
}

/// Why: LLMs reflow whitespace; a quote differing only in whitespace must match.
/// What: file has tabs/newlines the quote lacks; asserts acceptance + line.
/// Test: this test itself.
#[test]
fn matches_whitespace_insensitively() {
    let content = "fn main() {\n\tlet secret =\n\t\tget_env(\"KEY\");\n}\n";
    let sel = selection("main.rs", content);
    let out = verify_findings(
        vec![raw(
            "amber",
            "main.rs",
            "let secret = get_env(\"KEY\");",
            None,
        )],
        &sel,
    );
    assert_eq!(out.verified.len(), 1, "whitespace-only diff must match");
    assert_eq!(out.verified[0].line, Some(2));
}

/// Why: greens are title-only topics; they need no evidence and never reject.
/// What: a green with no file/quote is verified as a bare topic.
/// Test: this test itself.
#[test]
fn green_is_title_only() {
    let sel = selection("a.rs", "code\n");
    let mut g = raw("green", "", "", None);
    g.title = "Clean dependency tree".to_string();
    let out = verify_findings(vec![g], &sel);
    assert_eq!(out.verified.len(), 1);
    assert_eq!(out.rejected, 0);
    assert_eq!(out.verified[0].severity, Severity::Green);
    assert!(out.verified[0].evidence_quote.is_empty());
    assert!(out.verified[0].file.is_empty());
}

/// Why: a finding with no title is meaningless and must be rejected.
/// What: an empty-title finding is dropped.
/// Test: this test itself.
#[test]
fn rejects_empty_title() {
    let sel = selection("a.rs", "code\n");
    let mut r = raw("red", "a.rs", "code", None);
    r.title = "   ".to_string();
    let out = verify_findings(vec![r], &sel);
    assert!(out.verified.is_empty());
    assert_eq!(out.rejected, 1);
}

/// #6137: the live defect. An installer's security note was quoted through
/// "…before running it." and the very next clause, on the SAME line, was cut:
/// "All downloaded binaries are SHA-256 verified." The finding then read as
/// unmitigated remote code execution.
#[test]
fn quote_is_completed_to_the_end_of_its_line() {
    let content = "# require a higher assurance level, download and review the script manually\n\
                   # before running it. All downloaded binaries are SHA-256 verified.\n\
                   set -e\n";
    let sel = selection("install.sh", content);
    let r = raw("amber", "install.sh", "# before running it.", None);
    let out = verify_findings(vec![r], &sel);

    assert_eq!(out.rejected, 0);
    assert!(
        out.verified[0]
            .evidence_quote
            .contains("All downloaded binaries are SHA-256 verified."),
        "the mitigation on the same line must survive: {:?}",
        out.verified[0].evidence_quote
    );
    assert!(
        !out.verified[0].evidence_quote.contains("set -e"),
        "completion stops at the line end, never runs on"
    );
}

/// The extension is bounded: a minified or generated single line must not drag
/// its whole content into the quote.
#[test]
fn quote_is_not_extended_across_a_very_long_line() {
    let tail = "x".repeat(400);
    let content = format!("let a = 1; {tail}\n");
    let sel = selection("a.js", &content);
    let r = raw("amber", "a.js", "let a = 1;", None);
    let out = verify_findings(vec![r], &sel);

    assert_eq!(out.verified[0].evidence_quote, "let a = 1;");
}
