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

/// #4319 (code-critic CRITICAL, 2026-07-29 second follow-up): after two
/// prior passes on this knob (crash-on-everything, then silently-drop-real-
/// requests), a THIRD failure mode was proven by executing the classifier:
/// crash-on-ordinary-nouns. 19 plain verbs x dozens of generic context words
/// ("token", "session", "config", "queue", "cache", "container", "timeout",
/// "exception", "incident", "certificate", "production", "staging"),
/// matched anywhere in the sentence, meant ordinary requests like "check my
/// token balance" or "check the staging area before the wedding" still hit
/// `Implementation` — the exact subprocess-crash class #4319 exists to
/// eliminate — because those are common polysemous English nouns, not
/// reliable evidence of a coding request.
///
/// The fix inverts the default instead of curating a fourth list:
/// `Implementation` now requires an UNAMBIGUOUS signal only (see
/// `has_unambiguous_technical_signal` below) — a hard verb, a slash command,
/// a repo-file-shaped token, a snake_case identifier, or an explicit
/// error/stack-trace marker. This word list no longer feeds `Implementation`
/// AT ALL (except via the tiny, separately-declared
/// `CODE_ARTIFACT_IMPLEMENTATION_WORDS` exception below) — it now ONLY
/// distinguishes `Research` from `Conversational`, where the blast radius of
/// a wrong guess is near zero (in-process, tool-armed, no subprocess either
/// way). Kept broad deliberately for that reason: a verb-less bug report
/// ("the login page has been broken...") or a plain-verb-plus-generic-noun
/// sentence ("check my token balance") both correctly land on `Research`,
/// never `Conversational` (silently dropping a real signal) and never
/// `Implementation` (crashing).
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
    "codebase",
];

/// #4319 (code-critic CRITICAL follow-up): a TINY, deliberately separate
/// exception — the ONLY code-artifact nouns allowed to gate `Implementation`
/// (not just `Research`) when paired with a plain verb, and ONLY because
/// "run the tests"/"build the release" are a non-negotiable requirement
/// (owner-verified working, must not regress) with no other unambiguous
/// signal available: neither sentence has a hard verb, a file path, a
/// snake_case identifier, or an error marker.
///
/// Why kept separate from `TECHNICAL_CONTEXT_WORDS` rather than folded in:
/// this list DOES feed `Implementation`, so it must stay as small as
/// possible and be reviewed on its own — adding a third word here should
/// feel uncomfortable. "tests"/"release" are themselves imperfect (a
/// "release" can mean a movie release; "tests" can mean school exams), but
/// in the shape "VERB the ARTIFACT" with nothing else in the sentence, they
/// overwhelmingly mean a software test suite / build artifact in this
/// product's actual traffic. Also reused by `route::route_task`'s
/// `run`/`build` gate (`super::CODE_ARTIFACT_IMPLEMENTATION_WORDS`) so the
/// two routers agree.
const CODE_ARTIFACT_IMPLEMENTATION_WORDS: &[&str] = &["tests", "release"];

/// #4319 (code-critic CRITICAL follow-up): True when `input` carries an
/// UNAMBIGUOUS technical signal — the ONLY evidence base for
/// `IntentClass::Implementation` now that a bare `ACTION_VERBS` hit plus a
/// generic context word is no longer sufficient (that combination is
/// AMBIGUOUS; see `TECHNICAL_CONTEXT_WORDS`'s doc comment — it only feeds
/// `Research`). Combines, all reused rather than reimplemented:
/// `route::has_repo_file_token` (a `.rs`/`.ts`/`.py` extension or `src/`
/// path token), `route::has_error_or_stack_trace_marker` (a raw `error:`
/// marker or a "unit test"/"failing test"/"stack trace" phrase), and a
/// snake_case-identifier check (a token containing `_` longer than 3 chars,
/// e.g. `delegate_to_agent` — `normalize` already preserves underscores for
/// exactly this reason, so recognizing that preserved shape reuses the SAME
/// design decision rather than adding a third detector).
/// Test: `classifier_tests_2::*` (bucket tests), `route_tests::*`.
fn has_unambiguous_technical_signal(input: &str, words: &[&str]) -> bool {
    route::has_repo_file_token(input)
        || route::has_error_or_stack_trace_marker(input)
        || words.iter().any(|w| w.len() > 3 && w.contains('_'))
}

/// #4319: True when `input` carries the broader, AMBIGUOUS technical/bug-
/// report vocabulary in `TECHNICAL_CONTEXT_WORDS` — used ONLY to route
/// `Research` (a verb-less bug report, or a plain verb plus a generic
/// context word), never `Implementation`.
/// Test: `classifier_tests_2::*` (bug-report and bucket-1 cases).
fn has_technical_context_word(words: &[&str]) -> bool {
    words.iter().any(|w| TECHNICAL_CONTEXT_WORDS.contains(w))
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
/// What: Applies heuristics in priority order — empty, slash command,
/// greeting, closing, self-question, hard-action-verb scan (wins outright,
/// any length — `route::TCODE_HARD_VERBS`: fix/debug/implement/refactor),
/// unambiguous-technical-signal scan (see `has_unambiguous_technical_signal`
/// — repo-file token, snake_case identifier, error/stack-trace marker),
/// the tiny action-verb-plus-code-artifact-noun exception
/// (`CODE_ARTIFACT_IMPLEMENTATION_WORDS`: "tests"/"release"),
/// research-verb/question-word scan, trailing question mark, "help me",
/// action-verb-plus-generic-context-word (Research — AMBIGUOUS, not
/// Implementation), technical-context-word-alone (Research), then defaults
/// to `Conversational` when no positive signal fired.
/// `Implementation` requires an UNAMBIGUOUS signal ONLY (#4319 code-critic
/// CRITICAL, 2026-07-29 second follow-up) — word count is never evidence
/// (original #4319), a bare `ACTION_VERBS` hit is never evidence on its own
/// (first follow-up), and neither is a plain verb plus a generic context
/// word like "token"/"session"/"config" (this follow-up: "check my token
/// balance" still crashed the subprocess pipeline under the first
/// follow-up's design). Every ambiguous case is biased toward `Research`
/// instead: `src/ctrl/pm_task/dispatch/history.rs` shows `Research` and the
/// `Conversational` fallthrough share the SAME in-process tool-armed loop,
/// structurally distinct from `Implementation`'s re-exec-as-subprocess — a
/// wrong `Research` guess costs a slightly heavier in-process turn; a wrong
/// `Implementation` guess crashes the chat.
/// Test: `tests::*` below covers greetings, closings, self-questions,
/// research verbs, question words, clear task verbs, slash commands, the
/// #4319 long-conversational-input regression, and edge cases;
/// `classifier_tests_2::*` pins the four decision buckets including the
/// 12-sentence CRITICAL regression.
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

    // #4319 code-critic CRITICAL follow-up (2026-07-29): `Implementation`
    // requires an UNAMBIGUOUS signal. Two tiers:
    //
    // 1. The 4 "hard" verbs reused from `route::TCODE_HARD_VERBS`
    //    (fix/debug/implement/refactor) ALWAYS win — they're concrete
    //    enough on their own that `route_task` already makes this exact
    //    judgment for the SAME verbs. "how do I fix this bug" ->
    //    Implementation.
    //    "how do I fix this bug" -> Implementation because "fix" wins even
    //    over the leading question word "how".
    if words.iter().any(|w| route::TCODE_HARD_VERBS.contains(w)) {
        return IntentClass::Implementation;
    }

    // Research signal: starts with question word OR contains a research
    // verb. Checked BEFORE `has_unambiguous_technical_signal` below (unlike
    // the hard-verb check above) so a genuine QUESTION about an identifier —
    // "what does run_pm_task do" — still lands on Research instead of the
    // snake_case-identifier check alone forcing Implementation. Contrast
    // "find all uses of delegate_to_agent": no leading question word, so it
    // falls through to the identifier check below and correctly reaches
    // Implementation.
    if starts_with_question_word || has_research_verb {
        return IntentClass::Research;
    }

    // 2. `has_unambiguous_technical_signal` — a repo-file-shaped token, a
    //    snake_case identifier, or an explicit error/stack-trace marker —
    //    wins regardless of any verb at all, now that a leading question
    //    word/research verb has already been ruled out above.
    if has_unambiguous_technical_signal(trimmed, &words) {
        return IntentClass::Implementation;
    }

    // Tiny, deliberately narrow exception (see `CODE_ARTIFACT_IMPLEMENTATION_WORDS`'s
    // doc comment): "run the tests" / "build the release" have no OTHER
    // unambiguous signal (no hard verb, no file token, no identifier, no
    // error marker) but are a non-negotiable requirement. A plain action
    // verb plus one of these 2 words is the ONLY other path to
    // Implementation.
    if has_action_verb
        && words
            .iter()
            .any(|w| CODE_ARTIFACT_IMPLEMENTATION_WORDS.contains(w))
    {
        return IntentClass::Implementation;
    }

    // A trailing question mark is positive evidence of an interrogative, not
    // a coding command -> Research, regardless of length.
    if ends_with_question_mark {
        return IntentClass::Research;
    }

    // "help me ..." is an implementation request even though "help" alone
    // isn't a verb we list (to avoid catching "help?").
    if normalized.starts_with("help me ") || normalized == "help me" {
        return IntentClass::Implementation;
    }

    // #4319 code-critic CRITICAL follow-up: a plain `ACTION_VERBS` hit PLUS
    // a generic technical-context word ("check my token balance", "check
    // the staging area before the wedding") is AMBIGUOUS, not unambiguous —
    // it goes to Research, never Implementation. This is the fix for the
    // proven regression: the prior follow-up treated this exact combination
    // as sufficient for Implementation, which still crashed the subprocess
    // pipeline on ordinary sentences using common polysemous nouns (token,
    // session, config, queue, cache, container, timeout, exception,
    // incident, certificate, production, staging, ...).
    if has_action_verb && has_technical_context_word(&words) {
        return IntentClass::Research;
    }

    // #4319 (code-critic HIGH-1): a verb-less bug report or incident
    // narrative must not silently drop to Conversational just because it
    // names no `ACTION_VERBS` word — that is WORSE than the original bug
    // (a real coding request answered as chat, with nothing anywhere to
    // signal it happened). Route it to Research instead: in-process,
    // tool-armed, lets the PM decide with real tools available, and —
    // critically — never spawns a subprocess.
    if has_technical_context_word(&words) {
        return IntentClass::Research;
    }

    // No positive evidence of ANY kind -> default to the cheap conversational
    // path. Word count alone is NEVER evidence for `Implementation` (#4319);
    // neither is a bare `ACTION_VERBS` hit, nor a plain verb plus a generic
    // context word (both first/second follow-up corrections above).
    //
    // Genuine coding requests are unaffected: they carry a hard verb, an
    // unambiguous technical signal, a slash command, or (for "run the
    // tests"/"build the release" specifically) the tiny code-artifact-noun
    // exception, so `route_task` -> `ProcessPmBridge::run_tcode` still fires
    // for real work.
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
