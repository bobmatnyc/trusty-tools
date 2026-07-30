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
//! - Action verb / technical-context interaction across all verb x context
//!   combinations (4 hard verbs always dominate; the other 19 NEVER reach
//!   Implementation from context alone — code-critic CRITICAL follow-up,
//!   2026-07-29)
//! - Word count boundary sweep (1-30 words, all Conversational absent
//!   verb/question signals — #4319)
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
// Invariant 5 (REVISED A THIRD TIME, code-critic CRITICAL follow-up,
// 2026-07-29): an action verb no longer unconditionally dominates, and —
// after the critic PROVED (by executing the classifier, not reasoning about
// it) that the previous "plain verb + ANY technical-context word" rule
// still crashed the subprocess pipeline on ordinary sentences ("check my
// token balance") — a plain verb can NO LONGER reach Implementation via a
// context word at all. The other 19 NEVER reach Implementation from
// verb-plus-context alone, empirically pinned across all 6 templates:
// - context-free ("{v} something", "please {v} the thing") -> Conversational
// - question-shaped ("can you {v} it", "what should I {v}") -> Research
//   (the leading question word wins, not the verb)
// - research-verb-bearing ("explain how to {v} a test") -> Research (the
//   research verb "explain" wins)
// - technical-context-word-bearing ("hello, {v} a script") -> Research
//   (AMBIGUOUS: verb + generic context word is Research, never
//   Implementation — this is the exact fix for the proven regression)
//
// This closes the corner the ORIGINAL #4319 fix left open ("can you check
// if it's raining tomorrow" still hit Implementation) AND the corner the
// FIRST follow-up reopened wider ("check my token balance" also still hit
// Implementation, because "token" was treated as sufficient context).
//
// A FOURTH follow-up (code-critic CRITICAL, 2026-07-29) then proved the 4
// `route::TCODE_HARD_VERBS` (fix/debug/implement/refactor) — previously
// exempt from all of the above, winning UNCONDITIONALLY on a bare token
// match — are polysemous too ("fix a drink", "debug why I feel anxious",
// "refactor my life" all still crashed). The blind spot: the ORIGINAL
// version of this exact test only ever exercised hard verbs against the
// same 6 templates above, every one of which still reads as the repair
// sense once a technical-context word is added ("hello, fix a script")
// — nothing ever probed a hard verb against a genuinely non-technical
// object. Hard verbs now behave IDENTICALLY to plain verbs across these 6
// templates (both require corroboration; `hard_verbs_never_reach_implementation_from_bare_verb_alone`
// below pins the 6-template matrix for hard verbs); the ONE remaining
// difference is hard verbs win even over a LEADING question word/research
// verb once corroborated (`hard_verb_plus_corroboration_beats_question_word`
// in `classifier_tests_2.rs`), while corroborated plain verbs never do
// (they only ever reach Research, per the second follow-up above).
// =====================================================================

#[test]
fn hard_verbs_never_reach_implementation_from_bare_verb_alone() {
    let hard_verbs = ["fix", "debug", "implement", "refactor"];
    let context_free = ["{v} something", "please {v} the thing"];
    let never_implementation_without_corroboration = [
        "can you {v} it",
        "explain how to {v} a test",
        "what should I {v}",
    ];

    for verb in &hard_verbs {
        for ctx in &context_free {
            let input = ctx.replace("{v}", verb);
            assert_eq!(
                classify_intent(&input),
                IntentClass::Conversational,
                "hard verb '{}' in context-free '{}' should be Conversational, not Implementation",
                verb,
                input
            );
        }
        for ctx in &never_implementation_without_corroboration {
            let input = ctx.replace("{v}", verb);
            assert_ne!(
                classify_intent(&input),
                IntentClass::Implementation,
                "hard verb '{}' in '{}' must NEVER reach Implementation without corroboration",
                verb,
                input
            );
            assert_eq!(
                classify_intent(&input),
                IntentClass::Research,
                "hard verb '{}' in '{}' should land on Research",
                verb,
                input
            );
        }
        // "hello, {v} a script" DOES reach Implementation — "script" is a
        // TECHNICAL_CONTEXT_WORDS entry, so this is corroborated, not a
        // bare-verb win. Included here (rather than a bare-verb template)
        // specifically to confirm corroboration still works when present.
        let corroborated = format!("hello, {verb} a script");
        assert_eq!(
            classify_intent(&corroborated),
            IntentClass::Implementation,
            "hard verb '{}' plus context word 'script' in '{}' SHOULD be Implementation",
            verb,
            corroborated
        );
    }
}

#[test]
fn hard_verbs_require_corroboration_against_genuinely_non_technical_objects() {
    // Code-critic CRITICAL fifth follow-up (2026-07-29): the blind spot that
    // let hard verbs stay unconditional for six rounds — every existing
    // test paired them with either a technical noun or a template that
    // still read as the repair sense. This exercises each hard verb
    // against objects with an UNAMBIGUOUSLY non-technical, everyday
    // reading and NO other signal. None of these 36 combinations (4 verbs x
    // 9 objects) should ever reach Implementation.
    let hard_verbs = ["fix", "debug", "implement", "refactor"];
    let non_technical_objects = [
        "a drink",
        "my hair",
        "breakfast",
        "my schedule",
        "my life",
        "a new morning routine",
        "better habits",
        "why I feel anxious",
        "me up with your friend",
    ];
    for verb in &hard_verbs {
        for obj in &non_technical_objects {
            let input = format!("{verb} {obj}");
            assert_ne!(
                classify_intent(&input),
                IntentClass::Implementation,
                "hard verb '{}' with non-technical object '{}' ('{}') must NOT reach Implementation",
                verb,
                obj,
                input
            );
        }
    }
}

#[test]
fn plain_action_verbs_never_reach_implementation_from_context_alone() {
    let plain_verbs = [
        "write", "create", "build", "run", "add", "update", "delete", "test", "deploy", "generate",
        "show", "list", "find", "search", "remove", "rename", "install", "compile", "check",
    ];
    let context_free = ["{v} something", "please {v} the thing"];
    let never_implementation = [
        "can you {v} it",
        "explain how to {v} a test",
        "what should I {v}",
        "hello, {v} a script",
    ];

    for verb in &plain_verbs {
        for ctx in &context_free {
            let input = ctx.replace("{v}", verb);
            assert_eq!(
                classify_intent(&input),
                IntentClass::Conversational,
                "plain verb '{}' in context-free '{}' should be Conversational, not Implementation",
                verb,
                input
            );
        }
        for ctx in &never_implementation {
            let input = ctx.replace("{v}", verb);
            assert_ne!(
                classify_intent(&input),
                IntentClass::Implementation,
                "plain verb '{}' in '{}' must NEVER reach Implementation from context alone",
                verb,
                input
            );
            assert_eq!(
                classify_intent(&input),
                IntentClass::Research,
                "plain verb '{}' in '{}' should land on Research",
                verb,
                input
            );
        }
    }
}

// =====================================================================
// Parameterized: word count boundary sweep (no verb signals)
//
// #4319: word count alone must NEVER promote a message to Implementation.
// Every length from 1 to 30 words, absent any action/research verb,
// question word, or trailing "?", stays Conversational.
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

    for n in 1..=30 {
        let input: String = filler
            .iter()
            .cycle()
            .take(n)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let result = classify_intent(&input);
        assert_eq!(
            result,
            IntentClass::Conversational,
            "word_count={} (no verb signals) should be Conversational, got {:?}",
            n,
            result
        );
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
