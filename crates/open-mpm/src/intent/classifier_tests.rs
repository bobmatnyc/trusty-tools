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
        "what is open-mpm",
        "what is open mpm",
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

#[test]
fn every_action_verb_triggers_implementation() {
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
    for verb in &action_verbs {
        let input = format!("{} something now", verb);
        assert_eq!(
            classify_intent(&input),
            IntentClass::Implementation,
            "action verb '{}' should trigger Implementation",
            verb
        );
    }
}

#[test]
fn action_verb_case_insensitive() {
    assert_eq!(
        classify_intent("WRITE A SCRIPT"),
        IntentClass::Implementation
    );
    assert_eq!(classify_intent("Fix The Bug"), IntentClass::Implementation);
    assert_eq!(
        classify_intent("Deploy to production"),
        IntentClass::Implementation
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

#[test]
fn question_word_not_first_does_not_trigger_research() {
    // 4 words, no signals -> Conversational.
    assert_eq!(
        classify_intent("the what of it"),
        IntentClass::Conversational
    );
}

// =====================================================================
// Section 9: Priority rules — action verb wins over everything
// =====================================================================

#[test]
fn action_verb_wins_over_question_word() {
    assert_eq!(
        classify_intent("how do I fix this bug"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("what should I write here"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("where should I deploy this"),
        IntentClass::Implementation
    );
}

#[test]
fn action_verb_wins_over_research_verb() {
    assert_eq!(
        classify_intent("explain how to write a test"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("review and fix the code"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("analyze then refactor the module"),
        IntentClass::Implementation
    );
}

#[test]
fn action_verb_wins_over_greeting_prefix() {
    assert_eq!(
        classify_intent("hi, can you write a script that adds two numbers?"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Hello, please fix the failing test in src/main.rs"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("hey, run the tests"),
        IntentClass::Implementation
    );
}

#[test]
fn write_a_review_is_implementation() {
    assert_eq!(
        classify_intent("write a review"),
        IntentClass::Implementation
    );
}

// =====================================================================
// Section 10: Question-mark fallback
// =====================================================================

#[test]
fn short_question_mark_is_research() {
    assert_eq!(
        classify_intent("is bedrock enabled?"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("does this support tokio?"),
        IntentClass::Research
    );
}

#[test]
fn question_mark_on_long_input_without_action_verb() {
    let long_q = "so I was wondering about the overall performance characteristics \
                  of the system under heavy load with many concurrent users?";
    assert_eq!(classify_intent(long_q), IntentClass::Implementation);
}

#[test]
fn question_mark_at_15_words_boundary() {
    let input = "that thing about the data pipeline staging environment having problems \
                 every single night recently?";
    let word_count = input.split_whitespace().count();
    assert!(word_count <= 15, "expected <=15 words, got {}", word_count);
    assert_eq!(classify_intent(input), IntentClass::Research);
}

// =====================================================================
// Section 11: Word count boundary conditions
// =====================================================================

#[test]
fn four_word_input_no_signals_is_conversational() {
    assert_eq!(
        classify_intent("just a random thought"),
        IntentClass::Conversational
    );
}

#[test]
fn five_to_ten_words_no_signals_is_conversational() {
    assert_eq!(
        classify_intent("that new library seems pretty nice honestly"),
        IntentClass::Conversational
    );
}

#[test]
fn eleven_plus_words_no_verbs_is_implementation() {
    let long = "the failing integration test for the auth middleware on staging \
                seems related to the recent token refresh changes from last week";
    assert!(long.split_whitespace().count() > 10);
    assert_eq!(classify_intent(long), IntentClass::Implementation);
}

#[test]
fn greeting_prefix_word_count_boundary_at_six() {
    assert_eq!(
        classify_intent("hello my dear old trusted friend"),
        IntentClass::Conversational
    );
    assert_eq!(
        classify_intent("hello my dear old trusted good friend"),
        IntentClass::Conversational
    );
}

// =====================================================================
// Section 12: "help me" special case
// =====================================================================

#[test]
fn help_me_is_implementation() {
    assert_eq!(
        classify_intent("help me debug this issue"),
        IntentClass::Implementation
    );
    assert_eq!(classify_intent("help me"), IntentClass::Implementation);
}

#[test]
fn help_alone_is_conversational() {
    assert_eq!(classify_intent("help"), IntentClass::Conversational);
}

#[test]
fn help_question_mark_is_research() {
    assert_eq!(classify_intent("help?"), IntentClass::Research);
}

// =====================================================================
// Section 13: Normalization edge cases
// =====================================================================

#[test]
fn case_insensitivity() {
    assert_eq!(classify_intent("HELLO"), IntentClass::Conversational);
    assert_eq!(
        classify_intent("EXPLAIN the architecture"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("WRITE A SCRIPT"),
        IntentClass::Implementation
    );
}

#[test]
fn underscores_preserved_prevent_false_action_match() {
    assert_eq!(
        classify_intent("what does run_pm_task_with_session do"),
        IntentClass::Research
    );
}

#[test]
fn hyphens_preserved_in_identifiers() {
    assert_eq!(
        classify_intent("what is open-mpm"),
        IntentClass::Conversational
    );
}

#[test]
fn apostrophe_preserved() {
    assert_eq!(
        classify_intent("what's your name"),
        IntentClass::Conversational
    );
}

#[test]
fn mixed_punctuation_normalized() {
    assert_eq!(classify_intent("Hello!!!"), IntentClass::Conversational);
    assert_eq!(
        classify_intent("Write...a...script"),
        IntentClass::Implementation
    );
}

#[test]
fn unicode_lowercasing() {
    assert_eq!(classify_intent("GRÜßE"), IntentClass::Conversational);
}

#[test]
fn tabs_and_newlines_treated_as_whitespace() {
    assert_eq!(
        classify_intent("write\ta\nscript"),
        IntentClass::Implementation
    );
}

// =====================================================================
// Section 14: Single ambiguous words
// =====================================================================

#[test]
fn single_ambiguous_word_is_conversational() {
    assert_eq!(classify_intent("yes"), IntentClass::Conversational);
    assert_eq!(classify_intent("ok"), IntentClass::Conversational);
    assert_eq!(classify_intent("cool"), IntentClass::Conversational);
    assert_eq!(classify_intent("sure"), IntentClass::Conversational);
    assert_eq!(classify_intent("nope"), IntentClass::Conversational);
}

// =====================================================================
// Section 15: Normalize function unit tests
// =====================================================================

#[test]
fn normalize_strips_punctuation_preserves_apostrophe() {
    assert_eq!(normalize("Hello!!!"), "hello");
    assert_eq!(normalize("what's up?"), "what's up");
    assert_eq!(normalize("open-mpm"), "open-mpm");
    assert_eq!(normalize("run_pm_task"), "run_pm_task");
}

#[test]
fn normalize_collapses_whitespace() {
    assert_eq!(normalize("  hello   world  "), "hello world");
    assert_eq!(normalize("a\t\nb"), "a b");
}

#[test]
fn normalize_empty_and_punctuation() {
    assert_eq!(normalize(""), "");
    assert_eq!(normalize("!!!"), "");
    assert_eq!(normalize("   "), "");
}

// =====================================================================
// Section 16: Real-world scenarios
// =====================================================================

#[test]
fn real_world_task_requests() {
    assert_eq!(
        classify_intent("Write a Python script that formats data as a markdown table"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Create a REST API endpoint for user registration"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Refactor the database module to use connection pooling"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Deploy the staging environment"),
        IntentClass::Implementation
    );
}

#[test]
fn real_world_research_requests() {
    assert_eq!(
        classify_intent("What does the workflow engine do?"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("Explain the IPC protocol between PM and sub-agents"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("How does context budgeting work in the memory system?"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("Compare the performance of redb vs sled"),
        IntentClass::Research
    );
}

#[test]
fn real_world_conversational() {
    assert_eq!(
        classify_intent("Good afternoon!"),
        IntentClass::Conversational
    );
    assert_eq!(classify_intent("Thank you!"), IntentClass::Conversational);
    assert_eq!(classify_intent("ok thanks"), IntentClass::Conversational);
    assert_eq!(classify_intent("see ya"), IntentClass::Conversational);
}

// =====================================================================
// Section 17: IntentClass enum properties
// =====================================================================

#[test]
fn intent_class_debug_display() {
    assert_eq!(
        format!("{:?}", IntentClass::Conversational),
        "Conversational"
    );
    assert_eq!(format!("{:?}", IntentClass::Research), "Research");
    assert_eq!(
        format!("{:?}", IntentClass::Implementation),
        "Implementation"
    );
}

#[test]
fn intent_class_clone_and_copy() {
    let a = IntentClass::Research;
    let b = a;
    let c = a;
    assert_eq!(a, b);
    assert_eq!(a, c);
}

#[test]
fn intent_class_equality() {
    assert_eq!(IntentClass::Conversational, IntentClass::Conversational);
    assert_ne!(IntentClass::Conversational, IntentClass::Research);
    assert_ne!(IntentClass::Research, IntentClass::Implementation);
}
