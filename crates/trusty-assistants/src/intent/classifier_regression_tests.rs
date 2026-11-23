//! Comprehensive classifier tests for `src/intent/mod.rs` (part 2 of 2).
//!
//! Why: Split from `classifier_tests.rs` per #366 to keep each test file under
//! the 500-line cap; wired via `#[path]` from `intent/mod.rs` so `super::*`
//! still resolves to the `intent` module. Renamed from `classifier_tests_2.rs`
//! (#4319, round 7) so `scripts/check_line_cap.sh`'s `*_tests.rs` test-file
//! classification applies (3000 SLOC cap) — this file is 100% `#[cfg(test)]`
//! content and had crossed the 500-SLOC production cap after the round-7
//! regression-test additions; the `_2` suffix in the old name was an
//! oversight from the original #366 split that excluded it from the same
//! test-file exemption its siblings (`classifier_tests.rs`,
//! `classifier_property_tests.rs`, `unit_tests.rs`) already had.
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
// Section 9: Priority rules — #4319 OWNER DECISION (2026-07-29, final
// iteration): NO verb, hard or plain, wins over anything anymore. A leading
// question word or a research verb always wins over ANY verb (hard or
// plain) because they're checked first; a verb plus a
// `TECHNICAL_CONTEXT_WORDS` co-occurrence is Research, never Implementation
// (round 6 proved that combination still crashes on ordinary sentences that
// use the SAME words already in the list — see
// `hard_verb_polysemy_with_everyday_sense_context_word_is_research_not_implementation`
// below).
// =====================================================================

#[test]
fn leading_question_word_beats_any_verb_even_with_corroboration() {
    // "fix" + "bug" (a TECHNICAL_CONTEXT_WORDS entry) used to win over the
    // leading question word "how" under the round-6 hard-verb-corroboration
    // design. Round 7 deleted that path entirely — the leading question
    // word is checked BEFORE any technical-signal logic and no verb ever
    // reaches Implementation, so this now lands on Research.
    assert_eq!(
        classify_intent("how do I fix this bug"),
        IntentClass::Research
    );
}

#[test]
fn plain_verb_loses_to_question_word_and_research_verb() {
    // A verb ("write", "deploy") never wins over a leading question word or
    // a research verb — question-word/research-verb detection is checked
    // BEFORE the unambiguous-technical-signal gate, precisely so a genuine
    // question ("what does X do") lands on Research rather than a
    // coincidental lexical match forcing Implementation.
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
fn verb_plus_context_word_never_beats_research_verb() {
    // #4319 OWNER DECISION (2026-07-29, final iteration): these two used to
    // pin "a corroborated hard verb wins over a research verb in the same
    // sentence" (round 5/6). That property no longer exists — no verb ever
    // reaches Implementation, so the research verb ("review"/"analyze")
    // determines Research regardless of what else is in the sentence.
    assert_eq!(
        classify_intent("review and fix the login bug"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("analyze then refactor the auth module"),
        IntentClass::Research
    );
    // Same outcome without any TECHNICAL_CONTEXT_WORDS co-occurrence at
    // all — the research verb alone already guarantees Research.
    assert_eq!(
        classify_intent("review and fix the code"),
        IntentClass::Research
    );
    assert_eq!(
        classify_intent("analyze then refactor the module"),
        IntentClass::Research
    );
}

#[test]
fn unambiguous_signal_wins_over_greeting_prefix_regardless_of_verb() {
    // "script" is an AMBIGUOUS context word (Research-only) and this
    // sentence ends in "?", so it lands on Research — "hi, can you ...?" is
    // a question, not a command.
    assert_eq!(
        classify_intent("hi, can you write a script that adds two numbers?"),
        IntentClass::Research
    );
    // Wins over the greeting prefix because of the repo-file token
    // "src/main.rs" (`has_unambiguous_technical_signal`) — NOT because
    // "fix" is special; no verb of any kind carries that weight anymore.
    assert_eq!(
        classify_intent("Hello, please fix the failing test in src/main.rs"),
        IntentClass::Implementation
    );
    // "run" is plain and "tests" is an AMBIGUOUS context word (the tiny
    // "tests"/"release" Implementation exception was deleted — #4319
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

#[test]
fn owner_rescinded_hard_verb_gate_must_work_cases_updated_for_round_7() {
    // #4319 OWNER DECISION (2026-07-29, final iteration — seventh
    // follow-up): the four sentences code-critic's fifth follow-up required
    // as Implementation for the (now-deleted) hard-verb-corroboration gate.
    // The owner has FORMALLY RESCINDED that requirement for the two that
    // depended on a word list — round 6 proved the gate they satisfied is
    // structurally unsafe. The two that depend on a syntactic signal
    // (repo-file token, leading slash) are UNCHANGED.
    //
    // - "fix the login bug": "bug" is only a TECHNICAL_CONTEXT_WORDS entry
    //   (no repo-file token, no snake_case, no error marker) -> now Research
    //   (verb + context word), a DELIBERATE contract change, not a
    //   regression.
    assert_eq!(classify_intent("fix the login bug"), IntentClass::Research);
    // - "/fix the thing": leading slash, unaffected — still Implementation.
    assert_eq!(
        classify_intent("/fix the thing"),
        IntentClass::Implementation
    );
    // - "debug the auth middleware": "auth"/"middleware" are only
    //   TECHNICAL_CONTEXT_WORDS entries -> now Research, a DELIBERATE
    //   contract change.
    assert_eq!(
        classify_intent("debug the auth middleware"),
        IntentClass::Research
    );
    // - "fix main.rs": ".rs" is a repo-file extension ->
    //   has_unambiguous_technical_signal — still Implementation, unaffected
    //   by the word-list deletion.
    assert_eq!(classify_intent("fix main.rs"), IntentClass::Implementation);
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
fn help_me_never_reaches_implementation_from_a_verb_alone() {
    // #4319 OWNER DECISION (2026-07-29, final iteration): "debug" carries no
    // special status anymore. "issue" is a TECHNICAL_CONTEXT_WORDS entry, so
    // this lands on Research (verb + context word), not Implementation.
    assert_eq!(
        classify_intent("help me debug this issue"),
        IntentClass::Research
    );
    // Code-critic CRITICAL fourth follow-up (2026-07-29): bare "help me"
    // (and "help me <anything signal-free>") used to be an UNCONDITIONAL
    // `Implementation` special case — pre-existing on `origin/main`, not
    // introduced by any of the #4319 fix rounds. Deleted outright (see
    // `classify_intent`'s doc comment) — nothing else in the crate keys off
    // this prefix.
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

#[test]
fn hard_verb_polysemy_with_no_context_word_is_conversational() {
    // #4319 code-critic CRITICAL fifth follow-up (2026-07-29), verbatim:
    // these 9 phrases pair a hard verb with a genuinely non-technical
    // object carrying NO `TECHNICAL_CONTEXT_WORDS` token at all —
    // fix/debug/implement/refactor are polysemous in ordinary English (fix
    // a drink, fix my hair, fix breakfast, fix me up on a date; debug
    // meaning troubleshoot a feeling; implement meaning adopt a habit;
    // refactor meaning reorganize a non-code thing). Under the round-6
    // design (a bare hard verb won unconditionally) all 9 crashed to
    // Implementation. Round 7 deletes every verb-based path to
    // Implementation entirely, so — with no context word present either —
    // these fall all the way through to the Conversational default.
    let hard_verb_polysemy_phrases = [
        "fix a drink",
        "fix my hair",
        "fix breakfast",
        "fix me up with your friend",
        "debug why I feel anxious",
        "implement a new morning routine",
        "implement better habits",
        "refactor my schedule",
        "refactor my life",
    ];
    for s in hard_verb_polysemy_phrases {
        assert_eq!(
            classify_intent(s),
            IntentClass::Conversational,
            "'{s}' must NEVER reach Implementation, and lands on Conversational (no context word present)"
        );
    }
}

#[test]
fn hard_verb_polysemy_with_everyday_sense_context_word_is_research_not_implementation() {
    // #4319 code-critic CRITICAL SIXTH follow-up (2026-07-29): the round-6
    // design ("hard verb + TECHNICAL_CONTEXT_WORDS co-occurrence" ->
    // Implementation) was disproven by these ordinary sentences — the
    // first 5 verbatim from the critic's report, the rest constructed by
    // the same method (pairing a hard verb with an object that happens to
    // contain a TECHNICAL_CONTEXT_WORDS token in its everyday, non-software
    // sense) to cover the list systematically rather than narrowly. The
    // verb+context-word rule cannot distinguish technical from everyday
    // sense — that's exactly why it grants only `Research`, never
    // `Implementation` (see `TECHNICAL_CONTEXT_WORDS`'s doc comment). None
    // of these 29 sentences (this test's 24 plus the 5-phrase bare-verb set
    // above rounds out the full regression) should ever reach
    // Implementation; these 24 land on Research specifically, since each
    // carries a genuine action-verb + context-word pairing.
    let phrases = [
        "fix my gym session",
        "debug the incident report from the fender bender",
        "fix the queue at the deli counter",
        "refactor the outage in our friendship",
        "debug my unresponsive teenager",
        "fix the timeout on my microwave",
        "debug the cache of old receipts in my wallet",
        "implement a better config for my morning routine",
        "refactor the script for the school play",
        "fix the container of leftovers in the fridge",
        "debug the database of family recipes in the cookbook",
        "fix the credentials dispute with my landlord",
        "implement a certificate ceremony for the kids' graduation",
        "refactor the pipeline of dishes piling up in the sink",
        "fix the token my kid uses for the arcade",
        "debug the server at the restaurant who forgot our order",
        "implement the deployment of lawn chairs for the barbecue",
        "fix the middleware seat mix-up on our flight booking",
        "refactor the frontend yard landscaping",
        "debug the backend room of the antique shop",
        "fix the production of the school musical",
        "implement the staging of furniture before the open house",
        "fix the mix-up with my blood tests appointment",
        "fix the release date mix-up for the movie premiere",
        "refactor the regression line on my daughter's science fair poster",
        "fix the issue with my neighbor over the fence line",
        "debug the exceptions my toddler makes to the bedtime rule",
        "refactor the codebase of grandma's recipe box",
        "fix the bugs in my garden",
        "debug the crash of my toy drone",
    ];
    for s in phrases {
        assert_ne!(
            classify_intent(s),
            IntentClass::Implementation,
            "'{s}' must NEVER reach Implementation — the exact crash class round 6 reopened"
        );
        assert_eq!(
            classify_intent(s),
            IntentClass::Research,
            "'{s}' should land on Research (ambiguous verb + context word)"
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
    // #4319 OWNER DECISION (2026-07-29, final iteration): "script"/"api"/
    // "endpoint"/"staging"/"database" are all AMBIGUOUS context words
    // (Research-only) — a verb ("write"/"create"/"deploy"/"refactor") plus
    // one of them is no longer sufficient for Implementation. No verb has
    // special status anymore, including "refactor" — round 6 proved
    // "refactor" + a TECHNICAL_CONTEXT_WORDS word ("database") is exactly
    // the exploitable combination (see
    // `hard_verb_polysemy_with_everyday_sense_context_word_is_research_not_implementation`).
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
        IntentClass::Research
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
