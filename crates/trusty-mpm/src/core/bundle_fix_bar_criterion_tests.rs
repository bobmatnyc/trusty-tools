//! The fix bar's acceptance-criterion class reaches every surface (#10803).
//!
//! Why: DOC-88 adds one class to the fix bar and fixes its wording once. A
//! surface that drops or paraphrases the sentence leaves one reader (PM,
//! Architect, reviewer) on the old bar, and no other test notices.
//! What: [`FIX_BAR_CRITERION_SENTENCE`], the single constant; a presence test
//! over every DOC-88 §4 surface and the rendered-prompt goldens; the `cmp`
//! check of the paired Architect skill copies; the Limits check; a red-path
//! check that removing the sentence is caught; and the F1 to F5 rubric
//! fixtures as a deterministic model of the verdict rule the rubric states.
//! Test: this file is the coverage; run `cargo test -p trusty-mpm
//! fix_bar_criterion`.
use crate::core::architect_core::collapse_whitespace;
use crate::core::content_source::test_support::repo_root;

/// The DOC-88 §2 sentence, worded once and reused verbatim on every surface.
const FIX_BAR_CRITERION_SENTENCE: &str = "A finding of any severity that directly fails a stated acceptance criterion of the brief or issue blocks; its fix uses the one fix round (no extra round).";

/// Surfaces 1 to 5, 7 and 8 of DOC-88 §4 (6 is a copy of 5, checked by bytes).
const SENTENCE_SURFACES: [&str; 7] = [
    "content/instructions/sections/core.md",
    "content/instructions/architect/core.md",
    "content/instructions/architect/architect.md",
    "content/skills/tm-workflow/references/completion-standard.md",
    "content/instructions/architect/skills/architect-completion-standard.md",
    "content/skills/code-review-standards.md",
    "content/instructions/trusty-code/skills/code-review-standards/SKILL.md",
];

/// Surfaces whose Limits text states the criterion-fail fix uses the one round.
const LIMITS_SURFACES: [&str; 4] = [
    "content/instructions/sections/core.md",
    "content/instructions/architect/core.md",
    "content/skills/tm-workflow/references/completion-standard.md",
    "content/instructions/architect/skills/architect-completion-standard.md",
];

/// The rendered prompts that carry the fix bar (`supervisor-prompt.md` has none).
const GOLDENS: [&str; 6] = [
    "pm-prompt-bundled-fallback.md",
    "pm-prompt-claude-md-override.md",
    "pm-prompt-launch-with-style.md",
    "pm-prompt-roster-absent.md",
    "architect-prompt.md",
    "architect-prompt-agents-md.md",
];

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn golden(name: &str) -> String {
    read(&format!("crates/trusty-mpm/src/core/testdata/{name}"))
}

/// The labels of `texts` whose whitespace-collapsed body lacks `sentence`.
fn lacking<'a>(texts: &[(&'a str, String)], sentence: &str) -> Vec<&'a str> {
    texts
        .iter()
        .filter(|(_, body)| !collapse_whitespace(body).contains(sentence))
        .map(|(label, _)| *label)
        .collect()
}

#[test]
fn fix_bar_criterion_sentence_is_on_every_surface_and_prompt() {
    // T1: whitespace is collapsed so a re-wrap of the source does not fail it.
    let mut texts: Vec<(&str, String)> = SENTENCE_SURFACES.iter().map(|p| (*p, read(p))).collect();
    texts.extend(GOLDENS.iter().map(|g| (*g, golden(g))));
    assert_eq!(
        lacking(&texts, FIX_BAR_CRITERION_SENTENCE),
        Vec::<&str>::new(),
        "every DOC-88 surface and prompt golden must carry the sentence verbatim"
    );
}

#[test]
fn fix_bar_criterion_sentence_missing_from_one_surface_is_caught() {
    // T6: the presence check is able to fail.
    let stripped = read(SENTENCE_SURFACES[0])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(FIX_BAR_CRITERION_SENTENCE, "");
    let texts = [(SENTENCE_SURFACES[0], stripped)];
    assert_eq!(
        lacking(&texts, FIX_BAR_CRITERION_SENTENCE),
        vec![SENTENCE_SURFACES[0]]
    );
}

#[test]
fn fix_bar_criterion_architect_skill_copies_are_identical() {
    // T2: surfaces 5 and 6, the `cmp` check `check_architect_subproject.sh` runs.
    let shipped = std::fs::read(
        repo_root().join("content/instructions/architect/skills/architect-completion-standard.md"),
    )
    .expect("content copy");
    let source = std::fs::read(
        repo_root().join("python/trusty-architect/skills/architect-completion-standard.md"),
    )
    .expect("python copy");
    assert_eq!(shipped, source, "the two skill copies must stay byte-identical");
}

#[test]
fn fix_bar_criterion_limits_use_the_one_fix_round() {
    // T5: Limits on surfaces 1, 2, 4 and 5 name the criterion-fail fix as the one round.
    for path in LIMITS_SURFACES {
        let flat = collapse_whitespace(&read(path)).to_lowercase();
        assert!(
            flat.contains("criterion-fail fix uses that one round"),
            "{path}: Limits must state the criterion-fail fix uses the one fix round"
        );
    }
}

/// One review finding as the rubric sees it.
struct Finding {
    severity: &'static str,
    /// Confidence, in percent.
    confidence: u8,
    /// `Some(source)` when the finding carries a `criterion-fail:` tag.
    criterion_source: Option<&'static str>,
}

/// The verdict rule `code-review-standards` states: the 80% filter first, then
/// BLOCK on CRITICAL or a valid criterion-fail tag, WARN on HIGH, else APPROVE.
/// A tag is valid only when its source is the issue body, the brief or an owner
/// ruling; an Architect paraphrase with no owner source is not.
fn verdict(findings: &[Finding]) -> &'static str {
    let kept: Vec<_> = findings.iter().filter(|f| f.confidence > 80).collect();
    let valid_tag = |f: &&Finding| {
        f.criterion_source
            .is_some_and(|s| s != "architect paraphrase" && s != "reviewer inference")
    };
    if kept.iter().any(|f| f.severity == "CRITICAL") || kept.iter().any(valid_tag) {
        "BLOCK"
    } else if kept.iter().any(|f| f.severity == "HIGH") {
        "WARN"
    } else {
        "APPROVE"
    }
}

#[test]
fn fix_bar_criterion_rubric_fixtures_f1_to_f5() {
    let f = |severity, confidence, criterion_source| Finding {
        severity,
        confidence,
        criterion_source,
    };
    // F1: "quoted lines stay inert" has no test; MEDIUM with a valid tag blocks.
    assert_eq!(verdict(&[f("MEDIUM", 90, Some("issue #10780 body"))]), "BLOCK");
    // F2: the criterion is inferred by the reviewer; an ordinary MEDIUM.
    assert_eq!(verdict(&[f("MEDIUM", 90, Some("reviewer inference"))]), "APPROVE");
    // F3: a LOW naming nit with no criterion does not block.
    assert_eq!(verdict(&[f("LOW", 90, None)]), "APPROVE");
    // F4: a tagged finding at 70% confidence is dropped by the 80% filter.
    assert_eq!(verdict(&[f("MEDIUM", 70, Some("brief"))]), "APPROVE");
    // F5: the criterion appears only in an Architect paraphrase; ordinary.
    assert_eq!(verdict(&[f("LOW", 90, Some("architect paraphrase"))]), "APPROVE");

    // The rubric text carries each rule the model above encodes.
    for path in [SENTENCE_SURFACES[5], SENTENCE_SURFACES[6]] {
        let flat = collapse_whitespace(&read(path));
        for part in [
            "criterion-fail: \"<verbatim quote>\" (source:",
            "or any criterion-fail finding",
            "regardless of its severity",
            "an Architect restatement with no owner source counts only as a brief criterion",
        ] {
            assert!(flat.contains(part), "{path}: rubric lacks {part:?}");
        }
    }
}
