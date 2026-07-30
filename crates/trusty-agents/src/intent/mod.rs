//! Intent classification for PM orchestrator fast-pathing.
//!
//! Why: The PM system prompt instructs "always use delegate_to_agent", which
//! means even trivial conversational inputs like "Hello" trigger sub-agent
//! spawning — a 60-90s round trip for what should be a sub-second reply.
//! Research questions ("explain X", "what does Y do") similarly don't need
//! the full prescriptive subprocess pipeline — they can run in-process with
//! tools. Classifying input cheaply (no network) lets the controller route
//! each intent to its lowest-cost path.
//! What: A pure-Rust heuristic classifier returning `IntentClass::Conversational`,
//! `IntentClass::Research`, or `IntentClass::Implementation`. No regex crate,
//! no LLM — just lowercased string matching and word-count gates. Slash
//! commands are always Implementation so the user can force the full pipeline.
//! Test: `cargo test intent::` exercises greetings, closings, self-questions,
//! research verbs, question words, action-verb tasks, and edge cases.
//! See `tests` module below — fixes #199, #203, #4319.

/// Deterministic Tm-vs-Tcode router for the `dispatch_task` bridge tool
/// (epic #3052, PR B, lane 3).
///
/// Why: a separate, focused module rather than folding into
/// `classify_intent` above — that classifier answers "how much machinery
/// does this input need" (conversational / research / implementation);
/// `route` answers an orthogonal question once the answer is already
/// "hand this off": WHICH black-boxed backend (orchestration vs direct
/// coding) should receive it.
/// What: see `route::route_task` / `route::BridgeRoute`.
/// Test: `route::route_tests`.
pub mod route;

/// Classification of user input for PM fast-pathing.
///
/// Why: Distinguishes conversational chatter (no work) from research questions
/// (in-process with tools) from implementation requests (full prescriptive
/// subprocess pipeline) so the controller routes each to its lowest-cost path.
/// What: `Conversational` -> reply directly, no tools.
/// `Research` -> in-process PM loop with `delegate_to_agent` available.
/// `Implementation` -> full subprocess prescriptive workflow.
/// Test: Pattern-match in `submit_task` + `run_pm_task_with_session`; covered
/// by `tests::*` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentClass {
    /// Greeting, thanks, or simple self-referential question — answer directly.
    Conversational,
    /// Research/explain/analyze — in-process PM loop with tools.
    Research,
    /// Action request — route through the full prescriptive subprocess pipeline.
    Implementation,
}

/// Action verbs that strongly indicate an implementation request.
///
/// Why: Centralizing the verb list as a constant keeps the classifier honest
/// — any change to "what counts as a task verb" lives in one place.
/// What: Lowercase verb tokens; matched as whole words against normalized input.
const ACTION_VERBS: &[&str] = &[
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

/// Research verbs that signal "explain / analyze / investigate" intent.
///
/// Why: Research questions don't need the prescriptive subprocess pipeline.
/// They benefit from PM's tool-armed in-process loop (delegate to a sub-agent
/// only when needed) for fast turnaround on read-only tasks.
/// What: Lowercase verb tokens; matched as whole words against normalized input.
/// Note: An ACTION_VERB elsewhere in the input wins over a research verb
/// (e.g. "explain how to fix this" -> Implementation, because "fix" is
/// concrete work).
const RESEARCH_VERBS: &[&str] = &[
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

/// Question words that signal an interrogative (research) intent.
///
/// Why: When input starts with a question word and lacks an action verb, it's
/// almost always a research question (e.g. "what does X do", "why is Y slow").
/// What: Lowercase tokens; matched only as the FIRST word of normalized input.
const QUESTION_WORDS: &[&str] = &[
    "what", "why", "how", "when", "where", "which", "who", "whose", "whom", "does", "is", "are",
    "can", "could", "would", "should",
];

/// Greeting prefixes that signal a conversational opener.
///
/// Why: Recognized as whole-message matches OR as the first word of a short
/// input. Kept as a constant so additions (e.g. "salutations") are a one-liner.
/// What: Lowercase, punctuation-stripped greeting tokens.
const GREETINGS: &[&str] = &[
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

/// Closing / gratitude phrases.
///
/// Why: Same rationale as `GREETINGS` — single source of truth.
const CLOSINGS: &[&str] = &[
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

/// Self-referential conversational questions.
///
/// Why: Users frequently probe "what can you do?" before delegating real work.
/// Answering directly (≤2 sentences from the PM) is faster than spawning an agent.
const SELF_QUESTIONS: &[&str] = &[
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

/// #4319 (code-critic HIGH-1, widened in the owner-approved 2026-07-29
/// follow-up): technical-context vocabulary not already covered by
/// `route::has_tcode_lexical_signal` — words that describe something being
/// WRONG (broken, failing, crash, error, issue, bug, regression, outage,
/// timeout, exception), name the technical subsystem it's wrong in
/// (middleware, backend, frontend, database, server, endpoint, api,
/// staging, production, auth, token, session, config, deployment,
/// credentials, certificate, pipeline, queue, cache, container), or name a
/// code artifact (script, test, tests, release, codebase).
///
/// Why: this list now serves TWO callers that both need "is this actually
/// about code" as a signal (`has_technical_context_signal` below, shared
/// rather than reimplemented per caller):
/// 1. The bug-report `Research` fallback (original #4319 HIGH-1): a
///    verb-less bug report — "the login page has been broken on mobile
///    safari for the past two days and none of my customers can complete
///    checkout", "the situation with the auth middleware on staging seems
///    related to the recent token refresh changes from last week" —
///    carries NEITHER an `ACTION_VERBS` word NOR any
///    `route::has_tcode_lexical_signal` hit (no file path, no `error:`
///    marker, no "stack trace"/"failing test" phrase). Both regressed to
///    `Conversational` under the first #4319 fix pass (code-critic
///    finding, verified empirically against pre/post binaries) — worse
///    than the original bug: a genuine coding request answered as idle
///    chat with nothing to signal it happened.
/// 2. Gating `ACTION_VERBS` (owner-approved follow-up): an `ACTION_VERBS`
///    hit ALONE is common in ordinary conversation with a non-coding
///    meaning ("can you check if it's raining tomorrow", "I'll run by the
///    store after work" both contain an `ACTION_VERBS` word) and must not
///    alone trigger the subprocess pipeline. "write a script"/"run the
///    tests"/"build the release" must still reach `Implementation`, so the
///    artifact nouns (`script`/`test`/`tests`/`release`/`codebase`) that
///    distinguish a real short coding command from casual phrasing using
///    the same verb are added here rather than to a second list.
/// What: matched via exact word-token equality against normalized input
/// (see `has_technical_context_signal`), same as
/// `ACTION_VERBS`/`RESEARCH_VERBS`. Deliberately biased toward false
/// positives in both roles: caller 1 only ever promotes to `Research`
/// (in-process, tool-armed, no subprocess); caller 2 only promotes an
/// ALREADY-present `ACTION_VERBS` hit to `Implementation` one step earlier
/// than it would otherwise resolve — misrouting ordinary technical-sounding
/// chatter here costs a slightly more expensive but still-safe path, not a
/// crash. Known corner case (owner-briefed, not silently swallowed): "test"
/// doubles as both an `ACTION_VERBS` entry (the verb "to test something")
/// and a `TECHNICAL_CONTEXT_WORDS` entry (the noun "a test"/"the tests"),
/// so a sentence combining an unrelated soft verb with the word "test" in
/// its exam/quiz sense (e.g. "check my test scores") would also read as
/// having technical context. Judged acceptable: false positives here land
/// on the safe `Research`/`Implementation` distinction the PM can still
/// correct, never on a crash, and this product's actual traffic is a
/// coding-assistant chat where "test" overwhelmingly means "a software
/// test."
const TECHNICAL_CONTEXT_WORDS: &[&str] = &[
    "broken",
    "failing",
    "fails",
    "crash",
    "crashed",
    "crashing",
    "error",
    "errors",
    "issue",
    "issues",
    "bug",
    "bugs",
    "regression",
    "outage",
    "incident",
    "unresponsive",
    "timeout",
    "timeouts",
    "exception",
    "exceptions",
    "middleware",
    "backend",
    "frontend",
    "database",
    "server",
    "endpoint",
    "api",
    "staging",
    "production",
    "auth",
    "authentication",
    "token",
    "session",
    "config",
    "configuration",
    "deployment",
    "credentials",
    "certificate",
    "pipeline",
    "queue",
    "cache",
    "container",
    "script",
    "test",
    "tests",
    "release",
    "codebase",
];

/// #4319: True when `input` describes a concrete technical situation — a
/// bug report, an incident, a real coding request — via lexical cues,
/// without requiring an `ACTION_VERBS` hit on its own to be sufficient
/// evidence. Combines `route::has_tcode_lexical_signal` (reused, not
/// reimplemented — the common-entry-point principle) with
/// `TECHNICAL_CONTEXT_WORDS` above, plus a general snake_case-identifier
/// check: a token containing `_` longer than 3 chars (e.g.
/// `delegate_to_agent`, `auth_middleware.rs`) names a real code symbol —
/// `normalize` already preserves underscores for exactly this reason (see
/// its own doc comment), so recognizing that preserved shape as a
/// technical-context signal reuses the SAME design decision rather than
/// adding a third detector.
/// Test: `classifier_tests_2::*` (bug-report and action-verb-plus-context
/// cases), `unit_tests::search_verb_is_implementation`.
fn has_technical_context_signal(input: &str, words: &[&str]) -> bool {
    route::has_tcode_lexical_signal(input)
        || words.iter().any(|w| TECHNICAL_CONTEXT_WORDS.contains(w))
        || words.iter().any(|w| w.len() > 3 && w.contains('_'))
}

/// Strip surrounding/embedded punctuation for matching.
///
/// Why: Users write "Hello!" / "hi." / "hey," — comparing to plain "hello"
/// requires normalization. We keep apostrophes (so "what's" stays whole)
/// and internal hyphens (so "trusty-agents" stays whole).
/// What: Lowercases and replaces ASCII punctuation (except `'`, `-`, `_`)
/// with spaces, then collapses runs of whitespace. Underscores are preserved
/// so identifiers like `run_pm_task_with_session` remain a single token
/// rather than fragmenting into "run" / "task" (which would falsely match
/// ACTION_VERBS).
/// Test: Covered indirectly by classifier tests — "Hello!!!" must classify
/// the same as "hello".
/// `pub(crate)` (not private) so `intent::route`'s `route_task` can reuse the
/// exact same normalization instead of forking a second copy (#3052 PR B).
pub(crate) fn normalize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_alphanumeric() || ch == '\'' || ch == '-' || ch == '_' || ch.is_whitespace() {
            for low in ch.to_lowercase() {
                out.push(low);
            }
        } else {
            out.push(' ');
        }
    }
    // Collapse whitespace.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Classify a user input string into a coarse intent class.
///
/// Why: Lets `submit_task` and `run_pm_task_with_session` route each input
/// to its cheapest viable path — direct reply (Conversational), in-process
/// tool-armed loop (Research), or full subprocess pipeline (Implementation).
/// What: Applies heuristics in priority order — empty, slash command, greeting,
/// closing, self-question, hard-action-verb scan (wins outright, any length —
/// `route::TCODE_HARD_VERBS`: fix/debug/implement/refactor), soft-action-verb
/// scan (wins only WITH a technical-context signal — see
/// `has_technical_context_signal`), research-verb/question-word scan, trailing
/// question mark, "help me", technical-context-signal-alone (Research), then
/// defaults to `Conversational` when no positive signal fired. Word count
/// alone is NEVER evidence for `Implementation` (#4319); a soft `ACTION_VERBS`
/// hit ALONE isn't either (owner-approved 2026-07-29 follow-up) — only a hard
/// verb, a soft verb plus context, or a leading slash routes there.
/// Test: `tests::*` below covers greetings, closings, self-questions, research
/// verbs, question words, clear task verbs, slash commands, the #4319
/// long-conversational-input regression, and edge cases.
pub fn classify_intent(input: &str) -> IntentClass {
    let trimmed = input.trim();

    // Empty / whitespace-only -> conversational (the caller will produce a
    // friendly default; no point in calling the LLM for "").
    if trimmed.is_empty() {
        return IntentClass::Conversational;
    }

    // Slash commands always go through the full pipeline. They have explicit
    // semantics and the user is signaling intent unambiguously.
    if trimmed.starts_with('/') {
        return IntentClass::Implementation;
    }

    let normalized = normalize(trimmed);
    if normalized.is_empty() {
        // All punctuation — nothing actionable.
        return IntentClass::Conversational;
    }

    // Whole-message matches against canned phrase lists. These are the
    // strongest signals: "hello.", "thanks!", "what can you do?" etc.
    if GREETINGS.iter().any(|g| &normalized == g) {
        return IntentClass::Conversational;
    }
    if CLOSINGS.iter().any(|c| &normalized == c) {
        return IntentClass::Conversational;
    }
    if SELF_QUESTIONS.iter().any(|q| &normalized == q) {
        return IntentClass::Conversational;
    }

    // Prefix matches for greetings — "hello there friend" still reads as a
    // greeting; "hello, can you write a script" should NOT (action verb wins).
    let words: Vec<&str> = normalized.split_whitespace().collect();
    let word_count = words.len();

    let has_action_verb = words.iter().any(|w| ACTION_VERBS.contains(w));
    let has_research_verb = words.iter().any(|w| RESEARCH_VERBS.contains(w));
    let starts_with_question_word = words
        .first()
        .map(|w| QUESTION_WORDS.contains(w))
        .unwrap_or(false);
    let ends_with_question_mark = trimmed.ends_with('?');

    // Greeting prefix on a short message (no action/research verb) -> conversational.
    if !has_action_verb && !has_research_verb && !starts_with_question_word {
        for g in GREETINGS {
            if normalized.starts_with(g)
                && (normalized.len() == g.len()
                    || normalized.as_bytes().get(g.len()) == Some(&b' '))
                && word_count <= 6
            {
                return IntentClass::Conversational;
            }
        }
        for c in CLOSINGS {
            if normalized.starts_with(c)
                && (normalized.len() == c.len()
                    || normalized.as_bytes().get(c.len()) == Some(&b' '))
                && word_count <= 6
            {
                return IntentClass::Conversational;
            }
        }
    }

    // Owner-approved #4319 follow-up (2026-07-29): an ACTION_VERBS hit is no
    // longer unconditionally sufficient for Implementation. It splits in two:
    //
    // 1. The 4 "hard" verbs reused from `route::TCODE_HARD_VERBS`
    //    (fix/debug/implement/refactor) ALWAYS win, even over question words
    //    and research verbs — they're concrete enough on their own that
    //    `route_task` already makes this exact judgment for the SAME verbs.
    //    "how do I fix this bug" -> Implementation (because "fix" is concrete
    //    work, no other evidence needed).
    // 2. Every other ACTION_VERBS entry ("write", "create", "build", "run",
    //    "check", "test", ...) is common in ordinary conversation with a
    //    non-coding meaning — "can you check if it's raining tomorrow",
    //    "I'll run by the store after work" both contain one — so it ALSO
    //    needs `has_technical_context_signal` (the SAME detector the
    //    bug-report Research fallback below uses) before winning.
    //    "explain how to write a test" -> Implementation ("write" + the
    //    artifact noun "test"). "write a script" -> Implementation ("write" +
    //    "script"). "I'll run by the store after work" -> falls through
    //    (verb present, no context) to the checks below instead.
    if words.iter().any(|w| route::TCODE_HARD_VERBS.contains(w)) {
        return IntentClass::Implementation;
    }
    if has_action_verb && has_technical_context_signal(trimmed, &words) {
        return IntentClass::Implementation;
    }

    // Research signal: starts with question word OR contains a research verb,
    // and lacks an action verb (checked above).
    if starts_with_question_word || has_research_verb {
        return IntentClass::Research;
    }

    // A trailing question mark is positive evidence of an interrogative, not
    // a coding command -> Research, regardless of length. (Previously capped
    // at `word_count <= 15`; the cap only existed to interact with the
    // now-removed length-based Implementation fallback below, and itself
    // risked misrouting long genuine questions to Implementation — see #4319.)
    if ends_with_question_mark {
        return IntentClass::Research;
    }

    // "help me ..." is an implementation request even though "help" alone
    // isn't a verb we list (to avoid catching "help?").
    if normalized.starts_with("help me ") || normalized == "help me" {
        return IntentClass::Implementation;
    }

    // #4319 (code-critic HIGH-1): a verb-less bug report or incident
    // narrative must not silently drop to Conversational just because it
    // names no `ACTION_VERBS` word — that is WORSE than the original bug
    // (a real coding request answered as chat, with nothing anywhere to
    // signal it happened). Route it to Research instead: in-process,
    // tool-armed, lets the PM decide with real tools available, and —
    // critically — never spawns a subprocess. This is deliberately
    // narrower than the old length-based fallback: it requires an actual
    // lexical cue (see `has_technical_context_signal`/
    // `TECHNICAL_CONTEXT_WORDS`), not merely "the message is long". (Any
    // input reaching this line already lacks an `ACTION_VERBS` hit — that
    // path was fully resolved, one way or the other, above.)
    if has_technical_context_signal(trimmed, &words) {
        return IntentClass::Research;
    }

    // #4319: No positive evidence of a coding request OR a bug-report cue
    // at ANY length -> default to the cheap conversational path.
    //
    // Previously, input longer than 10 words with none of the signals above
    // fell through to `IntentClass::Implementation`, which respawns the
    // entire orchestrator as a subprocess. Word count and the absence of a
    // question mark are NOT evidence of a coding request — plenty of
    // ordinary conversational messages (status updates, context-setting,
    // "confirm the research agent is available" style check-ins) run past
    // 10 words with no action verb, no research verb, no leading question
    // word, and no trailing "?". Routing those to Implementation crashed
    // Concierge/Telegram/Slack (all three route through
    // `ctrl::run_pm_task_with_history`) whenever the subprocess spawn failed,
    // surfacing the literal string `subprocess exited with status Some(1)`
    // as the assistant's reply.
    //
    // Genuine coding requests are unaffected: they carry an `ACTION_VERBS`
    // hit (checked above, and unconditional on length) or a slash command
    // (checked at the top of this function), so `route_task` ->
    // `ProcessPmBridge::run_tcode` still fires for real work regardless of
    // sentence length.
    IntentClass::Conversational
}

#[cfg(test)]
#[path = "classifier_tests.rs"]
mod classifier_tests;

#[cfg(test)]
#[path = "classifier_tests_2.rs"]
mod classifier_tests_2;

#[cfg(test)]
#[path = "classifier_property_tests.rs"]
mod classifier_property_tests;

#[cfg(test)]
#[path = "unit_tests.rs"]
mod tests;
