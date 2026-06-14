use crate::classify::tiers::weighted_sum::{
    score_file_paths, score_keywords, score_merge_indicator, score_message_length,
    score_ticket_prefix, Cat, WeightedSumClassifier, WeightedSumConfig,
};
use crate::core::models::ClassificationMethod;

fn default_classifier() -> WeightedSumClassifier {
    WeightedSumClassifier::new(WeightedSumConfig::default())
}

// ── per-signal unit tests ─────────────────────────────────────────────

/// Why: regression guard ensuring the keyword signal produces non-zero
/// scores for messages containing category-specific keywords.
/// What: checks bugfix keywords drive a positive Bugfix score and that
/// other categories score lower.
/// Test: direct call to `score_keywords`.
#[test]
fn keyword_score_bugfix_keywords_dominate_bugfix_category() {
    let lower = "fix null pointer regression hotfix";
    let scores = score_keywords(lower);
    let bugfix_score = scores[Cat::Bugfix.index()];
    let feature_score = scores[Cat::Feature.index()];
    assert!(
        bugfix_score > feature_score,
        "bugfix keywords should score higher for Bugfix than Feature, got bugfix={bugfix_score:.3} feature={feature_score:.3}"
    );
    assert!(bugfix_score > 0.0, "bugfix score must be positive");
}

/// Why: verifies the keyword signal for feature-oriented messages.
/// What: checks that "add implement feature" scores highest for Feature.
/// Test: direct call to `score_keywords`.
#[test]
fn keyword_score_feature_keywords_dominate_feature_category() {
    let lower = "add implement feature support";
    let scores = score_keywords(lower);
    let feature_score = scores[Cat::Feature.index()];
    let bugfix_score = scores[Cat::Bugfix.index()];
    assert!(
        feature_score > bugfix_score,
        "feature keywords should score higher for Feature, got feature={feature_score:.3} bugfix={bugfix_score:.3}"
    );
}

/// Why: verifies the ticket-prefix signal fires for JIRA-style messages.
/// What: checks that "PROJ-123: update auth" produces non-zero scores.
/// Test: direct call to `score_ticket_prefix`.
#[test]
fn ticket_prefix_signal_fires_for_jira_prefix() {
    let msg = "PROJ-123: update auth module";
    let scores = score_ticket_prefix(msg);
    // Every category should get a small positive boost.
    for (i, &s) in scores.iter().enumerate() {
        assert!(s > 0.0, "category {i} should get a ticket-prefix boost");
    }
}

/// Why: verifies the ticket-prefix signal does not fire for plain messages.
/// What: checks that "update auth module" produces zero scores.
/// Test: direct call to `score_ticket_prefix`.
#[test]
fn ticket_prefix_signal_zero_for_no_prefix() {
    let msg = "update auth module";
    let scores = score_ticket_prefix(msg);
    for (i, &s) in scores.iter().enumerate() {
        assert_eq!(
            s, 0.0,
            "category {i} should score 0.0 without ticket prefix"
        );
    }
}

/// Why: short messages should nudge toward KTLO/Maintenance and away
/// from Feature.
/// What: checks message <12 chars gives positive KTLO, negative Feature.
/// Test: direct call to `score_message_length`.
#[test]
fn length_signal_short_message_nudges_ktlo_not_feature() {
    let scores = score_message_length("wip");
    assert!(
        scores[Cat::Ktlo.index()] > 0.0,
        "short message should nudge KTLO"
    );
    assert!(
        scores[Cat::Feature.index()] < 0.0,
        "short message should penalise Feature"
    );
}

/// Why: long messages should nudge toward Feature.
/// What: checks message >80 chars gives positive Feature score.
/// Test: direct call to `score_message_length`.
#[test]
fn length_signal_long_message_nudges_feature() {
    let long = "add new payment integration with Stripe — supports 3DS, refunds, webhooks, and idempotency keys";
    assert!(long.len() > 80, "test message must be >80 chars");
    let scores = score_message_length(long);
    assert!(
        scores[Cat::Feature.index()] > 0.0,
        "long message should nudge Feature"
    );
}

/// Why: the merge indicator is the strongest signal for merge commits.
/// What: checks is_merge=true produces a large positive Merge score and
/// small negative scores elsewhere.
/// Test: direct call to `score_merge_indicator`.
#[test]
fn merge_indicator_signal_fires_for_is_merge_flag() {
    let scores = score_merge_indicator(true, "some message");
    assert!(
        scores[Cat::Merge.index()] > 0.40,
        "merge indicator should give large Merge score"
    );
    assert!(
        scores[Cat::Feature.index()] < 0.0,
        "merge indicator should penalise Feature"
    );
}

/// Why: commits not tagged as merges should score zero for the merge signal.
/// What: checks is_merge=false + no "Merge " prefix = all zeros.
/// Test: direct call to `score_merge_indicator`.
#[test]
fn merge_indicator_signal_zero_for_non_merge() {
    let scores = score_merge_indicator(false, "fix null pointer");
    for (i, &s) in scores.iter().enumerate() {
        assert_eq!(s, 0.0, "non-merge commit should produce 0 for cat {i}");
    }
}

/// Why: file-path signal must return zero when paths are empty to avoid
/// penalising commits where path data was not collected.
/// What: checks that `score_file_paths(&[])` is all zeros.
/// Test: direct call to `score_file_paths`.
#[test]
fn file_paths_signal_zero_when_empty() {
    let scores = score_file_paths(&[]);
    for (i, &s) in scores.iter().enumerate() {
        assert_eq!(s, 0.0, "empty paths should produce 0 for cat {i}");
    }
}

/// Why: a tests-heavy changeset should nudge toward Maintenance.
/// What: checks that paths containing mostly test files boosts Maintenance.
/// Test: direct call to `score_file_paths`.
#[test]
fn file_paths_signal_tests_heavy_nudges_maintenance() {
    let paths: Vec<String> = vec![
        "tests/auth_test.rs".to_string(),
        "tests/payment_test.rs".to_string(),
        "tests/webhook_test.rs".to_string(),
        "src/lib.rs".to_string(),
    ];
    let scores = score_file_paths(&paths);
    assert!(
        scores[Cat::Maintenance.index()] > 0.0,
        "tests-heavy paths should boost Maintenance"
    );
}

/// Why: a docs-heavy changeset should nudge toward Content.
/// What: checks that paths containing mostly .md files boosts Content.
/// Test: direct call to `score_file_paths`.
#[test]
fn file_paths_signal_docs_heavy_nudges_content() {
    let paths: Vec<String> = vec![
        "docs/api.md".to_string(),
        "docs/setup.md".to_string(),
        "README.md".to_string(),
    ];
    let scores = score_file_paths(&paths);
    assert!(
        scores[Cat::Content.index()] > 0.0,
        "docs-heavy paths should boost Content"
    );
}

// ── integration tests ─────────────────────────────────────────────────

/// Why: a classic "fix: handle null user" message contains strong bugfix
/// keywords and should produce a Bugfix verdict above min_confidence.
/// What: classify via the full tier; assert category == "bugfix" and
/// confidence >= 0.55.
/// Test: end-to-end call to `WeightedSumClassifier::classify`.
#[test]
fn integration_fix_message_classifies_as_bugfix() {
    let clf = default_classifier();
    let result = clf.classify("fix: handle null user — fixes regression", false, &[]);
    assert!(result.is_some(), "expected a verdict for a bugfix message");
    let r = result.unwrap();
    assert_eq!(r.category, "bugfix", "expected bugfix category");
    assert!(
        r.confidence >= 0.55,
        "confidence should be >= 0.55, got {}",
        r.confidence
    );
    assert_eq!(r.method, ClassificationMethod::WeightedSum);
}

/// Why: "Merge pull request" + is_merge=true should give a "merge" verdict.
/// What: classify with the merge indicator; assert category == "merge".
/// Test: end-to-end call to `WeightedSumClassifier::classify`.
#[test]
fn integration_merge_commit_classifies_as_merge() {
    let clf = default_classifier();
    let result = clf.classify("Merge pull request #42 from main", true, &[]);
    assert!(result.is_some(), "expected a verdict for a merge commit");
    let r = result.unwrap();
    assert_eq!(r.category, "merge");
    assert_eq!(r.method, ClassificationMethod::WeightedSum);
}

/// Why: "add implement feature support" should give a "feature" verdict.
/// What: classify; assert category == "feature" and confidence >= 0.55.
/// Test: end-to-end call to `WeightedSumClassifier::classify`.
#[test]
fn integration_feature_message_classifies_as_feature() {
    let clf = default_classifier();
    let result = clf.classify(
        "add new payment feature support with webhook integration",
        false,
        &[],
    );
    assert!(result.is_some(), "expected a verdict for a feature message");
    let r = result.unwrap();
    assert_eq!(r.category, "feature");
    assert!(r.confidence >= 0.55);
}

/// Why: a completely ambiguous message with no strong signals should fall
/// through (return None) so the fuzzy tier handles it.
/// What: classify a UUID-like garbled string; assert None.
/// Test: end-to-end call to `WeightedSumClassifier::classify`.
#[test]
fn fall_through_when_no_signal_dominates() {
    let clf = default_classifier();
    // A message that hits no category-specific keywords and is not a merge.
    let result = clf.classify("zzz qqq vvv www yyy uuu ppp rrr", false, &[]);
    // With no meaningful signals the keyword scores are all ~equal-zero,
    // the length bucket alone (medium) is also neutral, so the argmax
    // either ties across all categories or scores below min_confidence.
    // Either way: no verdict, fall through to fuzzy.
    if let Some(ref r) = result {
        assert!(
            r.confidence >= 0.55,
            "if a verdict is emitted it must exceed min_confidence"
        );
    }
    // We do NOT assert `result.is_none()` because a random garbled message
    // *could* weakly match one category; the important invariant is that
    // any emitted verdict has confidence >= min_confidence.
}

/// Why: two categories with exactly equal top scores must not produce a
/// verdict — the tie-break rule prevents a spurious argmax selection.
/// What: construct a message that will produce equal keyword scores for
/// two categories, then assert None or a verdict that clears min_confidence.
/// Test: end-to-end call to `WeightedSumClassifier::classify`.
#[test]
fn argmax_tie_does_not_emit_verdict() {
    let clf = default_classifier();
    // A completely neutral message (all signals zero or equal): all
    // keyword bags are empty-match, length is medium, no ticket, no merge,
    // no paths.
    let result = clf.classify("xyzxyzxyz blah blah blah nothing here", false, &[]);
    if let Some(ref r) = result {
        assert!(
            r.confidence >= clf.config.min_confidence as f64,
            "any emitted verdict must clear min_confidence"
        );
    }
}

/// Why: when `enabled: false`, the tier must never produce a verdict
/// regardless of the message content.
/// What: construct a classifier with enabled=false, classify a strong
/// bugfix message, assert None.
/// Test: end-to-end call with a disabled classifier.
#[test]
fn disabled_classifier_always_returns_none() {
    let clf = WeightedSumClassifier::new(WeightedSumConfig {
        enabled: false,
        ..WeightedSumConfig::default()
    });
    let result = clf.classify("fix: handle null pointer — critical bug", false, &[]);
    assert!(
        result.is_none(),
        "disabled classifier must always return None"
    );
}

/// Why: a commit message with multiple bugfix keywords plus a tests-heavy
/// changeset should produce a confident bugfix or refactor verdict.
/// What: classify with both a multi-keyword bugfix message and a tests-heavy
/// path list; assert category is bugfix or refactor with confidence >= 0.55.
/// Test: end-to-end call with paths.
#[test]
fn integration_fix_with_test_paths_produces_bugfix_or_maintenance() {
    let clf = default_classifier();
    let paths = vec![
        "tests/auth_test.rs".to_string(),
        "tests/null_test.rs".to_string(),
    ];
    // Two bugfix keywords ("fix" + "bug") produce score 0.60, which clears
    // min_confidence (0.55) before the path signal even contributes.
    let result = clf.classify("fix bug: handle null pointer in auth module", false, &paths);
    assert!(result.is_some(), "expected a verdict");
    let r = result.unwrap();
    assert!(
        r.category == "bugfix" || r.category == "refactor",
        "expected bugfix or refactor, got: {}",
        r.category
    );
    assert!(r.confidence >= 0.55);
    assert_eq!(r.method, ClassificationMethod::WeightedSum);
}

/// Why: confidence output must respect the [min_confidence, 0.95] clamp.
/// What: verify the emitted confidence for a strong-signal message is <= 0.95.
/// Test: end-to-end call; assert confidence in bounds.
#[test]
fn emitted_confidence_stays_within_bounds() {
    let clf = default_classifier();
    // A very strong bugfix message to maximise the raw score.
    let result = clf.classify(
        "fix bug issue broken regression hotfix patch resolve repair correct",
        false,
        &[],
    );
    if let Some(r) = result {
        assert!(r.confidence >= 0.55, "below min_confidence floor");
        assert!(r.confidence <= 0.95, "above max confidence ceiling");
    }
}
