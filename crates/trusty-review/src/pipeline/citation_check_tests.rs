//! Unit tests for the citation-verification layer (#2881, extended #4042).

use super::*;
use crate::models::{Effort, Finding};
use crate::pipeline::diff_analyzer::models::{
    DroppedFile, FileDisposition, FilteredDiff, FilteredFile, FilteredHunk,
};
use std::collections::HashMap;

// ─── Fixtures ────────────────────────────────────────────────────────────────

fn hunk(header: &str, lines: &[&str]) -> FilteredHunk {
    FilteredHunk {
        header: header.to_string(),
        lines: lines.iter().map(|l| l.to_string()).collect(),
        substantive_confidence: 1.0,
        reason_kept: "test".to_string(),
    }
}

fn kept_file(name: &str, hunks: Vec<FilteredHunk>) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::Kept,
        hunks,
        dropped_hunks: Vec::new(),
        summary_line: None,
    }
}

fn filtered(files: Vec<FilteredFile>) -> FilteredDiff {
    FilteredDiff {
        files,
        dropped_files: Vec::new(),
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 0,
        filtered_byte_size: 0,
    }
}

/// The #2881 repro shape: a large bundle file + a new vitest test file.
fn repro_index() -> DiffContentIndex {
    let bundle = kept_file(
        "api/chat.js",
        vec![hunk(
            "@@ -35200,3 +35271,2 @@ function bundleFn() {",
            &[
                "+  const bundleVar = compiledThing(42);",
                "+  return bundleVar;",
            ],
        )],
    );
    let test = kept_file(
        "src/instructions-content.test.ts",
        vec![hunk(
            "@@ -0,0 +1,3 @@",
            &[
                "+import { describe, it, expect } from 'vitest';",
                "+describe('instructions', () => {",
                "+  it('contains aria prompt', () => expect(INSTRUCTIONS).toContain('aria'));",
            ],
        )],
    );
    DiffContentIndex::from_filtered(&filtered(vec![bundle, test]))
}

fn code_provable_finding(file: &str, description: &str) -> Finding {
    let mut f = Finding::new(
        file,
        "logic-error",
        description,
        "fix it",
        0.75,
        Effort::High,
    );
    f.code_provable = true;
    f
}

/// Helper: wrap a single finding into a `Vec` (kept local to avoid churn).
fn findings_of(f: Finding) -> Vec<Finding> {
    vec![f]
}

// ─── Index tests ─────────────────────────────────────────────────────────────

#[test]
fn index_from_filtered_indexes_each_file() {
    let idx = repro_index();
    assert!(idx.lookup("api/chat.js").is_some());
    assert!(idx.lookup("src/instructions-content.test.ts").is_some());
    assert!(idx.lookup("does/not/exist.rs").is_none());
}

#[test]
fn index_indexes_dropped_files_with_empty_content() {
    // #4042: a Stage-A-excluded file (lockfile, snapshot, generated code) is a
    // REAL file in the diff — its path must resolve — but no content was ever
    // shown to the model, so it is indexed with an EMPTY body.
    let mut diff = filtered(vec![]);
    diff.dropped_files.push(DroppedFile {
        path: "build/gen.js".to_string(),
        reason: "generated".to_string(),
    });
    let idx = DiffContentIndex::from_filtered(&diff);
    assert_eq!(
        idx.lookup("build/gen.js"),
        Some(""),
        "path resolves; content is empty"
    );
}

#[test]
fn lookup_matches_by_basename() {
    let idx = repro_index();
    // A finding may cite just the basename or an `a/`-prefixed path.
    assert!(idx.lookup("chat.js").is_some());
    assert!(idx.lookup("a/api/chat.js").is_some());
}

#[test]
fn lookup_ambiguous_basename_is_none() {
    let a = kept_file("pkg/a/mod.rs", vec![hunk("@@ -1 +1 @@", &["+a"])]);
    let b = kept_file("pkg/b/mod.rs", vec![hunk("@@ -1 +1 @@", &["+b"])]);
    let idx = DiffContentIndex::from_filtered(&filtered(vec![a, b]));
    assert!(
        idx.lookup("mod.rs").is_none(),
        "ambiguous basename must not guess"
    );
}

#[test]
fn contains_is_whitespace_tolerant() {
    assert_eq!(normalize("a   b\n\tc"), "a b c");
}

// ─── enforce_citation_integrity: drop behaviour ───────────────────────────────

#[test]
fn drops_cross_file_misattribution() {
    // THE #2881 REPRO: a code_provable finding cites the bundle but quotes the
    // vitest test content, which lives in a different changed file.
    let idx = repro_index();
    let mut findings = vec![code_provable_finding(
        "api/chat.js",
        "The test file content `import { describe, it, expect } from 'vitest';` was \
         prepended into the bundle around line 35271.",
    )];
    findings[0].line = Some(35271);

    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(n, 1, "the misattributed finding must be dropped");
    assert!(findings.is_empty());
}

#[test]
fn keeps_grounded_code_provable() {
    // A code_provable finding that quotes content actually present in the cited
    // file must be left untouched.
    let idx = repro_index();
    let mut findings = vec![code_provable_finding(
        "api/chat.js",
        "Suspicious call `const bundleVar = compiledThing(42);` may overflow.",
    )];
    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(n, 0, "a grounded finding must survive");
    assert_eq!(findings.len(), 1);
    assert!(findings[0].code_provable);
    assert!((findings[0].confidence - 0.75).abs() < f32::EPSILON);
}

#[test]
fn drops_citation_to_path_outside_diff() {
    // #4042: a finding citing a file that was NEVER part of the diff at all
    // (not Kept, not SummaryOnly, not even Stage-A-dropped) must be DROPPED,
    // not fail-open — this is the standalone `hotelPage.ts:207` incident shape.
    let idx = repro_index();
    let mut findings = vec![code_provable_finding(
        "src/never/changed.rs",
        "Off-by-one in the loop bound `for i in 0..=len { arr[i] }`.",
    )];
    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(
        n, 1,
        "a citation to a path outside the diff must be dropped"
    );
    assert!(findings.is_empty());
}

#[test]
fn fail_open_when_no_quote() {
    // A finding whose cited file resolves but quotes nothing concrete cannot be
    // verified further — leave it alone (fail open) rather than risk a
    // false-positive drop.
    let idx = repro_index();
    let mut findings = vec![code_provable_finding(
        "api/chat.js",
        "There may be a subtle logic issue in this function.",
    )];
    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(n, 0, "no verifiable quote → fail open");
    assert_eq!(findings.len(), 1);
}

#[test]
fn every_finding_is_checked_not_just_code_provable() {
    // #4042's NAMED coverage gap: a finding that is neither `code_provable` nor
    // `code:`-cited must STILL have its `file` path verified against the diff —
    // the #2882 fix scoped verification to the code_provable subset, which is
    // exactly the gap #4042 exploited.
    let idx = repro_index();
    let mut f = Finding::new(
        "src/never/changed.rs",
        "style",
        "Minor formatting nit.",
        "n/a",
        0.9,
        Effort::Low,
    );
    f.source_citation = Some("jira:PROJ-1".to_string());
    let mut findings = vec![f];
    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(
        n, 1,
        "path resolution now applies to every finding, not only code_provable ones"
    );
}

#[test]
fn unknown_file_sentinel_is_exempt_from_path_check() {
    // A genuinely general, file-less finding (the LLM omitted `file`, parsed as
    // the `UNKNOWN_FILE_PLACEHOLDER` sentinel) must never be dropped for "not
    // resolving" a path it never claimed.
    let idx = repro_index();
    let mut findings = vec![Finding::new(
        crate::models::UNKNOWN_FILE_PLACEHOLDER,
        "style",
        "Overall the PR could use a changelog entry.",
        "n/a",
        0.6,
        Effort::Low,
    )];
    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(
        n, 0,
        "the unknown-file sentinel is exempt from path checking"
    );
    assert_eq!(findings.len(), 1);
}

#[test]
fn code_citation_bracket_content_is_now_verified() {
    // #4042's OTHER named gap: the mandated `[code: …]` citation's own excerpt
    // was previously EXEMPTED from verification entirely. A finding whose ONLY
    // "evidence" lives inside that bracket, and which is fabricated, must now
    // be dropped.
    let idx = repro_index();
    let mut f = code_provable_finding(
        "api/chat.js",
        "The bundle embeds the test suite \
         [code: `api/chat.js:1` — \"import { describe, it, expect } from 'vitest';\"].",
    );
    f.line = Some(1);
    let n = enforce_citation_integrity(&mut findings_of_mut(&mut f), &idx);
    assert_eq!(
        n, 1,
        "a fabricated bracket-citation excerpt must now be caught"
    );
}

/// Helper: wrap a `&mut Finding` clone into an owned `Vec` for the drop-based API.
fn findings_of_mut(f: &mut Finding) -> Vec<Finding> {
    vec![f.clone()]
}

#[test]
fn code_citation_bracket_grounded_content_survives() {
    // A correctly-grounded `[code: …]` excerpt (matches the real cited content)
    // must NOT be dropped — the new bracket-verification must not over-fire.
    let idx = repro_index();
    let mut f = code_provable_finding(
        "api/chat.js",
        "Suspicious overflow risk \
         [code: `api/chat.js:35271` — \"const bundleVar = compiledThing(42);\"].",
    );
    f.line = Some(35271);
    let mut findings = findings_of(f);
    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(n, 0, "a grounded bracket citation must survive");
    assert_eq!(findings.len(), 1);
}

#[test]
fn code_citation_bracket_path_outside_diff_is_dropped() {
    // A `[code: …]` citation whose OWN path is not part of the diff at all
    // (independent of `f.file`) must be dropped — the `hotelPage.ts:207` shape
    // expressed via the bracket grammar instead of `f.file`.
    let idx = repro_index();
    let mut f = code_provable_finding(
        "api/chat.js",
        "Race condition in the loser branch \
         [code: `hotelPage.ts:207` — \"await Promise.race([a, b])\"].",
    );
    let n = enforce_citation_integrity(&mut findings_of_mut(&mut f), &idx);
    assert_eq!(
        n, 1,
        "an unresolvable bracket citation path must be dropped"
    );
}

// ─── Mutual contradiction ──────────────────────────────────────────────────────

#[test]
fn drops_findings_with_line_contradiction() {
    // #4042 centerpiece shape: two findings cite the SAME file+line via the
    // `[code: …]` bracket grammar but quote mutually exclusive content — at
    // least one must be invented, so both are dropped.
    let idx = repro_index();
    let mut findings = vec![
        code_provable_finding(
            "api/chat.js",
            "This is a Drizzle JSON snapshot \
             [code: `api/chat.js:1` — \"id: 893644d4-fabricated-snapshot-content\"].",
        ),
        code_provable_finding(
            "api/chat.js",
            "This is a Vitest suite for draft-form \
             [code: `api/chat.js:1` — \"import { describe, expect, it } from 'vitest'\"].",
        ),
    ];
    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(n, 2, "both contradictory findings must be dropped");
    assert!(findings.is_empty());
}

#[test]
fn distinct_lines_are_never_contradictory() {
    // Two REAL, distinct findings about DIFFERENT lines of the same file are
    // normal — must never be flagged as contradictory.
    let idx = repro_index();
    let mut findings = vec![
        code_provable_finding(
            "api/chat.js",
            "Overflow risk \
             [code: `api/chat.js:35271` — \"const bundleVar = compiledThing(42);\"].",
        ),
        code_provable_finding(
            "api/chat.js",
            "Missing null check \
             [code: `api/chat.js:35272` — \"return bundleVar;\"].",
        ),
    ];
    let n = enforce_citation_integrity(&mut findings, &idx);
    assert_eq!(
        n, 0,
        "distinct real lines in the same file must not be treated as contradictory"
    );
    assert_eq!(findings.len(), 2);
}

// ─── Helper unit tests ───────────────────────────────────────────────────────

#[test]
fn split_locator_extracts_line() {
    assert_eq!(
        split_locator("src/lib/foo.ts:42"),
        ("src/lib/foo.ts".to_string(), Some(42))
    );
}

#[test]
fn split_locator_no_line() {
    assert_eq!(
        split_locator("docs/specs/foo.md"),
        ("docs/specs/foo.md".to_string(), None)
    );
}

#[test]
fn code_citation_re_captures_path_line_and_excerpt() {
    let f = Finding::new(
        "f",
        "k",
        "See [code: `src/big/File.ts:4242` — \"a verbatim excerpt right here\"].",
        "s",
        0.5,
        Effort::Low,
    );
    let extracted = extract_citations(&f);
    assert_eq!(extracted.code_citations.len(), 1);
    assert_eq!(extracted.code_citations[0].path, "src/big/File.ts");
    assert_eq!(extracted.code_citations[0].line, Some(4242));
    assert_eq!(
        extracted.code_citations[0].excerpt,
        "a verbatim excerpt right here"
    );
}

#[test]
fn extract_spans_skips_prompt_mandated_citation_grammar() {
    // The non-`code:` bracket forms are stripped before generic quote
    // extraction — `jira`/`gh`/`apex` citations are out of this module's scope.
    let f = Finding::new(
        "f",
        "k",
        "See [jira: PROJ-9 — \"ticket paraphrase text goes here\"].",
        "s",
        0.5,
        Effort::Low,
    );
    let extracted = extract_citations(&f);
    assert!(
        extracted.generic.is_empty(),
        "non-code bracket contents must not be treated as generic quotes: {:?}",
        extracted.generic
    );
    assert!(extracted.code_citations.is_empty());
}

#[test]
fn extract_spans_pulls_backtick_and_quotes() {
    let mut f = Finding::new(
        "f",
        "k",
        "backtick `let x = compute(y);` and double \"another_long_snippet()\".",
        "s",
        0.5,
        Effort::Low,
    );
    f.consequence = "single 'yet_another_fragment(z)' here".to_string();
    let extracted = extract_citations(&f);
    assert!(
        extracted
            .generic
            .iter()
            .any(|s| s.contains("let x = compute(y);"))
    );
    assert!(
        extracted
            .generic
            .iter()
            .any(|s| s.contains("another_long_snippet()"))
    );
    assert!(
        extracted
            .generic
            .iter()
            .any(|s| s.contains("yet_another_fragment(z)"))
    );
}

#[test]
fn extract_spans_skips_short() {
    let f = Finding::new("f", "k", "tiny `x` and `ab`", "s", 0.5, Effort::Low);
    assert!(
        extract_citations(&f).generic.is_empty(),
        "short quotes are not evidence"
    );
}
