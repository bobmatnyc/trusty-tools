//! Unit tests for the citation-verification layer (#2881, extended #4042 and
//! #4999).

use super::*;
use crate::models::{Effort, Finding};
use crate::pipeline::diff_analyzer::models::{
    DroppedFile, DroppedHunk, FileDisposition, FilteredDiff, FilteredFile, FilteredHunk,
    HunkDropReason,
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

/// #9188 H: an old path (say, before a rename) never resolves to a different
/// file that only shares its basename; a whole-segment suffix still resolves.
#[test]
fn an_old_path_never_resolves_to_a_different_file_by_basename() {
    let renamed = kept_file("src/new/billing.rs", vec![hunk("@@ -1 +1 @@", &["+a"])]);
    let idx = DiffContentIndex::from_filtered(&filtered(vec![renamed]));
    assert!(
        idx.lookup("src/old/billing.rs").is_none(),
        "a different directory is a different file"
    );
    assert!(idx.lookup("new/billing.rs").is_some());
    assert!(idx.lookup("billing.rs").is_some());
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

    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
    assert_eq!(
        n, 1,
        "a citation to a path outside the diff must be dropped"
    );
    assert!(findings.is_empty());
}

#[test]
fn a_dropped_citation_is_recorded_as_withheld() {
    // #4044 (owner ruling on #8905, 2026-09-30): a #4042 drop reaches the
    // review record with a reason naming the gate and the cause.
    let idx = repro_index();
    let mut findings = vec![code_provable_finding(
        "src/never/changed.rs",
        "Off-by-one in the loop bound `for i in 0..=len { arr[i] }`.",
    )];
    let mut withheld = Vec::new();
    enforce_citation_integrity(&mut findings, &idx, &mut withheld);
    assert_eq!(withheld.len(), 1);
    assert_eq!(
        withheld[0].reason,
        format!("{CITATION_REASON}: cited file is not part of the diff at all")
    );
    assert_eq!(withheld[0].finding.file, "src/never/changed.rs");
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
    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings_of_mut(&mut f), &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings_of_mut(&mut f), &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
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
    let n = enforce_citation_integrity(&mut findings, &idx, &mut Vec::new());
    assert_eq!(
        n, 0,
        "distinct real lines in the same file must not be treated as contradictory"
    );
    assert_eq!(findings.len(), 2);
}

// ─── Helper unit tests ───────────────────────────────────────────────────────

/// The pre-#9188 view of [`locator_span`]: path and start line.
fn split_locator(locator: &str) -> (String, Option<u32>) {
    let (path, span) = locator_span(locator);
    (path, span.map(|(start, _)| start))
}

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
    // extraction — `jira`/`gh`/`confluence` citations are out of this module's scope.
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

/// REGRESSION (#5022): a `[confluence: …]` citation is stripped like the other
/// bracket forms, so its excerpt never reaches the generic quote scan.
///
/// Why: `duettoresearch/code-intelligence` emits this fifth form
/// (`pr_review_service.py:377,909`). `BRACKET_CITATION_RE` matched only four,
/// so the excerpt inside the bracket survived the strip and was scanned as an
/// ungrounded free-text code quote — tripping the #4042 fabrication check on
/// prose the model cited correctly.
/// What: a finding whose description carries only a `[confluence: …]` citation
/// must extract no generic spans and no code citations.
/// Test: this test. Fails pre-fix on the generic assertion — both the page
/// title and the excerpt were extracted as quotes.
#[test]
fn extract_spans_skips_confluence_citation_form() {
    let f = Finding::new(
        "f",
        "k",
        "See [confluence: \"Rate Plan Ingestion Design\" — \"the loader retries \
         each shard twice\"].",
        "s",
        0.5,
        Effort::Low,
    );
    let extracted = extract_citations(&f);
    assert!(
        extracted.generic.is_empty(),
        "a confluence citation's contents must not be treated as generic quotes: {:?}",
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

// ─── Line-span check (#4999) ─────────────────────────────────────────────────

#[test]
fn hunk_max_line_reads_both_sides() {
    assert_eq!(hunk_max_line("@@ -1,3 +1,9 @@"), Some(9));
    assert_eq!(hunk_max_line("@@ -40,12 +2,2 @@ fn outer()"), Some(51));
    // A pure-addition header: the old side contributes only its start (0).
    assert_eq!(hunk_max_line("@@ -0,0 +1,4 @@"), Some(4));
}

#[test]
fn hunk_max_line_defaults_count_to_one() {
    assert_eq!(hunk_max_line("@@ -7 +7 @@"), Some(7));
}

#[test]
fn hunk_max_line_rejects_a_malformed_header() {
    assert_eq!(hunk_max_line("not a hunk header"), None);
    assert_eq!(hunk_max_line("@@ -x,y +z,w @@"), None);
}

#[test]
fn line_beyond_last_diffed_line_is_unresolvable() {
    let index = DiffContentIndex::from_filtered(&filtered(vec![kept_file(
        "src/index.ts",
        vec![hunk("@@ -1,3 +1,9 @@", &["+const REGISTRY = new Map();"])],
    )]));

    let mut f = Finding::new("src/index.ts", "logic-error", "d", "s", 0.9, Effort::High);
    f.line = Some(412);
    assert_eq!(
        unresolvable_reason(&f, &index),
        Some("cited line is beyond the file's last diffed line")
    );
}

#[test]
fn line_inside_the_diffed_span_is_kept() {
    let index = DiffContentIndex::from_filtered(&filtered(vec![kept_file(
        "src/index.ts",
        vec![hunk("@@ -1,3 +1,9 @@", &["+const REGISTRY = new Map();"])],
    )]));

    let mut f = Finding::new("src/index.ts", "logic-error", "d", "s", 0.9, Effort::High);
    f.line = Some(9);
    assert_eq!(
        unresolvable_reason(&f, &index),
        None,
        "the last diffed line is in span"
    );
}

#[test]
fn a_bracket_citation_line_is_checked_without_an_excerpt() {
    let index = DiffContentIndex::from_filtered(&filtered(vec![kept_file(
        "src/index.ts",
        vec![hunk("@@ -1,3 +1,9 @@", &["+const REGISTRY = new Map();"])],
    )]));

    let f = Finding::new(
        "src/index.ts",
        "logic-error",
        "Story scaffolding replaced the barrel [code: `src/index.ts:412` — see the export block].",
        "s",
        0.9,
        Effort::High,
    );
    assert_eq!(
        unresolvable_reason(&f, &index),
        Some("[code: …] citation's line is beyond the file's last diffed line")
    );
}

#[test]
fn a_file_with_no_parsable_hunk_header_fails_open() {
    let mut file = kept_file("src/index.ts", Vec::new());
    file.disposition = FileDisposition::SummaryOnly;
    file.summary_line = Some("barrel updated".to_string());
    let index = DiffContentIndex::from_filtered(&filtered(vec![file]));

    let mut f = Finding::new("src/index.ts", "logic-error", "d", "s", 0.9, Effort::High);
    f.line = Some(412);
    assert_eq!(
        unresolvable_reason(&f, &index),
        None,
        "no span is known, so the line cannot be disproved"
    );
}

#[test]
fn a_dropped_hunk_widens_the_span_rather_than_narrowing_it() {
    let mut file = kept_file(
        "src/index.ts",
        vec![hunk("@@ -1,3 +1,9 @@", &["+const REGISTRY = new Map();"])],
    );
    file.dropped_hunks.push(DroppedHunk {
        reason: HunkDropReason::ImportOnly,
        lines_count: 2,
        header: "@@ -400,4 +400,6 @@".to_string(),
    });
    let index = DiffContentIndex::from_filtered(&filtered(vec![file]));

    let mut f = Finding::new("src/index.ts", "logic-error", "d", "s", 0.9, Effort::High);
    f.line = Some(405);
    assert_eq!(
        unresolvable_reason(&f, &index),
        None,
        "a line inside a Stage-B-dropped hunk is still a real line of the diff"
    );
}

// ─── Ranged and anchor locators (#9188) ──────────────────────────────────────

/// `src/a.rs` diffed over lines 1-20, quoting one real line.
fn ranged_index() -> DiffContentIndex {
    DiffContentIndex::from_filtered(&filtered(vec![kept_file(
        "src/a.rs",
        vec![hunk(
            "@@ -1,3 +1,20 @@",
            &["+let total = amounts.iter().sum::<u64>();"],
        )],
    )]))
}

/// A finding on `src/a.rs` whose only evidence is one `[code: …]` bracket.
fn bracket_finding(bracket: &str) -> Finding {
    Finding::new(
        "src/a.rs",
        "logic-error",
        format!("The sum can overflow {bracket}."),
        "s",
        0.9,
        Effort::High,
    )
}

#[test]
fn a_ranged_citation_with_a_verbatim_excerpt_is_kept() {
    for locator in [
        "src/a.rs:10-20",
        "src/a.rs:L10",
        "src/a.rs:L10-L20",
        "src/a.rs#L10-L20",
    ] {
        let mut findings = vec![bracket_finding(&format!(
            "[code: `{locator}` — \"let total = amounts.iter().sum::<u64>();\"]"
        ))];
        let mut withheld = Vec::new();
        enforce_citation_integrity(&mut findings, &ranged_index(), &mut withheld);
        assert!(
            withheld.is_empty(),
            "{locator}: a true ranged citation was withheld: {:?}",
            withheld.iter().map(|w| &w.reason).collect::<Vec<_>>()
        );
        assert_eq!(findings.len(), 1, "{locator}");
    }
}

#[test]
fn a_ranged_citation_starting_past_the_diff_is_withheld() {
    for locator in [
        "src/a.rs:30-40",
        "src/a.rs:L30",
        "src/a.rs:L30-L40",
        "src/a.rs#L30-L40",
    ] {
        let mut findings = vec![bracket_finding(&format!(
            "[code: `{locator}` — see the loop]"
        ))];
        let mut withheld = Vec::new();
        enforce_citation_integrity(&mut findings, &ranged_index(), &mut withheld);
        assert!(
            findings.is_empty(),
            "{locator}: a fabricated range was kept"
        );
        assert_eq!(
            withheld[0].reason,
            format!(
                "{CITATION_REASON}: [code: …] citation's line is beyond the file's last diffed line"
            ),
            "{locator}"
        );
    }
}

#[test]
fn locator_span_reads_ranges_anchors_and_columns() {
    for (locator, span) in [
        ("a.rs:10-20", (10, 20)),
        ("a.rs:L10", (10, 10)),
        ("a.rs:L10-L20", (10, 20)),
        ("a.rs#L10-L20", (10, 20)),
        ("a.rs:10:5", (10, 10)),
        ("a.rs:12", (12, 12)),
        ("a.rs:20-10", (20, 20)),
    ] {
        assert_eq!(
            locator_span(locator),
            ("a.rs".to_string(), Some(span)),
            "{locator}"
        );
    }
    assert_eq!(
        locator_span(r"C:\x.rs:5"),
        (r"C:\x.rs".to_string(), Some((5, 5)))
    );
}

#[test]
fn locator_span_leaves_lineless_and_malformed_locators_whole() {
    for locator in [
        "a.rs",
        "docs/foo.md:section",
        "docs/foo.md#section",
        r"C:\x.rs",
        "a.rs:12-x",
        "a.rs:99999999999",
        "a.rs:1-99999999999",
    ] {
        assert_eq!(
            locator_span(locator),
            (locator.to_string(), None),
            "{locator}"
        );
    }
}

#[test]
fn a_ranged_citation_never_contradicts_a_line_inside_it() {
    let index = DiffContentIndex::from_filtered(&filtered(vec![kept_file(
        "src/a.rs",
        vec![hunk(
            "@@ -1,3 +1,20 @@",
            &[
                "+let total = amounts.iter().sum::<u64>();",
                "+let average = total / amounts.len() as u64;",
            ],
        )],
    )]));
    let mut findings = vec![
        bracket_finding("[code: `src/a.rs:10` — \"let total = amounts.iter().sum::<u64>();\"]"),
        bracket_finding(
            "[code: `src/a.rs:10-11` — \"let average = total / amounts.len() as u64;\"]",
        ),
    ];
    let n = enforce_citation_integrity(&mut findings, &index, &mut Vec::new());
    assert_eq!(n, 0, "a range may quote any line in it");
    assert_eq!(findings.len(), 2);
}

// ─── #10964: payload and fix quotes; same-line findings ──────────────────────

/// `login.py`: line 6 builds a query from form input and compares the
/// password in plain text.
fn login_index() -> DiffContentIndex {
    DiffContentIndex::from_filtered(&filtered(vec![kept_file(
        "login.py",
        vec![hunk(
            "@@ -0,0 +1,12 @@",
            &[
                "+import sqlite3",
                "+",
                "+def login(db, form):",
                "+    user = form[\"user\"]",
                "+    pw = form[\"password\"]",
                "+    q = \"SELECT pw FROM users WHERE name = '\" + user + \"' AND pw = '\" + pw + \"'\"",
                "+    try:",
                "+        row = db.execute(q).fetchone()",
                "+    except:",
                "+        return True",
                "+    authed = row",
                "+    return authed",
            ],
        )],
    )]))
}

/// The SQL-injection quote and the plaintext-password quote, both on line 6.
const SQL_QUOTE: &str = "q = \"SELECT pw FROM users WHERE name = '\" + user";
const PW_QUOTE: &str = "\"' AND pw = '\" + pw";

fn login_finding(kind: &str, quote: Option<&str>, body: &str) -> Finding {
    let mut f = Finding::new("login.py", kind, body, "", 0.9, Effort::High);
    f.line = Some(6);
    f.quoted_snippet = quote.map(str::to_string);
    f
}

/// #10964 (TRUE 3): two findings on one line that describe different
/// defects are not a mutual contradiction; both are kept.
#[test]
fn two_defects_on_one_line_are_not_a_contradiction() {
    let mut findings = vec![
        login_finding(
            "SQL injection",
            Some(SQL_QUOTE),
            &format!("`{SQL_QUOTE}` concatenates the form name into SQL."),
        ),
        login_finding(
            "Plaintext password compare",
            Some(PW_QUOTE),
            &format!("`{PW_QUOTE}` compares the stored password in plain text."),
        ),
    ];
    let mut withheld = Vec::new();
    let n = enforce_citation_integrity(&mut findings, &login_index(), &mut withheld);
    assert_eq!(n, 0, "{withheld:?}");
    assert_eq!(findings.len(), 2);
}

/// #10964 (TRUE 3): apostrophes in prose and single quotes in SQL are not
/// citations, so two findings quoting the same code do not contradict.
#[test]
fn single_quotes_in_prose_and_sql_do_not_contradict() {
    let mut findings = vec![
        login_finding(
            "SQL injection",
            None,
            &format!("The handler doesn't escape the user's name: `{SQL_QUOTE}`."),
        ),
        login_finding(
            "SQL injection",
            None,
            &format!("It's built by concatenation in `{SQL_QUOTE}`, so it's injectable."),
        ),
    ];
    let mut withheld = Vec::new();
    let n = enforce_citation_integrity(&mut findings, &login_index(), &mut withheld);
    assert_eq!(n, 0, "{withheld:?}");
}

/// #10964 (TRUE 1): a labelled payload is not required to be in the file
/// when the finding's `quoted_snippet` is.
#[test]
fn a_labelled_payload_does_not_drop_a_finding_whose_snippet_is_in_the_file() {
    let mut f = login_finding(
        "SQL injection",
        Some(SQL_QUOTE),
        "The name reaches SQL unescaped; an input such as `' OR '1'='1' --` doesn't need the password.",
    );
    f.consequence = "Fix: `cur.execute(\"... WHERE name = ?\", (user,))` closes it.".to_string();
    let mut findings = vec![f];
    let mut withheld = Vec::new();
    let n = enforce_citation_integrity(&mut findings, &login_index(), &mut withheld);
    assert_eq!(n, 0, "{withheld:?}");
}

/// #10964 fail-open guard: the same defect claim on one line with
/// incompatible `quoted_snippet` values still drops both findings.
#[test]
fn the_same_defect_with_incompatible_snippets_still_contradicts() {
    let mut findings = vec![
        login_finding(
            "SQL injection",
            Some(SQL_QUOTE),
            "The query is built by hand.",
        ),
        login_finding(
            "SQL injection",
            Some("cursor.executescript(raw_sql)"),
            "The query is built by hand.",
        ),
    ];
    let n = enforce_citation_integrity(&mut findings, &login_index(), &mut Vec::new());
    assert_eq!(n, 2, "{findings:?}");
}

/// #10964 fail-open guard: a finding whose only backtick quote is fabricated
/// file content still drops.
#[test]
fn an_unlabelled_fabricated_quote_still_drops() {
    let mut findings = vec![login_finding(
        "SQL injection",
        None,
        "`db.execute(q, trusted=True)` runs the raw query.",
    )];
    let n = enforce_citation_integrity(&mut findings, &login_index(), &mut Vec::new());
    assert_eq!(n, 1);
}

/// #10964 fail-open guard: a labelled payload with no file quote beside it
/// is still checked, so a finding quoting only a payload drops.
#[test]
fn a_payload_with_no_file_quote_still_drops() {
    let mut findings = vec![login_finding(
        "SQL injection",
        None,
        "A payload such as `' OR '1'='1'; DROP TABLE users --` bypasses login.",
    )];
    let n = enforce_citation_integrity(&mut findings, &login_index(), &mut Vec::new());
    assert_eq!(n, 1);
}
