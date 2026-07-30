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
// Section 9: Priority rules — ONLY hard verbs (route::TCODE_HARD_VERBS:
// fix/debug/implement/refactor) win over everything. A plain action verb
// no longer does (code-critic CRITICAL follow-up, 2026-07-29) — it loses to
// a leading question word, a research verb, and even a generic
// technical-context word (which is AMBIGUOUS, not unambiguous evidence).
// =====================================================================

#[test]
fn hard_verb_wins_over_question_word() {
    // "fix" is a hard verb (route::TCODE_HARD_VERBS) -> always wins, even
    // over the leading question word "how".
    assert_eq!(
        classify_intent("how do I fix this bug"),
        IntentClass::Implementation
    );
}

#[test]
fn plain_verb_loses_to_question_word_and_research_verb() {
    // Code-critic CRITICAL follow-up (2026-07-29): a PLAIN verb ("write",
    // "deploy") no longer wins over a leading question word or a research
    // verb — question-word/research-verb detection is checked BEFORE the
    // unambiguous-technical-signal gate now, precisely so a genuine
    // question ("what does X do") lands on Research rather than a
    // coincidental lexical match forcing Implementation. See
    // `classifier_property_tests::plain_action_verbs_never_reach_implementation_from_context_alone`
    // for the full verb x template matrix.
    assert_eq!(
        classify_intent("what should I write here"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("where should I deploy this"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("explain how to write a test"),
        IntentClass::Research
    );
}

#[test]
fn hard_verb_wins_over_research_verb() {
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
fn only_hard_verb_wins_over_greeting_prefix() {
    // "script" is an AMBIGUOUS context word (Research-only) and this
    // sentence ends in "?", so it lands on Research now, not Implementation
    // — "hi, can you ...?" is a question, not a command.
    assert_eq!(
        classify_intent("hi, can you write a script that adds two numbers?"),
        IntentClass::Research
    );
    // "fix" is a hard verb -> always wins, even over the greeting prefix
    // (also has a repo-file token, doubly confirming Implementation).
    assert_eq!(
        classify_intent("Hello, please fix the failing test in src/main.rs"),
        IntentClass::Implementation
    );
    // "run" is plain and "tests" is an AMBIGUOUS context word now (the
    // tiny "tests"/"release" Implementation exception was deleted — #4319
    // code-critic CRITICAL third follow-up: it reopened the same crash
    // class on "check my blood tests"/"check the release date of the
    // movie") -> Research, not Implementation.
    assert_eq!(classify_intent("hey, run the tests"), IntentClass::Research);
}

#[test]
fn write_a_review_is_research_not_implementation() {
    // "review" is a RESEARCH_VERBS entry, checked before any
    // context/artifact-word logic — a plain verb ("write") no longer wins
    // over it (code-critic CRITICAL follow-up, 2026-07-29).
    assert_eq!(classify_intent("write a review"), IntentClass::Research);
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
    // #4319: pins the direction every narrowing pass on this classifier
    // must NOT regress — a real coding request must still classify
    // Implementation (here via the hard verb "fix", `route::TCODE_HARD_VERBS`)
    // AND still route to Tcode once handed to `route_task` (the deterministic
    // router `dispatch_task` uses once something is already headed to a
    // backend — see `intent::route`). Uses a sentence carrying both a hard
    // verb and a repo-file token, so both stages are exercised together end
    // to end, unaffected by any of the ambiguity-narrowing fixes above.
    let task = "fix the failing test in src/auth_middleware.rs";
    assert_eq!(classify_intent(task), IntentClass::Implementation);
    assert_eq!(
        crate::intent::route::route_task(task),
        crate::intent::route::BridgeRoute::Tcode
    );
}

// =====================================================================
// Owner-approved #4319 follow-up (2026-07-29): the four decision buckets,
// pinned together with the owner's verbatim sentences, since the first
// #4319 fix pass only pinned bucket 3 and left buckets 1/2 as a real,
// live gap ("can you check if it's raining tomorrow" and "I'll run by the
// store after work" both still crashed the subprocess pipeline).
// =====================================================================

#[test]
fn bucket_1_casual_phrasing_with_action_verb_is_not_implementation() {
    // "check" and "run" are both PLAIN ACTION_VERBS entries (common in
    // ordinary conversation with a non-coding meaning) with no technical
    // context anywhere in either sentence.
    assert_ne!(
        classify_intent("can you check if it's raining tomorrow"),
        IntentClass::Implementation
    );
    assert_ne!(
        classify_intent("I'll run by the store after work"),
        IntentClass::Implementation
    );
    // Specific landing spots: "can you check ..." starts with the question
    // word "can" -> Research; "I'll run by ..." has no signal left once the
    // verb-alone gate doesn't fire -> Conversational.
    assert_eq!(
        classify_intent("can you check if it's raining tomorrow"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("I'll run by the store after work"),
        IntentClass::Conversational
    );
}

#[test]
fn bucket_2_short_imperative_coding_request_is_research_but_can_still_reach_tcode() {
    // #4319 code-critic CRITICAL third follow-up (2026-07-29): "run the
    // tests" / "build the release" no longer classify Implementation. The
    // tiny "plain verb + tests/release" exception that used to grant them
    // Implementation was deleted — it was proven, by the same
    // execute-the-classifier method, to reopen the crash class one level
    // down ("check my blood tests", "check the release date of the movie"
    // are ordinary English that also hit that exception). Both verbs are
    // PLAIN (not `route::TCODE_HARD_VERBS`) and neither sentence has any
    // OTHER unambiguous signal (no file token, no snake_case identifier, no
    // error/stack-trace marker), so both now correctly land on Research.
    for task in ["run the tests", "build the release"] {
        assert_eq!(
            classify_intent(task),
            IntentClass::Research,
            "'{task}' should be Research, not Implementation"
        );
    }
    // Verified end-to-end (not assumed): a Research classification does NOT
    // lose the ability to reach trusty-code. `dispatch_task` (`PmBridgeTool`)
    // is registered unconditionally in
    // `ctrl::pm_task::dispatch::history::run_pm_task_with_history`'s
    // tool-armed loop for ANY non-Conversational classification — that
    // function's only intent-based branch is a Conversational-only
    // fast-path skip, so Research and Implementation were ALREADY
    // tool-equivalent there before this change. The PM can still choose to
    // call `dispatch_task("run the tests")` when it judges the work
    // warrants it; `route_task` then applies its own independent
    // Tm-vs-Tcode signal detection to that call.
    //
    // Code-critic HIGH correction (2026-07-29): `route_task` carries its OWN
    // route.rs-LOCAL "tests"/"release" exception
    // (`RUN_BUILD_TCODE_ARTIFACT_WORDS` — never shared with
    // `classify_intent`, which stays zero-exception), restored after the
    // owner's non-negotiable "route to Tcode" requirement for these two
    // exact phrasings was found to have never actually been retracted. So
    // both still resolve to `Tcode`, not the `Tm` default.
    assert_eq!(
        crate::intent::route::route_task("run the tests"),
        crate::intent::route::BridgeRoute::Tcode
    );
    assert_eq!(
        crate::intent::route::route_task("build the release"),
        crate::intent::route::BridgeRoute::Tcode
    );
}

#[test]
fn bucket_3_verbless_bug_report_with_domain_cues_is_research() {
    // The two #4319 HIGH-1 proven regression sentences, verbatim.
    let sentence_1 = "the login page has been broken on mobile safari for the past two days \
                       and none of my customers can complete checkout";
    let sentence_2 = "the situation with the auth middleware on staging \
                       seems related to the recent token refresh changes from last week";
    for s in [sentence_1, sentence_2] {
        assert_eq!(
            classify_intent(s),
            IntentClass::Research,
            "'{s}' should be Research"
        );
    }
}

#[test]
fn bucket_4_signal_free_conversation_is_conversational() {
    assert_eq!(
        classify_intent("that new library seems pretty nice honestly"),
        IntentClass::Conversational
    );
    assert_eq!(
        classify_intent(
            "please confirm that the research agent you mentioned earlier is actually available today"
        ),
        IntentClass::Conversational
    );
}

#[test]
fn code_critic_critical_ordinary_nouns_never_reach_implementation() {
    // #4319 code-critic CRITICAL (2026-07-29): these 14 sentences, verbatim
    // from the critic's reports, all returned Implementation under a
    // now-superseded design and reached `task_runner.rs`'s literal
    // `subprocess exited with status {:?}` on completely ordinary
    // conversation.
    //
    // The first 12 pair a plain ACTION_VERBS verb with a common polysemous
    // English noun (token, production, staging, credentials, session,
    // queue, container, timeout, exceptions, incident, config) that also
    // happens to be a legitimate technical term — proving that "plain verb
    // + generic context word" cannot be unambiguous evidence for
    // Implementation, only for Research (see `TECHNICAL_CONTEXT_WORDS`'s
    // doc comment for the inverted-default fix this pins).
    //
    // The last 2 ("check my blood tests", "check the release date of the
    // movie") were added after a NARROWER version of that same bug was found
    // one level down: a since-deleted tiny "plain verb + tests/release"
    // exception (kept only for "run the tests"/"build the release") also
    // let ordinary sentences using "tests"/"release" in their everyday
    // (medical exam / film release) sense reach Implementation. See
    // `has_unambiguous_technical_signal`'s doc comment for why that
    // exception was deleted outright rather than narrowed further.
    let ordinary_sentences_with_technical_sounding_nouns = [
        "check my token balance",
        "check the production schedule for the play",
        "check the staging area before the wedding",
        "update my credentials at the DMV",
        "find my token for the parking meter",
        "check my session times for yoga",
        "add me to the queue at the deli",
        "check the container garden this weekend",
        "check the timeout rule in basketball",
        "list the exceptions to the dress code",
        "check the incident report from the fender bender",
        "delete the old config from my calendar invite",
        "check my blood tests",
        "check the release date of the movie",
    ];
    for s in ordinary_sentences_with_technical_sounding_nouns {
        assert_ne!(
            classify_intent(s),
            IntentClass::Implementation,
            "'{s}' must NEVER reach Implementation — this is the exact crash class #4319 exists to eliminate"
        );
    }
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
fn help_me_reaches_implementation_only_via_a_hard_verb() {
    // "debug" is a hard verb -> Implementation, with no "help me" special
    // case needed at all.
    assert_eq!(
        classify_intent("help me debug this issue"),
        IntentClass::Implementation
    );
    // Code-critic CRITICAL fourth follow-up (2026-07-29): bare "help me"
    // (and "help me <anything signal-free>") used to be an UNCONDITIONAL
    // `Implementation` special case — pre-existing on `origin/main`, not
    // introduced by any of the #4319 fix rounds, and undetected for five of
    // them because the only prior test for this branch
    // ("help me debug this issue") independently satisfied the hard-verb
    // path too. Deleted outright (see `classify_intent`'s doc comment) —
    // nothing else in the crate keys off this prefix, and every genuine
    // "help me fix/debug/implement/refactor ..." request still reaches
    // Implementation via the hard-verb path with no special case at all.
    assert_eq!(classify_intent("help me"), IntentClass::Conversational);
}

#[test]
fn help_alone_is_conversational() {
    assert_eq!(classify_intent("help"), IntentClass::Conversational);
}

#[test]
fn help_question_mark_is_research() {
    assert_eq!(classify_intent("help?"), IntentClass::Research);
}

#[test]
fn code_critic_critical_help_me_phrases_never_reach_implementation() {
    // #4319 code-critic CRITICAL fourth follow-up (2026-07-29): these 13
    // phrases, verbatim from the critic's report, all classified
    // Implementation under the pre-existing (not introduced by any #4319
    // fix round) unconditional "help me " prefix special case, reaching
    // `handlers.rs`'s subprocess-spawn `run_task` and the literal
    // `subprocess exited with status Some(1)` crash on completely ordinary
    // requests for assistance. None of these carry a hard verb, a repo-file
    // token, a snake_case identifier, or an error/stack-trace marker, so
    // none should ever reach Implementation.
    let ordinary_help_me_phrases = [
        "help me plan my week",
        "help me decide what to eat",
        "help me relax",
        "help me sleep",
        "help me feel better",
        "help me write a poem",
        "help me pick a name",
        "help me get ready for the party",
        "help me pack for my trip",
        "help me choose a gift",
        "help me with my homework",
        "help me think through this decision",
        "help me calm down",
    ];
    for s in ordinary_help_me_phrases {
        assert_ne!(
            classify_intent(s),
            IntentClass::Implementation,
            "'{s}' must NEVER reach Implementation — this is the exact crash class #4319 exists to eliminate"
        );
    }
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
    // "script" is an AMBIGUOUS context word (Research-only, not an
    // unambiguous signal) — code-critic CRITICAL follow-up, 2026-07-29.
    assert_eq!(classify_intent("WRITE A SCRIPT"), IntentClass::Research);
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
    // "script" is ambiguous context (Research-only); normalization strips
    // the dots into whitespace either way, so this pins that punctuation
    // handling doesn't change the (now Research) outcome.
    assert_eq!(classify_intent("Write...a...script"), IntentClass::Research);
}

#[test]
fn unicode_lowercasing() {
    assert_eq!(classify_intent("GRÜßE"), IntentClass::Conversational);
}

#[test]
fn tabs_and_newlines_treated_as_whitespace() {
    assert_eq!(classify_intent("write\ta\nscript"), IntentClass::Research);
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
    // Code-critic CRITICAL follow-up (2026-07-29): "script"/"api"/
    // "endpoint"/"staging" are all AMBIGUOUS context words now
    // (Research-only) — a plain verb ("write"/"create"/"deploy") plus one
    // of them is no longer sufficient for Implementation. Only "refactor"
    // (a hard verb) still reaches Implementation unconditionally.
    assert_eq!(
        classify_intent("Write a Python script that formats data as a markdown table"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("Create a REST API endpoint for user registration"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("Refactor the database module to use connection pooling"),
        IntentClass::Implementation
    );
    assert_eq!(
        classify_intent("Deploy the staging environment"),
        IntentClass::Research
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
