//! Extraction tests for the #7735 shared closing-block parser.
//!
//! `real_closing_block_fixture_parses_both_sections` is this slice's
//! acceptance test (docs/specs/self-improvement-loop.md §3 slice 1): it
//! fails red against a stub that returns empty/`None` and passes once
//! [`extract_improvement_recommendations`]/[`extract_prompt_feedback`] are
//! implemented — see the PR report for the failure-then-pass transcript.

use super::*;

// ── a real closing block, copied verbatim from ───────────────────────────
// docs/research/engineer-transcript-token-sinks-2026-09-12.md:74-91
const REAL_CLOSING_BLOCK: &str = "\
## Improvement recommendations

- **Symptom**: T1 used Bash `sed -n`/`cat -n`/`grep` for 96 of ~135 Bash
  calls instead of Read/Grep, for in-worktree file inspection and search.
- **Cause**: no instruction steers an engineer toward Read/Grep over Bash
  text tools when both work; BASE-AGENT's divert guidance is written in
  terms of Read-tool offset/limit, so a Bash-sed agent never triggers it.
- **Change**: add a line to BASE-AGENT or rust-engineer preferring Read
  (offset/limit) and Grep over Bash `sed -n`/`cat -n`/`grep` for in-repo
  inspection — the Bash path bypasses Read-tool dedup/divert tracking and
  produced T2's genuine `exact.rs` double-read once offset/limit was in play.
- **Evidence**: T1 categories above (162,143+77,245 bytes / 96 Bash calls
  vs. 35,577 bytes / 7 Read calls); T2's `exact.rs` triple read (section 3).

## Prompt feedback

Separating genuine duplication from legitimate pagination required reading
actual event content beyond aggregate stats — a necessary second pass, not
avoidable from usage/byte totals alone. Framing the repetition ask as \"same
offset/limit twice\" vs. \"overlapping ranges\" vs. \"sequential chunks\" up front
would get the taxonomy right in one script pass instead of two.
";

#[test]
fn real_closing_block_fixture_parses_both_sections() {
    let findings = extract_improvement_recommendations(REAL_CLOSING_BLOCK);
    assert_eq!(findings.len(), 1, "one Symptom-led entry: {findings:?}");
    let f = &findings[0];
    assert!(
        f.symptom
            .as_deref()
            .unwrap_or_default()
            .starts_with("T1 used Bash")
    );
    assert!(
        f.cause
            .as_deref()
            .unwrap_or_default()
            .contains("no instruction steers")
    );
    assert!(
        f.change
            .as_deref()
            .unwrap_or_default()
            .contains("add a line to BASE-AGENT")
    );
    assert!(
        f.evidence
            .as_deref()
            .unwrap_or_default()
            .contains("T1 categories above")
    );

    let feedback = extract_prompt_feedback(REAL_CLOSING_BLOCK).expect("a Prompt feedback section");
    assert!(feedback.contains("Separating genuine duplication"));
    assert!(feedback.contains("sequential chunks"));
}

// ── shared heading behavior (mirrors #7702's prompt_feedback_tests.rs) ────

#[test]
fn absent_heading_extracts_nothing() {
    assert!(extract_prompt_feedback("just the work, no section").is_none());
    assert!(extract_improvement_recommendations("just the work, no section").is_empty());
}

#[test]
fn stops_at_the_next_heading() {
    let message = "## Prompt feedback\n\nkeep this\n\n## Something else\n\ndrop this\n";
    assert_eq!(
        extract_prompt_feedback(message).expect("a section"),
        "keep this"
    );
}

#[test]
fn prompt_feedback_last_occurrence_wins() {
    let message = "\
I was asked to end with a section.

## Prompt feedback

first

## Notes

middle

## Prompt feedback

second
";
    assert_eq!(
        extract_prompt_feedback(message).expect("a section"),
        "second"
    );
}

#[test]
fn prompt_feedback_accepts_h3() {
    let message = "### Prompt feedback\n\nkeep this\n\n### Next\n\ndrop this\n";
    assert_eq!(
        extract_prompt_feedback(message).expect("a section"),
        "keep this"
    );
}

#[test]
fn an_empty_section_extracts_nothing() {
    assert!(extract_prompt_feedback("work\n\n## Prompt feedback\n\n").is_none());
}

#[test]
fn an_inline_mention_is_not_a_section() {
    assert!(extract_prompt_feedback("I will add a ## Prompt feedback block next time.").is_none());
}

// ── structured Improvement recommendations parsing ────────────────────────

#[test]
fn multiple_entries_parse_as_separate_findings() {
    let message = "\
## Improvement recommendations

- **Symptom**: first symptom
- **Cause**: first cause
- **Change**: first change
- **Evidence**: first evidence

- **Symptom**: second symptom
- **Cause**: second cause
- **Change**: second change
- **Evidence**: second evidence
";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 2, "{findings:?}");
    assert_eq!(findings[0].symptom.as_deref(), Some("first symptom"));
    assert_eq!(findings[1].symptom.as_deref(), Some("second symptom"));
    assert_eq!(findings[1].evidence.as_deref(), Some("second evidence"));
}

#[test]
fn a_well_formed_finding_parses_all_four_fields() {
    let message = "\
## Improvement recommendations

- **Symptom**: observed thing
- **Cause**: named cause
- **Change**: concrete edit
- **Evidence**: file.rs:42
";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].symptom.as_deref(), Some("observed thing"));
    assert_eq!(findings[0].cause.as_deref(), Some("named cause"));
    assert_eq!(findings[0].change.as_deref(), Some("concrete edit"));
    assert_eq!(findings[0].evidence.as_deref(), Some("file.rs:42"));
}

#[test]
fn a_finding_missing_a_label_leaves_that_field_none() {
    let message = "## Improvement recommendations\n\n- **Symptom**: observed thing\n- **Change**: concrete edit\n";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].symptom.as_deref(), Some("observed thing"));
    assert!(findings[0].cause.is_none());
    assert_eq!(findings[0].change.as_deref(), Some("concrete edit"));
    assert!(findings[0].evidence.is_none());
}

#[test]
fn bold_labels_and_bullets_parse() {
    let message =
        "## Improvement recommendations\n\n* **Symptom** \u{2014} a dash-separated value\n";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].symptom.as_deref(),
        Some("a dash-separated value")
    );
}

#[test]
fn plain_unbold_labels_parse() {
    let message = "## Improvement recommendations\n\nSymptom: plain label, no bullet, no bold\n";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].symptom.as_deref(),
        Some("plain label, no bullet, no bold")
    );
}

#[test]
fn numbered_markers_parse() {
    let message = "## Improvement recommendations\n\n1. **Symptom**: numbered entry\n";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].symptom.as_deref(), Some("numbered entry"));
}

#[test]
fn h3_heading_for_improvement_recommendations_parses() {
    let message = "### Improvement recommendations\n\n- **Symptom**: under an h3\n";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].symptom.as_deref(), Some("under an h3"));
}

#[test]
fn continuation_lines_join_with_a_space() {
    let message = "\
## Improvement recommendations

- **Symptom**: a value that
  wraps onto a second
  physical line
";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].symptom.as_deref(),
        Some("a value that wraps onto a second physical line")
    );
}

// ── malformed-block and panic-safety tolerance ─────────────────────────────

#[test]
fn a_malformed_block_with_no_recognizable_label_is_empty() {
    let message = "## Improvement recommendations\n\njust some prose with no labels at all\n";
    assert!(extract_improvement_recommendations(message).is_empty());
}

#[test]
fn a_heading_with_an_empty_body_is_empty() {
    let message = "## Improvement recommendations\n\n\n## Prompt feedback\n\nsomething\n";
    assert!(extract_improvement_recommendations(message).is_empty());
}

#[test]
fn arbitrary_text_never_panics() {
    let inputs = [
        "",
        "#",
        "##",
        "### ",
        "####### too many hashes",
        "\u{0}\u{1}\u{2} control bytes",
        "## Improvement recommendations\u{0}\n- **Symptom**: \u{0} embedded nul",
        "- Symptom Cause Change Evidence all on one line with no separators",
        &"#".repeat(10_000),
        &"- **Symptom**: x\n".repeat(5_000),
    ];
    for input in inputs {
        let _ = extract_improvement_recommendations(input);
        let _ = extract_prompt_feedback(input);
    }
}

#[test]
fn a_trailing_incomplete_entry_with_content_is_still_returned() {
    let message =
        "## Improvement recommendations\n\n- **Symptom**: only a symptom, nothing else follows\n";
    let findings = extract_improvement_recommendations(message);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].symptom.as_deref(),
        Some("only a symptom, nothing else follows")
    );
}
