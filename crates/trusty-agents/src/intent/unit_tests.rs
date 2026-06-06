//! Unit tests for `intent` classification (extracted from `mod.rs`, #366).
//!
//! Why: Keeps `intent/mod.rs` under the 500-line cap while preserving the
//! in-module test access (`#[path]` keeps `super::*` resolving to `intent`).
//! What: Direct classifier assertions over representative prompts.
//! Test: This module is itself the test coverage.

use super::*;

#[test]
fn empty_input_is_conversational() {
    assert_eq!(classify_intent(""), IntentClass::Conversational);
    assert_eq!(classify_intent("   "), IntentClass::Conversational);
    assert_eq!(classify_intent("\n\t  "), IntentClass::Conversational);
}

#[test]
fn pure_greetings_are_conversational() {
    assert_eq!(classify_intent("Hello"), IntentClass::Conversational);
    assert_eq!(classify_intent("hi"), IntentClass::Conversational);
    assert_eq!(classify_intent("Hey there"), IntentClass::Conversational);
    assert_eq!(classify_intent("Howdy!"), IntentClass::Conversational);
    assert_eq!(classify_intent("Good morning"), IntentClass::Conversational);
    assert_eq!(classify_intent("yo"), IntentClass::Conversational);
}

#[test]
fn greetings_with_punctuation_are_conversational() {
    assert_eq!(classify_intent("Hello!"), IntentClass::Conversational);
    assert_eq!(classify_intent("hi."), IntentClass::Conversational);
    assert_eq!(classify_intent("Hey,"), IntentClass::Conversational);
    assert_eq!(classify_intent("Hello!!!"), IntentClass::Conversational);
}

#[test]
fn closings_are_conversational() {
    assert_eq!(classify_intent("Thanks!"), IntentClass::Conversational);
    assert_eq!(classify_intent("Bye"), IntentClass::Conversational);
    assert_eq!(classify_intent("Thank you"), IntentClass::Conversational);
    assert_eq!(classify_intent("cheers"), IntentClass::Conversational);
    assert_eq!(classify_intent("later"), IntentClass::Conversational);
}

#[test]
fn self_questions_are_conversational() {
    assert_eq!(
        classify_intent("What can you do?"),
        IntentClass::Conversational
    );
    assert_eq!(classify_intent("How are you?"), IntentClass::Conversational);
    assert_eq!(classify_intent("Who are you"), IntentClass::Conversational);
    assert_eq!(
        classify_intent("what is trusty-agents"),
        IntentClass::Conversational
    );
}

#[test]
fn action_verbs_signal_implementation() {
    assert_eq!(
        classify_intent("Write a Python script"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Fix the bug in main.rs"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Run the tests"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Build a markdown table formatter"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Implement intent classification"),
        IntentClass::Implementation
    );
}

#[test]
fn slash_commands_are_implementation() {
    assert_eq!(classify_intent("/help"), IntentClass::Implementation);
    assert_eq!(
        classify_intent("/connect /tmp/foo"),
        IntentClass::Implementation
    );
    assert_eq!(classify_intent("/status"), IntentClass::Implementation);
}

#[test]
fn greeting_plus_task_is_implementation() {
    // Action verb wins over greeting prefix.
    assert_eq!(
        classify_intent("hi, can you write a script that adds two numbers?"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Hello, please fix the failing test in src/main.rs"),
        IntentClass::Implementation
    );
}

#[test]
fn single_ambiguous_word_is_conversational() {
    // "yes", "ok", "cool" — short, no verb -> conversational.
    assert_eq!(classify_intent("yes"), IntentClass::Conversational);
    assert_eq!(classify_intent("ok"), IntentClass::Conversational);
    assert_eq!(classify_intent("cool"), IntentClass::Conversational);
}

#[test]
fn long_descriptive_input_routes_to_implementation() {
    // > 10 words, no action verb, but clearly a request for work.
    let long = "the failing integration test for the auth middleware on staging \
                seems related to the recent token refresh changes from last week";
    assert_eq!(classify_intent(long), IntentClass::Implementation);
}

#[test]
fn help_me_is_implementation() {
    assert_eq!(
        classify_intent("help me debug this issue"),
        IntentClass::Implementation
    );
}

#[test]
fn case_insensitive() {
    assert_eq!(classify_intent("HELLO"), IntentClass::Conversational);
    assert_eq!(
        classify_intent("WRITE A SCRIPT"),
        IntentClass::Implementation
    );
}

#[test]
fn punctuation_only_is_conversational() {
    assert_eq!(classify_intent("???"), IntentClass::Conversational);
    assert_eq!(classify_intent("..."), IntentClass::Conversational);
}

#[test]
fn search_verb_is_implementation() {
    assert_eq!(
        classify_intent("search the codebase for TODO"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("find all uses of delegate_to_agent"),
        IntentClass::Implementation
    );
}

// ---- Research-class tests (#203) ----

#[test]
fn question_words_signal_research() {
    assert_eq!(
        classify_intent("what does run_pm_task_with_session do"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("how does the intent classifier work"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("why is the server not starting"),
        IntentClass::Research
    );
}

#[test]
fn research_verbs_signal_research() {
    assert_eq!(
        classify_intent("explain the architecture"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("review the authentication code"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("analyze the performance bottleneck"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("describe how sessions work"),
        IntentClass::Research
    );
}

#[test]
fn short_question_mark_is_research() {
    assert_eq!(
        classify_intent("is bedrock enabled?"),
        IntentClass::Research
    );
}

#[test]
fn action_verb_wins_over_question_word() {
    // "how do I fix this bug" — starts with "how" but contains "fix"
    // (action verb) -> Implementation.
    assert_eq!(
        classify_intent("how do I fix this bug"),
        IntentClass::Implementation
    );
}

#[test]
fn action_verb_wins_over_research_verb() {
    // "explain how to write a test" — contains both "explain" (research)
    // and "write" (action) -> Implementation.
    assert_eq!(
        classify_intent("explain how to write a test"),
        IntentClass::Implementation
    );
}

#[test]
fn write_a_review_is_implementation() {
    // "write" is an action verb even though "review" is a research verb.
    assert_eq!(
        classify_intent("write a review"),
        IntentClass::Implementation
    );
}
