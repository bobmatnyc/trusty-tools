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
//! ## Coverage
//!
//! - Invariants: totality (never panics), determinism, whitespace invariance
//! - Slash prefix always Implementation
//! - #4319 OWNER DECISION (2026-07-29, final iteration — seventh follow-up):
//!   NO verb — hard or plain — is evidence for `Implementation`, alone or
//!   corroborated by ANY word list. `no_verb_ever_reaches_implementation_from_context_alone`
//!   below systematically pairs all 23 `ACTION_VERBS` (including the 4
//!   `route::TCODE_HARD_VERBS`) against all 46 `TECHNICAL_CONTEXT_WORDS`
//!   entries (1058 combinations) and asserts none reach `Implementation` —
//!   this supersedes the round-6 property tests, which only ever verified
//!   the opposite (that hard verbs DID reach Implementation when
//!   corroborated), a design round 7 deleted after it was proven unsafe.
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
// Invariant 5 (REPLACED, #4319 OWNER DECISION, 2026-07-29, final iteration —
// seventh follow-up): round 6 gave the 4 `route::TCODE_HARD_VERBS`
// (fix/debug/implement/refactor) a "wins if corroborated by a
// `TECHNICAL_CONTEXT_WORDS` word" path to `Implementation` — code-critic's
// CRITICAL sixth follow-up then proved that STILL crashes on ordinary
// sentences using words already in that exact list ("fix my gym session",
// "debug the incident report from the fender bender", "refactor the outage
// in our friendship", and ~24 more — see `classifier_regression_tests.rs`'s
// `hard_verb_polysemy_with_everyday_sense_context_word_is_research_not_implementation`).
// The owner's final ruling: no word list, of any size or care, can carry
// the weight of deciding `Implementation` — the failure is structural (verb
// + word list is exploitable BY CONSTRUCTION, since it can never tell "the
// bug" the software defect from "a bug" the insect), not a gap to patch.
// So this section no longer tests "does a corroborated hard verb reach
// Implementation" (it never does, for any verb, now) — it tests the
// OPPOSITE property exhaustively: pair EVERY `ACTION_VERBS` entry (all 23,
// hard and plain treated identically) against EVERY `TECHNICAL_CONTEXT_WORDS`
// entry (all 46) and confirm NONE of the resulting 1058 combinations reach
// Implementation. This is a superset of, and supersedes, every prior
// hand-curated regression list in this section.
// =====================================================================

#[test]
fn no_verb_ever_reaches_implementation_from_a_context_word_alone() {
    // Exhaustive sweep: every ACTION_VERBS entry (23, hard and plain
    // identically — the hard-verb/plain-verb distinction only still exists
    // in `route.rs`'s separate `route_task`, never in `classify_intent`)
    // paired with every TECHNICAL_CONTEXT_WORDS entry (46) in a generic
    // "{verb} the {word}" template. None of these 1058 combinations may
    // reach Implementation; each has exactly one action verb and one
    // context word and no other signal (no question word, no research
    // verb, no file token, no snake_case identifier, no error marker), so
    // each must land on Research via the `has_action_verb &&
    // has_technical_context_word` rule.
    for verb in ACTION_VERBS {
        for word in TECHNICAL_CONTEXT_WORDS {
            let input = format!("{verb} the {word}");
            assert_ne!(
                classify_intent(&input),
                IntentClass::Implementation,
                "'{input}' (verb '{verb}' + context word '{word}') must NEVER reach Implementation — \
                 no word list, of any size, feeds the Implementation decision"
            );
            assert_eq!(
                classify_intent(&input),
                IntentClass::Research,
                "'{input}' should land on Research"
            );
        }
    }
}

#[test]
fn hard_verbs_never_reach_implementation_from_bare_verb_alone() {
    let hard_verbs = ["fix", "debug", "implement", "refactor"];
    let context_free = ["{v} something", "please {v} the thing"];
    let never_implementation = [
        "can you {v} it",
        "explain how to {v} a test",
        "what should I {v}",
        // "hello, {v} a script" used to reach Implementation once
        // corroborated by "script" (a TECHNICAL_CONTEXT_WORDS entry) — round
        // 7 deletes that path too, so it now lands on Research like every
        // other verb+context-word combination.
        "hello, {v} a script",
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
        for ctx in &never_implementation {
            let input = ctx.replace("{v}", verb);
            assert_ne!(
                classify_intent(&input),
                IntentClass::Implementation,
                "hard verb '{}' in '{}' must NEVER reach Implementation — no verb does, now",
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
    }
}

#[test]
fn hard_verbs_never_reach_implementation_against_genuinely_non_technical_objects() {
    // Code-critic CRITICAL fifth follow-up (2026-07-29): exercises each hard
    // verb against objects with an UNAMBIGUOUSLY non-technical, everyday
    // reading and NO other signal. None of these 36 combinations (4 verbs x
    // 9 objects) should ever reach Implementation — with no context word
    // present, they fall through to Conversational.
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
            assert_eq!(
                classify_intent(&input),
                IntentClass::Conversational,
                "hard verb '{}' with non-technical object '{}' ('{}') must land on Conversational",
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
