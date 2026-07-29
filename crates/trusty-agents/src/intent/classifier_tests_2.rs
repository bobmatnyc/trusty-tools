//! Comprehensive classifier tests for `src/intent/mod.rs` (part 2 of 2).
//!
//! Why: Split from `classifier_tests.rs` per #366 to keep each test file under
//! the 500-line cap; wired via `#[path]` from `intent/mod.rs` so `super::*`
//! still resolves to the `intent` module.
//! Test: This module is itself part of the classifier test coverage.

use super::*;

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
    // #4319: a trailing "?" is evidence of a question at ANY length, not
    // evidence of a coding command. Previously the (now-removed) `word_count
    // > 10` fallback won here and misrouted this to Implementation.
    let long_q = "so I was wondering about the overall performance characteristics \
                  of the system under heavy load with many concurrent users?";
    assert_eq!(classify_intent(long_q), IntentClass::Research);
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
fn eleven_plus_words_with_domain_cue_is_research_not_implementation() {
    // #4319 regression, corrected after code-critic HIGH-1: length alone
    // must never promote to Implementation — but a verb-less BUG REPORT
    // must also never silently drop all the way to Conversational, which
    // is what this exact sentence did under the first #4319 fix pass
    // (empirically verified by code-critic against pre/post binaries: this
    // is a paraphrase of a real bug report with "failing test" replaced by
    // "situation", removing the only signal the first fix pass checked
    // for). It carries domain/incident vocabulary ("auth", "middleware",
    // "staging", "token") that `has_bug_report_signal` now catches, so it
    // lands on Research: in-process, tool-armed, no subprocess, PM decides.
    let long = "the situation with the auth middleware on staging \
                seems related to the recent token refresh changes from last week";
    assert!(long.split_whitespace().count() > 10);
    assert_eq!(classify_intent(long), IntentClass::Research);
}

#[test]
fn verbless_bug_report_login_broken_is_research_not_implementation_or_conversational() {
    // #4319 code-critic HIGH-1 proven regression #2 (verbatim): a verb-less
    // bug report with no action verb, no research verb, no leading question
    // word, and no trailing "?" — but with the incident word "broken".
    // Previously (first #4319 fix pass) this dropped to Conversational,
    // which is worse than the original bug: a genuine coding request
    // answered as idle chat with nothing to signal it happened.
    let reproducer = "the login page has been broken on mobile safari for the past two days \
                       and none of my customers can complete checkout";
    assert!(reproducer.split_whitespace().count() > 10);
    assert_ne!(classify_intent(reproducer), IntentClass::Implementation);
    assert_ne!(classify_intent(reproducer), IntentClass::Conversational);
    assert_eq!(classify_intent(reproducer), IntentClass::Research);
}

#[test]
fn issue_4319_reproducer_long_conversational_check_in_is_not_implementation() {
    // Live reproducer from #4319: an ordinary conversational check-in,
    // longer than 10 words, with no action verb, no research verb, no
    // leading question word, no trailing "?", and no bug-report/domain
    // cue. Previously this fell through to `IntentClass::Implementation`,
    // which respawns the orchestrator as a subprocess — reproduced live as
    // the literal string `subprocess exited with status Some(1)` surfacing
    // as the assistant's chat reply on Concierge, Telegram, and Slack (all
    // route through `ctrl::run_pm_task_with_history`).
    let reproducer =
        "please confirm that the research agent you mentioned earlier is actually available today";
    assert!(reproducer.split_whitespace().count() > 10);
    assert_ne!(classify_intent(reproducer), IntentClass::Implementation);
    assert_eq!(classify_intent(reproducer), IntentClass::Conversational);
}

#[test]
fn genuine_coding_request_still_reaches_implementation_and_tcode() {
    // #4319 code-critic HIGH-1: pins the direction the fallback narrowing
    // must NOT regress — a real coding request must still classify
    // Implementation (via the unconditional-on-length ACTION_VERBS path)
    // AND still route to Tcode once handed to `route_task` (the deterministic
    // router `dispatch_task` uses once something is already headed to a
    // backend — see `intent::route`). Uses a sentence carrying both an
    // action verb and a repo-file token, so both stages are exercised
    // together end to end.
    let task = "fix the failing test in src/auth_middleware.rs";
    assert_eq!(classify_intent(task), IntentClass::Implementation);
    assert_eq!(
        crate::intent::route::route_task(task),
        crate::intent::route::BridgeRoute::Tcode
    );
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
        classify_intent("what is trusty-agents"),
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
    assert_eq!(normalize("trusty-agents"), "trusty-agents");
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
