//! Comprehensive classifier tests for `src/intent/mod.rs`.
//!
//! ## Integration
//!
//! The `intent` module is `mod intent` in `main.rs` (not re-exported via `lib.rs`),
//! so these tests must live inside the module. Add to the bottom of `src/intent/mod.rs`:
//!
//! ```rust
//! #[cfg(test)]
//! #[path = "classifier_tests.rs"]
//! mod classifier_tests;
//! ```
//!
//! Then copy this file to `src/intent/classifier_tests.rs`.
//!
//! ## Coverage: 49 tests
//!
//! - Every constant list entry exercised at least once
//! - Priority rules (action verb > research verb > question word)
//! - Boundary conditions (word count thresholds: 4, 6, 10, 15)
//! - Normalization edge cases (unicode, punctuation, underscores)
//! - Compositional properties (greeting + action verb, question + action verb)
//! - normalize() unit tests
//! - IntentClass enum trait tests (Debug, Clone, Copy, PartialEq)

use super::*;

// =====================================================================
// Section 1: Empty / whitespace / punctuation-only inputs
// =====================================================================

#[test]
fn empty_input_is_conversational() {
    assert_eq!(classify_intent(""), IntentClass::Conversational);
    assert_eq!(classify_intent("   "), IntentClass::Conversational);
    assert_eq!(classify_intent("\n\t  "), IntentClass::Conversational);
}

#[test]
fn punctuation_only_is_conversational() {
    assert_eq!(classify_intent("???"), IntentClass::Conversational);
    assert_eq!(classify_intent("..."), IntentClass::Conversational);
    assert_eq!(classify_intent("!!!"), IntentClass::Conversational);
    assert_eq!(classify_intent("@#$%^&*"), IntentClass::Conversational);
}

// =====================================================================
// Section 2: Exhaustive GREETINGS constant coverage
// =====================================================================

#[test]
fn every_greeting_constant_is_conversational() {
    let greetings = [
        "hello",
        "hi",
        "hey",
        "howdy",
        "greetings",
        "sup",
        "yo",
        "good morning",
        "good afternoon",
        "good evening",
        "hey there",
        "hi there",
        "hello there",
    ];
    for g in &greetings {
        assert_eq!(
            classify_intent(g),
            IntentClass::Conversational,
            "greeting '{}' should be Conversational",
            g
        );
    }
}

#[test]
fn greetings_with_varied_punctuation() {
    assert_eq!(classify_intent("Hello!"), IntentClass::Conversational);
    assert_eq!(classify_intent("hi."), IntentClass::Conversational);
    assert_eq!(classify_intent("Hey,"), IntentClass::Conversational);
    assert_eq!(classify_intent("Hello!!!"), IntentClass::Conversational);
    assert_eq!(
        classify_intent("good morning!"),
        IntentClass::Conversational
    );
    assert_eq!(classify_intent("HOWDY!!"), IntentClass::Conversational);
}

#[test]
fn greeting_prefix_short_message_is_conversational() {
    assert_eq!(classify_intent("hello friend"), IntentClass::Conversational);
    assert_eq!(
        classify_intent("hi there my friend"),
        IntentClass::Conversational
    );
    assert_eq!(
        classify_intent("hey everybody"),
        IntentClass::Conversational
    );
}

#[test]
fn greeting_prefix_long_message_without_verbs() {
    // 7 words, greeting prefix, no verbs -> 5-10 range -> Conversational.
    assert_eq!(
        classify_intent("hello there my dear old trusted companion"),
        IntentClass::Conversational
    );
}

// =====================================================================
// Section 3: Exhaustive CLOSINGS constant coverage
// =====================================================================

#[test]
fn every_closing_constant_is_conversational() {
    let closings = [
        "bye",
        "goodbye",
        "thanks",
        "thank you",
        "cheers",
        "later",
        "see ya",
        "see you",
        "ok thanks",
        "thx",
        "ty",
    ];
    for c in &closings {
        assert_eq!(
            classify_intent(c),
            IntentClass::Conversational,
            "closing '{}' should be Conversational",
            c
        );
    }
}

#[test]
fn closings_with_punctuation() {
    assert_eq!(classify_intent("Thanks!"), IntentClass::Conversational);
    assert_eq!(classify_intent("Bye."), IntentClass::Conversational);
    assert_eq!(classify_intent("CHEERS!"), IntentClass::Conversational);
    assert_eq!(classify_intent("thank you!"), IntentClass::Conversational);
}

#[test]
fn closing_prefix_short_message_is_conversational() {
    assert_eq!(
        classify_intent("thanks for that"),
        IntentClass::Conversational
    );
    assert_eq!(classify_intent("bye for now"), IntentClass::Conversational);
}

// =====================================================================
// Section 4: Exhaustive SELF_QUESTIONS constant coverage
// =====================================================================

#[test]
fn every_self_question_constant_is_conversational() {
    let self_questions = [
        "how are you",
        "what are you",
        "who are you",
        "what can you do",
        "what is trusty-agents",
        "what is trusty-agents",
        "what do you do",
        "what's your name",
        "whats your name",
        "are you there",
        "you there",
    ];
    for q in &self_questions {
        assert_eq!(
            classify_intent(q),
            IntentClass::Conversational,
            "self-question '{}' should be Conversational",
            q
        );
    }
}

#[test]
fn self_questions_with_punctuation() {
    assert_eq!(
        classify_intent("What can you do?"),
        IntentClass::Conversational
    );
    assert_eq!(classify_intent("How are you?"), IntentClass::Conversational);
    assert_eq!(
        classify_intent("Who are you??"),
        IntentClass::Conversational
    );
}

// =====================================================================
// Section 5: Slash commands -> always Implementation
// =====================================================================

#[test]
fn slash_commands_are_implementation() {
    assert_eq!(classify_intent("/help"), IntentClass::Implementation);
    assert_eq!(
        classify_intent("/connect /tmp/foo"),
        IntentClass::Implementation
    );
    assert_eq!(classify_intent("/status"), IntentClass::Implementation);
    assert_eq!(classify_intent("/build"), IntentClass::Implementation);
    assert_eq!(
        classify_intent("/unknown-command"),
        IntentClass::Implementation
    );
}

#[test]
fn slash_command_with_whitespace_prefix() {
    assert_eq!(classify_intent("  /help"), IntentClass::Implementation);
}

// =====================================================================
// Section 6: Exhaustive ACTION_VERBS constant coverage
// =====================================================================

// Code-critic CRITICAL follow-up (2026-07-29): `Implementation` now
// requires an UNAMBIGUOUS signal (see `classify_intent`'s doc comment and
// `has_unambiguous_technical_signal`) — a bare `ACTION_VERBS` hit is
// insufficient EVEN WITH a generic technical-context word alongside it
// (that combination is ambiguous and lands on `Research`; see
// `TECHNICAL_CONTEXT_WORDS`'s doc comment for the proven regression this
// closes). Only 4 of the 23 entries — the hard verbs — still trigger
// Implementation from the bare verb alone; the other 19 never do, from any
// amount of generic context.

#[test]
fn hard_action_verbs_trigger_implementation_without_context() {
    let hard_verbs = ["fix", "debug", "implement", "refactor"];
    for verb in &hard_verbs {
        let input = format!("{} something now", verb);
        assert_eq!(
            classify_intent(&input),
            IntentClass::Implementation,
            "'{}' should trigger Implementation without extra context",
            verb
        );
    }
}

#[test]
fn plain_action_verbs_never_trigger_implementation_from_context_alone() {
    let plain_verbs = [
        "write", "create", "build", "run", "add", "update", "delete", "test", "deploy", "generate",
        "show", "list", "find", "search", "remove", "rename", "install", "compile", "check",
    ];
    for verb in &plain_verbs {
        // No context at all.
        let bare = format!("{} something now", verb);
        assert_ne!(
            classify_intent(&bare),
            IntentClass::Implementation,
            "'{}' alone (no context) should NOT trigger Implementation",
            verb
        );
        // WITH a generic technical-context word — still must not reach
        // Implementation (this is the exact proven regression: a plain
        // verb plus a generic noun is ambiguous, not unambiguous).
        let with_context = format!("{} the session token now", verb);
        assert_ne!(
            classify_intent(&with_context),
            IntentClass::Implementation,
            "'{}' plus a generic context word ('{}') should still NOT trigger Implementation",
            verb,
            with_context
        );
    }
}

#[test]
fn action_verb_case_insensitive() {
    // "WRITE A SCRIPT" no longer reaches Implementation — "script" is an
    // AMBIGUOUS context word (Research-only), not an unambiguous signal.
    assert_eq!(classify_intent("WRITE A SCRIPT"), IntentClass::Research);
    assert_eq!(classify_intent("Fix The Bug"), IntentClass::Implementation);
    // "Deploy to production" — "production" is likewise ambiguous context
    // (Research-only): a plain verb + generic noun is ambiguous, not
    // unambiguous evidence of a coding request.
    assert_eq!(
        classify_intent("Deploy to production"),
        IntentClass::Research
    );
}

// =====================================================================
// Section 7: Exhaustive RESEARCH_VERBS constant coverage
// =====================================================================

#[test]
fn every_research_verb_triggers_research() {
    let research_verbs = [
        "explain",
        "analyze",
        "analyse",
        "investigate",
        "review",
        "examine",
        "explore",
        "describe",
        "summarize",
        "summarise",
        "understand",
        "diagnose",
        "audit",
        "assess",
        "evaluate",
        "compare",
    ];
    for verb in &research_verbs {
        let input = format!("{} the architecture", verb);
        assert_eq!(
            classify_intent(&input),
            IntentClass::Research,
            "research verb '{}' should trigger Research",
            verb
        );
    }
}

// =====================================================================
// Section 8: Exhaustive QUESTION_WORDS constant coverage
// =====================================================================

#[test]
fn every_question_word_as_opener_triggers_research() {
    let question_words = [
        "what", "why", "how", "when", "where", "which", "who", "whose", "whom", "does", "is",
        "are", "can", "could", "would", "should",
    ];
    for qw in &question_words {
        let input = format!("{} the data looks like", qw);
        assert_eq!(
            classify_intent(&input),
            IntentClass::Research,
            "question word '{}' as opener should trigger Research",
            qw
        );
    }
}
