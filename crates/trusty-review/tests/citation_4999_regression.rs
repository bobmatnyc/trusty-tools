//! Regression tests for #4999 Part B: cross-file misattribution, and the
//! "impossible line" overlay it carries.
//!
//! #4999's 69-finding audit partitions 19 fabricated-or-partial findings as:
//! 0 invented files, 2 wrong path prefixes, and 17 where the content of file A
//! is attributed to file B with BOTH files in the diff. Five of those 17 also
//! cite a line beyond the cited file's last diffed line.
//!
//! The issue asks for a fixture built from `duetto-blast-mfes#429`: a Storybook
//! story — the FIRST file in the diff — claimed committed at `src/index.ts`
//! "overwriting the package barrel", at severity critical / confidence 0.9,
//! which drove a D grade and REQUEST_CHANGES on a clean PR.
//!
//! Two properties are locked in:
//!  (a) the misattributed finding is dropped when it quotes the other file's
//!      content — the content check #4042 shipped already covers this shape,
//!      and this fixture pins it against the #429 diff geometry;
//!  (b) the same misattribution is ALSO dropped when it quotes nothing
//!      verifiable but cites a line beyond the cited file's last diffed line.
//!      Content verification fails open there, so before #4999 this finding
//!      survived with its critical severity intact.

use trusty_review::models::{Effort, Finding, Verdict};
use trusty_review::pipeline::citation_check::{DiffContentIndex, enforce_citation_integrity};
use trusty_review::pipeline::derive_verdict;
use trusty_review::pipeline::diff_analyzer::DiffAnalyzer;

/// The `#429` diff geometry: a new Storybook story as the FIRST file, and the
/// package barrel as a later file.
///
/// The barrel's hunk header is `@@ -1,3 +1,9 @@`, so the diff reaches new-side
/// line 9 at most. Any finding citing a larger line in that file names a
/// location the reviewer was never shown. The barrel carries real statements
/// beside its re-exports on purpose: a pure import/export hunk is dropped as
/// noise in Stage B, which would leave the file with no indexed content and
/// make the fixture prove nothing about line spans.
fn repro_diff() -> String {
    let mut d = String::new();

    // File 1 — the Storybook story whose content #429 attributed to the barrel.
    d.push_str(
        "diff --git a/src/components/Divider/Divider.stories.tsx \
         b/src/components/Divider/Divider.stories.tsx\n",
    );
    d.push_str("new file mode 100644\nindex 0000000..1111111\n");
    d.push_str("--- /dev/null\n+++ b/src/components/Divider/Divider.stories.tsx\n");
    d.push_str("@@ -0,0 +1,4 @@\n");
    d.push_str("+import type { Meta, StoryObj } from '@storybook/react';\n");
    d.push_str("+import { Divider } from './Divider';\n");
    d.push_str("+const meta: Meta<typeof Divider> = { component: Divider };\n");
    d.push_str("+export default meta;\n");

    // File 2 — the package barrel, modified. New side covers lines 1..=9.
    d.push_str("diff --git a/src/index.ts b/src/index.ts\n");
    d.push_str("index 2222222..3333333 100644\n");
    d.push_str("--- a/src/index.ts\n+++ b/src/index.ts\n");
    d.push_str("@@ -1,3 +1,9 @@\n");
    d.push_str(" export { Button } from './components/Button';\n");
    d.push_str(" export { Select } from './components/Select';\n");
    d.push_str("+export { Divider } from './components/Divider';\n");
    d.push_str("+\n");
    d.push_str("+const REGISTRY = new Map<string, unknown>();\n");
    d.push_str("+export function isRegistered(name: string): boolean {\n");
    d.push_str("+  return REGISTRY.has(name);\n");
    d.push_str("+}\n");
    d.push_str(" export { Snackbar } from './components/Snackbar';\n");

    d
}

/// #429 verbatim shape: the story's own content attributed to the barrel.
///
/// The quoted excerpt is real — it just belongs to `Divider.stories.tsx`, the
/// first file in the diff, not to `src/index.ts`.
fn misattributed_with_quote() -> Finding {
    let mut f = Finding::new(
        "src/index.ts",
        "logic-error",
        "The package barrel has been overwritten with a Storybook story: \
         `src/index.ts` now contains \
         [code: `src/index.ts:2` — \"const meta: Meta<typeof Divider> = { component: Divider }\"] \
         instead of its re-exports.",
        "Every consumer importing from the package root breaks at build time.",
        0.9,
        Effort::High,
    );
    f.line = Some(2);
    f.code_provable = true;
    f
}

/// The same misattribution with NO verifiable quote, citing a line beyond the
/// barrel's last diffed line (4).
///
/// Content verification fails open here by design — there is no substantial
/// quoted fragment to ground. Line 412 is the only checkable falsehood, and it
/// is checkable deterministically: the reviewer only ever saw lines 1–4.
fn misattributed_impossible_line() -> Finding {
    let mut f = Finding::new(
        "src/index.ts",
        "logic-error",
        "The barrel file was replaced by story scaffolding at this location, \
         overwriting the public export surface.",
        "The package root exports nothing consumers can import.",
        0.9,
        Effort::High,
    );
    f.line = Some(412);
    f.code_provable = true;
    f
}

/// Same shape, but the impossible line rides in a `[code: …]` bracket citation
/// rather than the finding's own `line` field.
fn impossible_line_in_bracket_citation() -> Finding {
    let mut f = Finding::new(
        "src/index.ts",
        "logic-error",
        "Story scaffolding replaced the barrel \
         [code: `src/index.ts:412` — see the overwritten export block].",
        "The package root exports nothing consumers can import.",
        0.9,
        Effort::High,
    );
    f.line = Some(3);
    f
}

/// A legitimate finding about the barrel's real, diffed content — must survive.
fn legitimate_finding() -> Finding {
    let mut f = Finding::new(
        "src/index.ts",
        "logic-error",
        "The registry lookup \
         [code: `src/index.ts:7` — \"return REGISTRY.has(name)\"] \
         never populates REGISTRY, so it always answers false.",
        "isRegistered reports every component as unregistered.",
        0.85,
        Effort::High,
    );
    f.line = Some(7);
    f.code_provable = true;
    f
}

/// (a) The quoted cross-file misattribution from #429 is dropped, and the
/// legitimate finding about the same file survives untouched.
#[tokio::test]
async fn quoted_cross_file_misattribution_is_dropped() {
    let filtered = DiffAnalyzer::default().analyze(&repro_diff()).await;
    let index = DiffContentIndex::from_filtered(&filtered);

    let mut findings = vec![misattributed_with_quote(), legitimate_finding()];
    assert_eq!(
        derive_verdict(Verdict::Approve, &findings),
        Verdict::Block,
        "precondition: the critical misattribution drives a floor before enforcement"
    );

    let dropped = enforce_citation_integrity(&mut findings, &index);
    assert_eq!(dropped, 1, "the misattributed finding must be dropped");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].description, legitimate_finding().description);
}

/// (b) A finding citing a line beyond the cited file's last diffed line is
/// dropped even with nothing quoted to verify.
#[tokio::test]
async fn citation_beyond_the_last_diffed_line_is_dropped() {
    let filtered = DiffAnalyzer::default().analyze(&repro_diff()).await;
    let index = DiffContentIndex::from_filtered(&filtered);

    let mut findings = vec![
        misattributed_impossible_line(),
        impossible_line_in_bracket_citation(),
        legitimate_finding(),
    ];

    let dropped = enforce_citation_integrity(&mut findings, &index);
    assert_eq!(
        dropped, 2,
        "both impossible-line findings must be dropped: {findings:#?}"
    );
    assert_eq!(findings.len(), 1, "only the legitimate finding survives");
    assert_eq!(findings[0].description, legitimate_finding().description);
}

/// The check must not fire on a line the diff genuinely covers, including a
/// line reachable only through the OLD side of a hunk header.
#[tokio::test]
async fn lines_inside_the_diffed_span_are_never_dropped() {
    let filtered = DiffAnalyzer::default().analyze(&repro_diff()).await;
    let index = DiffContentIndex::from_filtered(&filtered);

    let mut findings = vec![legitimate_finding()];
    let dropped = enforce_citation_integrity(&mut findings, &index);
    assert_eq!(dropped, 0, "a grounded in-span finding must survive");
    assert_eq!(findings.len(), 1);
}
