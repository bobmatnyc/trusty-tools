//! Regression tests for #2881: fabricated `code_provable: true` finding from
//! diff-hunk misattribution.
//!
//! Two independent guarantees are locked in:
//!  (a) ATTRIBUTION — a multi-file diff shaped like the repro (new test file +
//!      small source hunk + very large regenerated bundle) is attributed to the
//!      correct file through the full parse → filter → split pipeline; no file's
//!      content leaks into another file's map unit.
//!  (b) CITATION VERIFICATION — a confabulated `code_provable` finding that cites
//!      one file but quotes content from another is downgraded before the verdict
//!      floor runs, so it can no longer fail-close a clean PR (the exact #2881
//!      D+/REQUEST_CHANGES symptom).

use trusty_review::config::mapreduce::MapReduceConfig;
use trusty_review::models::{Effort, Finding, Verdict};
use trusty_review::pipeline::citation_check::{DiffContentIndex, downgrade_uncitable_findings};
use trusty_review::pipeline::derive_verdict;
use trusty_review::pipeline::diff_analyzer::DiffAnalyzer;
use trusty_review::pipeline::mapreduce::MapOutcome;
use trusty_review::pipeline::mapreduce::outcome::TokenUsage;
use trusty_review::pipeline::mapreduce::reduce;
use trusty_review::pipeline::mapreduce::split_into_units;
use trusty_review::pipeline::mapreduce::unit::MapUnitKind;

/// Build a unified diff matching the #2881 repro: a large regenerated bundle
/// (`api/chat.js`, ordered first by git), a NEW vitest test file, and a small
/// source update — the "source + regenerated bundle committed together" pattern.
fn repro_diff() -> String {
    let mut d = String::new();

    // (3) very large bundle hunk with high line numbers, ordered first by path.
    d.push_str("diff --git a/api/chat.js b/api/chat.js\n");
    d.push_str("index 1111111..2222222 100644\n");
    d.push_str("--- a/api/chat.js\n+++ b/api/chat.js\n");
    d.push_str("@@ -35200,3 +35271,4000 @@ function compiledBundle() {\n");
    for i in 0..4000 {
        d.push_str(&format!(
            "+  const bundleSymbol{i} = webpackRequire({i});\n"
        ));
    }

    // (1) new vitest test file.
    d.push_str(
        "diff --git a/src/instructions-content.test.ts b/src/instructions-content.test.ts\n",
    );
    d.push_str("new file mode 100644\nindex 0000000..3333333\n");
    d.push_str("--- /dev/null\n+++ b/src/instructions-content.test.ts\n");
    d.push_str("@@ -0,0 +1,3 @@\n");
    d.push_str("+import { describe, it, expect } from 'vitest';\n");
    d.push_str("+describe('instructions-content', () => {\n");
    d.push_str(
        "+  it('mentions the aria prompt', () => expect(INSTRUCTIONS).toContain('aria'));\n",
    );

    // (2) small source hunk.
    d.push_str("diff --git a/src/instructions-content.ts b/src/instructions-content.ts\n");
    d.push_str("index aaaaaaa..bbbbbbb 100644\n");
    d.push_str("--- a/src/instructions-content.ts\n+++ b/src/instructions-content.ts\n");
    d.push_str(
        "@@ -10,1 +10,1 @@\n-export const ARIA = 'old';\n+export const ARIA = 'new aria prompt';\n",
    );

    d
}

/// (a) Every hunk is attributed to its own file end-to-end; the bundle map unit
/// never contains the new test file's content.
#[tokio::test]
async fn repro_attribution_is_correct_through_pipeline() {
    let diff = repro_diff();
    let filtered = DiffAnalyzer::default().analyze(&diff).await;

    // All three files survive Stage A (none is a lockfile / snapshot / dist file).
    let names: Vec<&str> = filtered.files.iter().map(|f| f.filename.as_str()).collect();
    assert!(
        names.contains(&"api/chat.js"),
        "bundle must survive: {names:?}"
    );
    assert!(names.contains(&"src/instructions-content.test.ts"));
    assert!(names.contains(&"src/instructions-content.ts"));

    let units = split_into_units(&filtered, &MapReduceConfig::default());

    // Every reviewable unit's diff text contains ONLY its own file's content.
    for u in &units {
        if let MapUnitKind::Review { diff_text } = &u.kind {
            let has_vitest = diff_text.contains("vitest") || diff_text.contains("describe(");
            if u.file == "api/chat.js" {
                assert!(
                    !has_vitest,
                    "LEAK: bundle unit contains the test file's content"
                );
                assert!(
                    diff_text.contains("bundleSymbol"),
                    "bundle unit must carry bundle code"
                );
            }
            if u.file == "src/instructions-content.test.ts" {
                assert!(has_vitest, "test unit must carry the test content");
                assert!(
                    !diff_text.contains("bundleSymbol"),
                    "LEAK: test unit contains bundle code"
                );
            }
        }
    }
}

fn fabricated_finding() -> Finding {
    // The reviewer confabulation: cites the bundle, quotes the test file's import.
    let mut f = Finding::new(
        "api/chat.js",
        "logic-error",
        "The vitest test content `import { describe, it, expect } from 'vitest';` was \
         prepended into the bundle around line 35271, corrupting the module.",
        "The bundle will fail to load at runtime.",
        0.75,
        Effort::High,
    );
    f.line = Some(35271);
    f.code_provable = true;
    f
}

/// (b) BEFORE the fix the fabricated finding drives BLOCK; AFTER citation
/// verification it is downgraded and the verdict recovers to APPROVE.
#[tokio::test]
async fn repro_fabricated_finding_no_longer_forces_block() {
    let diff = repro_diff();
    let filtered = DiffAnalyzer::default().analyze(&diff).await;
    let index = DiffContentIndex::from_filtered(&filtered);

    // BEFORE: the model tagged it code_provable, so the deterministic floor
    // escalates a REQUEST_CHANGES seed all the way to BLOCK — the #2881 fail-close.
    let before = derive_verdict(Verdict::RequestChanges, &[fabricated_finding()]);
    assert_eq!(
        before,
        Verdict::Block,
        "precondition: unverified finding forces BLOCK"
    );

    // AFTER: verify citations, then re-derive.
    let mut findings = vec![fabricated_finding()];
    let n = downgrade_uncitable_findings(&mut findings, &index);
    assert_eq!(n, 1, "the misattributed finding must be downgraded");
    assert!(!findings[0].code_provable, "code_provable must be cleared");
    // The finding is still surfaced (advisory) — downgraded, not dropped.
    assert_eq!(findings.len(), 1);

    let after = derive_verdict(Verdict::RequestChanges, &findings);
    assert_eq!(
        after,
        Verdict::Approve,
        "downgraded finding must not force any floor"
    );
}

/// (b) The same guarantee through the real map-reduce `reduce` aggregation: a
/// bundle chunk that returned REQUEST_CHANGES purely on the fabricated finding no
/// longer poisons the aggregate verdict once the finding is downgraded.
#[tokio::test]
async fn repro_reduce_recovers_after_downgrade() {
    let diff = repro_diff();
    let filtered = DiffAnalyzer::default().analyze(&diff).await;
    let index = DiffContentIndex::from_filtered(&filtered);
    let cfg = MapReduceConfig::default();

    let mut findings = vec![fabricated_finding()];
    downgrade_uncitable_findings(&mut findings, &index);

    // The bundle chunk's model verdict was REQUEST_CHANGES (driven by the finding);
    // the two source chunks were clean APPROVE.
    let outcomes = vec![
        MapOutcome::Reviewed {
            file: "api/chat.js".to_string(),
            verdict: Verdict::RequestChanges,
            findings,
            tokens: TokenUsage::default(),
        },
        MapOutcome::Reviewed {
            file: "src/instructions-content.test.ts".to_string(),
            verdict: Verdict::Approve,
            findings: Vec::new(),
            tokens: TokenUsage::default(),
        },
        MapOutcome::Reviewed {
            file: "src/instructions-content.ts".to_string(),
            verdict: Verdict::Approve,
            findings: Vec::new(),
            tokens: TokenUsage::default(),
        },
    ];

    let reduced = reduce(outcomes, &cfg);
    assert_eq!(
        reduced.verdict,
        Verdict::Approve,
        "a clean PR must not be fail-closed by a downgraded confabulation"
    );
    assert_eq!(
        reduced.findings.len(),
        1,
        "the finding is retained as advisory"
    );
    assert!(!reduced.findings[0].code_provable);
}
