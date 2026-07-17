//! Unit tests for the #2881 citation-verification layer.

use super::*;
use crate::models::{Effort, Finding};
use crate::pipeline::diff_analyzer::models::{
    FileDisposition, FilteredDiff, FilteredFile, FilteredHunk,
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

// ─── Index tests ─────────────────────────────────────────────────────────────

#[test]
fn index_from_filtered_indexes_each_file() {
    let idx = repro_index();
    assert!(idx.lookup("api/chat.js").is_some());
    assert!(idx.lookup("src/instructions-content.test.ts").is_some());
    assert!(idx.lookup("does/not/exist.rs").is_none());
}

#[test]
fn index_omits_dropped_files() {
    let mut dropped = kept_file("build/gen.js", vec![hunk("@@ -1 +1 @@", &["+x"])]);
    dropped.disposition = FileDisposition::Dropped;
    let idx = DiffContentIndex::from_filtered(&filtered(vec![dropped]));
    assert!(idx.lookup("build/gen.js").is_none());
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

// ─── Downgrade behaviour ─────────────────────────────────────────────────────

#[test]
fn downgrades_cross_file_misattribution() {
    // THE #2881 REPRO: a code_provable finding cites the bundle but quotes the
    // vitest test content, which lives in a different changed file.
    let idx = repro_index();
    let mut findings = vec![code_provable_finding(
        "api/chat.js",
        "The test file content `import { describe, it, expect } from 'vitest';` was \
         prepended into the bundle around line 35271.",
    )];
    findings[0].line = Some(35271);

    let n = downgrade_uncitable_findings(&mut findings, &idx);
    assert_eq!(n, 1, "the misattributed finding must be downgraded");
    assert!(!findings[0].code_provable, "code_provable must be cleared");
    assert!(
        findings[0].confidence <= DOWNGRADED_CITATION_CONFIDENCE,
        "confidence must be lowered to the advisory floor"
    );
    // And it must no longer be able to drive the BLOCK floor.
    assert!(!crate::pipeline::grade::drives_block_floor(&findings[0]));
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
    let n = downgrade_uncitable_findings(&mut findings, &idx);
    assert_eq!(n, 0, "a grounded finding must survive");
    assert!(findings[0].code_provable);
    assert!((findings[0].confidence - 0.75).abs() < f32::EPSILON);
}

#[test]
fn fail_open_when_cited_file_not_indexed() {
    // FAIL OPEN: a finding citing a file we did not index (outside the diff, or a
    // header the parser could not attribute) is never downgraded — without the
    // file's content there is nothing to verify against, and citing an
    // out-of-diff file is not, by itself, proof of fabrication.
    let idx = repro_index();
    let mut findings = vec![code_provable_finding(
        "src/never/changed.rs",
        "Off-by-one in the loop bound `for i in 0..=len { arr[i] }`.",
    )];
    let n = downgrade_uncitable_findings(&mut findings, &idx);
    assert_eq!(
        n, 0,
        "an un-indexed cited file must not trigger a downgrade"
    );
    assert!(findings[0].code_provable);
}

#[test]
fn fail_open_when_no_quote() {
    // A code_provable finding that quotes nothing concrete cannot be verified —
    // leave it alone (fail open) rather than risk a false-positive downgrade.
    let idx = repro_index();
    let mut findings = vec![code_provable_finding(
        "api/chat.js",
        "There may be a subtle logic issue in this function.",
    )];
    let n = downgrade_uncitable_findings(&mut findings, &idx);
    assert_eq!(n, 0, "no verifiable quote → fail open");
    assert!(findings[0].code_provable);
}

#[test]
fn ignores_non_diff_grounded_findings() {
    // A finding that is neither code_provable nor code:-cited is out of scope,
    // even if it cites content absent from its file.
    let idx = repro_index();
    let mut f = Finding::new(
        "api/chat.js",
        "style",
        "Consider `import { describe } from 'vitest';` elsewhere.",
        "n/a",
        0.9,
        Effort::Low,
    );
    f.source_citation = Some("jira:PROJ-1".to_string());
    let mut findings = vec![f];
    let n = downgrade_uncitable_findings(&mut findings, &idx);
    assert_eq!(n, 0, "non-diff-grounded findings are untouched");
    assert_eq!(findings[0].source_citation.as_deref(), Some("jira:PROJ-1"));
}

#[test]
fn downgrades_code_citation_to_unverifiable_content() {
    // A `code:`-cited finding whose quote is not in the cited file is downgraded,
    // and the fabricated code citation is stripped (spec/ticket citations would be
    // kept — see `apply_downgrade_clears_provability_and_code_citation`).
    let idx = repro_index();
    let mut f = Finding::new(
        "api/chat.js",
        "logic-error",
        "Bundle embeds `it('contains aria prompt', () => expect(INSTRUCTIONS)` verbatim.",
        "fix",
        0.8,
        Effort::High,
    );
    f.code_provable = false;
    f.source_citation = Some("code:api/chat.js:35271".to_string());
    let mut findings = vec![f];
    let n = downgrade_uncitable_findings(&mut findings, &idx);
    assert_eq!(n, 1);
    assert!(
        findings[0].source_citation.is_none(),
        "code citation must be stripped"
    );
}

// ─── Helper unit tests ───────────────────────────────────────────────────────

#[test]
fn is_diff_grounded_detects_code_provable_and_code_citation() {
    let mut f = Finding::new("f", "k", "d", "s", 0.5, Effort::Low);
    assert!(!is_diff_grounded(&f));
    f.code_provable = true;
    assert!(is_diff_grounded(&f));
    f.code_provable = false;
    f.source_citation = Some("code:f:1".to_string());
    assert!(is_diff_grounded(&f));
    f.source_citation = Some("gh:owner/repo#1".to_string());
    assert!(!is_diff_grounded(&f));
}

#[test]
fn apply_downgrade_clears_provability_and_code_citation() {
    let mut f = code_provable_finding("f", "d");
    f.source_citation = Some("code:f:1".to_string());
    f.confidence = 0.9;
    apply_downgrade(&mut f);
    assert!(!f.code_provable);
    assert!(f.source_citation.is_none());
    assert!(f.confidence <= DOWNGRADED_CITATION_CONFIDENCE);

    // A non-code citation is preserved (only the code claim is refuted).
    let mut g = code_provable_finding("f", "d");
    g.source_citation = Some("jira:X-1".to_string());
    apply_downgrade(&mut g);
    assert_eq!(g.source_citation.as_deref(), Some("jira:X-1"));
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
    let spans = extract_quoted_spans(&f);
    assert!(spans.iter().any(|s| s.contains("let x = compute(y);")));
    assert!(spans.iter().any(|s| s.contains("another_long_snippet()")));
    assert!(spans.iter().any(|s| s.contains("yet_another_fragment(z)")));
}

#[test]
fn extract_spans_skips_short() {
    let f = Finding::new("f", "k", "tiny `x` and `ab`", "s", 0.5, Effort::Low);
    assert!(
        extract_quoted_spans(&f).is_empty(),
        "short quotes are not evidence"
    );
}
