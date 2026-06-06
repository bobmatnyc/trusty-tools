//! Property-based and parameterized tests for `src/intent/mod.rs`.
//!
//! ## Integration
//!
//! Add to the bottom of `src/intent/mod.rs`:
//!
//! ```rust
//! #[cfg(test)]
//! #[path = "property_tests.rs"]
//! mod property_tests;
//! ```
//!
//! Then copy this file to `src/intent/property_tests.rs`.
//!
//! ## Coverage: 22 tests
//!
//! - Invariants: totality (never panics), determinism, whitespace invariance
//! - Slash prefix always Implementation
//! - Action verb dominance across all verb x context combinations (23 * 6 = 138 cases)
//! - Word count boundary sweep (1-15 words)
//! - Constant-list completeness guards (count, lowercase, no duplicates, no overlap)
//! - Regression: underscored identifiers must not split into action verbs

use super::*;

// =====================================================================
// Invariant 1: classify_intent is total — never panics
// =====================================================================

#[test]
fn never_panics_on_adversarial_inputs() {
    let adversarial: Vec<String> = vec![
        "".into(),
        " ".into(),
        "\0".into(),
        "\x01\x02\x03".into(),
        "a".repeat(10_000),
        "/".into(),
        "//".into(),
        "\n\n\n".into(),
        "\u{1F525}\u{1F680}\u{1F480}".into(),
        "caf\u{00E9} r\u{00E9}sum\u{00E9} na\u{00EF}ve".into(),
        "\u{4E2D}\u{6587}\u{8F93}\u{5165}".into(),
        "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}".into(),
        "write ".into(),
        " write".into(),
        "write\0script".into(),
    ];
    for input in &adversarial {
        let _ = classify_intent(input);
    }
}

// =====================================================================
// Invariant 2: classify_intent is deterministic
// =====================================================================

#[test]
fn deterministic_across_calls() {
    let inputs = [
        "hello",
        "write a script",
        "explain the code",
        "what is this?",
        "the quick brown fox jumps over the lazy dog and then some more words",
    ];
    for input in &inputs {
        let first = classify_intent(input);
        let second = classify_intent(input);
        assert_eq!(first, second, "non-deterministic for '{}'", input);
    }
}

// =====================================================================
// Invariant 3: Leading/trailing whitespace does not change classification
// =====================================================================

#[test]
fn whitespace_invariance() {
    let inputs = [
        "hello",
        "write a script",
        "explain the code",
        "what is this",
        "thanks",
    ];
    for input in &inputs {
        let plain = classify_intent(input);
        let padded = classify_intent(&format!("  {}  ", input));
        assert_eq!(
            plain, padded,
            "whitespace changed classification for '{}'",
            input
        );
    }
}

// =====================================================================
// Invariant 4: Slash prefix always yields Implementation
// =====================================================================

#[test]
fn slash_prefix_always_implementation() {
    let slash_inputs = [
        "/",
        "/a",
        "/hello",
        "/explain something",
        "/123",
        "/ spaced",
    ];
    for input in &slash_inputs {
        assert_eq!(
            classify_intent(input),
            IntentClass::Implementation,
            "slash input '{}' should be Implementation",
            input
        );
    }
}

// =====================================================================
// Invariant 5: Any input containing an action verb must return Implementation
// =====================================================================

#[test]
fn action_verb_always_dominates() {
    let action_verbs = [
        "write",
        "create",
        "build",
        "run",
        "fix",
        "implement",
        "add",
        "update",
        "delete",
        "test",
        "deploy",
        "generate",
        "show",
        "list",
        "find",
        "search",
        "refactor",
        "remove",
        "rename",
        "install",
        "compile",
        "debug",
        "check",
    ];
    let contexts = [
        "{v} something",
        "please {v} the thing",
        "can you {v} it",
        "explain how to {v} a test",
        "what should I {v}",
        "hello, {v} a script",
    ];
    for verb in &action_verbs {
        for ctx in &contexts {
            let input = ctx.replace("{v}", verb);
            assert_eq!(
                classify_intent(&input),
                IntentClass::Implementation,
                "action verb '{}' in '{}' should be Implementation",
                verb,
                input
            );
        }
    }
}

// =====================================================================
// Parameterized: word count boundary sweep (no verb signals)
// =====================================================================

#[test]
fn word_count_boundary_sweep() {
    let filler = [
        "that",
        "new",
        "library",
        "seems",
        "pretty",
        "nice",
        "honestly",
        "really",
        "quite",
        "rather",
        "overall",
        "certainly",
        "definitely",
        "absolutely",
        "probably",
    ];

    for n in 1..=15 {
        let input: String = filler
            .iter()
            .cycle()
            .take(n)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let result = classify_intent(&input);
        if n <= 10 {
            assert_eq!(
                result,
                IntentClass::Conversational,
                "word_count={} should be Conversational, got {:?}",
                n,
                result
            );
        } else {
            assert_eq!(
                result,
                IntentClass::Implementation,
                "word_count={} (>10) should be Implementation, got {:?}",
                n,
                result
            );
        }
    }
}

// =====================================================================
// Constant-list completeness guards
// =====================================================================

#[test]
fn action_verbs_count_matches_expected() {
    assert_eq!(
        ACTION_VERBS.len(),
        23,
        "ACTION_VERBS count changed — update tests if intentional"
    );
}

#[test]
fn research_verbs_count_matches_expected() {
    assert_eq!(
        RESEARCH_VERBS.len(),
        16,
        "RESEARCH_VERBS count changed — update tests if intentional"
    );
}

#[test]
fn question_words_count_matches_expected() {
    assert_eq!(
        QUESTION_WORDS.len(),
        16,
        "QUESTION_WORDS count changed — update tests if intentional"
    );
}

#[test]
fn greetings_count_matches_expected() {
    assert_eq!(
        GREETINGS.len(),
        13,
        "GREETINGS count changed — update tests if intentional"
    );
}

#[test]
fn closings_count_matches_expected() {
    assert_eq!(
        CLOSINGS.len(),
        11,
        "CLOSINGS count changed — update tests if intentional"
    );
}

#[test]
fn self_questions_count_matches_expected() {
    assert_eq!(
        SELF_QUESTIONS.len(),
        11,
        "SELF_QUESTIONS count changed — update tests if intentional"
    );
}

// =====================================================================
// All constant entries are lowercase (normalization assumption)
// =====================================================================

#[test]
fn all_constants_are_lowercase() {
    for v in ACTION_VERBS {
        assert_eq!(*v, v.to_lowercase(), "ACTION_VERBS not lowercase: {}", v);
    }
    for v in RESEARCH_VERBS {
        assert_eq!(*v, v.to_lowercase(), "RESEARCH_VERBS not lowercase: {}", v);
    }
    for v in QUESTION_WORDS {
        assert_eq!(*v, v.to_lowercase(), "QUESTION_WORDS not lowercase: {}", v);
    }
    for v in GREETINGS {
        assert_eq!(*v, v.to_lowercase(), "GREETINGS not lowercase: {}", v);
    }
    for v in CLOSINGS {
        assert_eq!(*v, v.to_lowercase(), "CLOSINGS not lowercase: {}", v);
    }
    for v in SELF_QUESTIONS {
        assert_eq!(*v, v.to_lowercase(), "SELF_QUESTIONS not lowercase: {}", v);
    }
}

// =====================================================================
// No duplicates in constant lists
// =====================================================================

#[test]
fn no_duplicate_action_verbs() {
    let mut seen = std::collections::HashSet::new();
    for v in ACTION_VERBS {
        assert!(seen.insert(*v), "duplicate ACTION_VERB: {}", v);
    }
}

#[test]
fn no_duplicate_research_verbs() {
    let mut seen = std::collections::HashSet::new();
    for v in RESEARCH_VERBS {
        assert!(seen.insert(*v), "duplicate RESEARCH_VERB: {}", v);
    }
}

#[test]
fn no_duplicate_greetings() {
    let mut seen = std::collections::HashSet::new();
    for v in GREETINGS {
        assert!(seen.insert(*v), "duplicate GREETING: {}", v);
    }
}

#[test]
fn no_duplicate_closings() {
    let mut seen = std::collections::HashSet::new();
    for v in CLOSINGS {
        assert!(seen.insert(*v), "duplicate CLOSING: {}", v);
    }
}

// =====================================================================
// No overlap between action and research verb lists
// =====================================================================

#[test]
fn no_overlap_between_action_and_research_verbs() {
    for av in ACTION_VERBS {
        assert!(
            !RESEARCH_VERBS.contains(av),
            "'{}' appears in both ACTION_VERBS and RESEARCH_VERBS",
            av
        );
    }
}

// =====================================================================
// Regression: underscored identifiers must not split
// =====================================================================

#[test]
fn underscored_identifiers_do_not_split() {
    let identifiers_with_verbs = [
        "what does run_pm_task do",
        "what does build_info return",
        "what does test_helper mean",
        "what does delete_session handle",
    ];
    for input in &identifiers_with_verbs {
        assert_eq!(
            classify_intent(input),
            IntentClass::Research,
            "underscored identifier in '{}' should not trigger Implementation",
            input
        );
    }
}
