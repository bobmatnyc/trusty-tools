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

/// Action verbs that signal "this is a task, not idle chat" — but, per the
/// #4319 owner decision (2026-07-29, final iteration), NEVER by themselves
/// evidence for `IntentClass::Implementation`. Round 2 disproved a bare hit
/// on this list -> `Implementation` ("can you check if it's raining
/// tomorrow" crashed); round 6 disproved hard-verb-subset-of-this-list plus
/// `TECHNICAL_CONTEXT_WORDS` -> `Implementation` (~29 ordinary sentences
/// crashed). This constant earns its keep for two narrower, lower-stakes
/// jobs only: (1) suppressing the greeting-prefix short-circuit — "hello,
/// can you write a script" should NOT collapse to `Conversational` just
/// because it starts with "hello"; (2) pairing with
/// `TECHNICAL_CONTEXT_WORDS` to pick `Research` over `Conversational` for an
/// ambiguous plain-verb-plus-generic-noun sentence. Neither job feeds
/// `Implementation` — see `classify_intent`'s doc comment for the exhaustive
/// 4-signal contract.
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

/// #4319 (OWNER DECISION, 2026-07-29, final iteration — seventh follow-up):
/// this list is used ONLY to help choose `Research` vs `Conversational`. It
/// NEVER feeds `IntentClass::Implementation`, in any combination, with any
/// verb, hard or plain. That is not new — the second follow-up already
/// established it — but a SIXTH round briefly reintroduced a "hard verb +
/// this list" path to `Implementation`, which was then proven unsafe by the
/// same method (executing the classifier) that found every prior gap: "fix
/// my gym session", "debug the incident report from the fender bender",
/// "fix the queue at the deli counter", "refactor the outage in our
/// friendship", "debug my unresponsive teenager", and ~24 more ordinary
/// sentences all reached `Implementation` using words ALREADY in this exact
/// list — not a hypothetical gap, the list itself. The owner's final
/// decision: no word list, of any size, curated with any amount of care,
/// can distinguish "the bug" (a software defect) from "a bug in my garden"
/// (an insect) using only the word `bug`. `Implementation` is now reachable
/// via exactly 4 signals — none of them a word list — enumerated
/// exhaustively in `classify_intent`'s doc comment. This list's role is
/// unchanged from the second follow-up: choosing `Research` over
/// `Conversational` for a plain-verb-plus-generic-noun sentence or a
/// verb-less bug report, where the blast radius of a wrong guess is near
/// zero (in-process, tool-armed, no subprocess either way) — see
/// `classify_intent`'s doc comment for why that asymmetry is exactly what
/// makes a word list safe here and unsafe for `Implementation`.
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
    "tests",
    "release",
];

/// #4319 (OWNER DECISION, 2026-07-29, final iteration): True when `input`
/// carries one of the THREE non-slash unambiguous signals — the ENTIRE
/// evidence base for `IntentClass::Implementation` (together with a leading
/// `/`, that's all four; see `classify_intent`'s doc comment for the
/// authoritative enumeration). Deliberately contains NO word list of any
/// kind — every previous word-list-based attempt at this function (hard
/// verbs, generic context words, code-artifact nouns) was proven unsafe by
/// executing the classifier against ordinary English. Combines, all reused
/// rather than reimplemented: `route::has_repo_file_token` (a `.rs`/`.ts`/
/// `.py` extension or `src/` path token), `route::has_error_or_stack_trace_marker`
/// (a raw `error:` marker or a "unit test"/"failing test"/"stack trace"
/// phrase), and a snake_case-identifier check (a token containing `_`
/// longer than 3 chars, e.g. `delegate_to_agent` — `normalize` already
/// preserves underscores for exactly this reason, so recognizing that
/// preserved shape reuses the SAME design decision rather than adding a
/// fourth detector).
/// Test: `classifier_regression_tests::*` (bucket tests), `route_tests::*`.
fn has_unambiguous_technical_signal(input: &str, words: &[&str]) -> bool {
    route::has_repo_file_token(input)
        || route::has_error_or_stack_trace_marker(input)
        || words.iter().any(|w| w.len() > 3 && w.contains('_'))
}

/// #4319: True when `input` carries the broader, AMBIGUOUS technical/bug-
/// report vocabulary in `TECHNICAL_CONTEXT_WORDS` — used ONLY to route
/// `Research` (a verb-less bug report, or a plain verb plus a generic
/// context word), NEVER `Implementation`. See `TECHNICAL_CONTEXT_WORDS`'s
/// doc comment: this function must never be consulted by the
/// `Implementation` decision, in any combination, with any verb.
/// Test: `classifier_regression_tests::*` (bug-report and bucket-1 cases).
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
///
/// **#4319 OWNER DECISION (2026-07-29, final iteration — seventh follow-up):
/// `IntentClass::Implementation` is reachable via EXACTLY these four signals,
/// and no others. This is the complete, exhaustive list — cross-check it
/// against the actual `return IntentClass::Implementation` sites in this
/// function before trusting it; this exact sentence has been wrong before
/// (see the history below).**
/// 1. A leading `/` (slash command) — explicit, unambiguous user intent.
/// 2. A repo-file-shaped token — a path with a separator plus an extension,
///    or a known source extension (`route::has_repo_file_token`: `.rs`/
///    `.ts`/`.py`, or a `src/` path token).
/// 3. A snake_case identifier — a token containing `_`, longer than 3
///    chars (e.g. `delegate_to_agent`).
/// 4. An explicit error / stack-trace marker (`route::has_error_or_stack_trace_marker`:
///    a raw `error:` substring, or a "unit test"/"failing test"/"stack
///    trace" phrase).
///
/// Signals 2-4 are combined in `has_unambiguous_technical_signal` and
/// checked together (no verb of any kind is required for them to fire).
/// **No word list of any kind — no verb, hard or plain, and no noun —
/// contributes to this decision, in any combination, at any point in this
/// function.** Every ambiguous case (a verb-less bug report, a verb plus a
/// generic noun, or plain conversational text with no signal at all) routes
/// to `Research` instead: in-process, tool-armed — the PM decides, and can
/// still invoke `dispatch_task` when it judges the work warrants it
/// (`ctrl::pm_task::dispatch::history::run_pm_task_with_history` registers
/// `PmBridgeTool` unconditionally for any non-`Conversational`
/// classification — its only intent-based branch is a `Conversational`-only
/// fast-path skip). A wrong `Research` guess costs one extra decision hop;
/// a wrong `Implementation` guess (in the one path that still distinguishes
/// them, `api::server::handlers`'s subprocess-workflow branch) crashes the
/// chat. `TECHNICAL_CONTEXT_WORDS`/`ACTION_VERBS` still exist, but ONLY to
/// help choose `Research` over `Conversational` for that ambiguous middle
/// ground — see their own doc comments; neither is consulted by the
/// `Implementation` decision anywhere in this function.
///
/// This is the seventh and, the owner has stated, final iteration on this
/// exact question, arrived at only after six narrower designs were each
/// disproven by executing the classifier against ordinary English:
/// 1. (Original #4319) word count over 10 -> `Implementation`. Disproven:
///    "confirm the research agent is available" (an ordinary check-in)
///    crashed.
/// 2. A bare `ACTION_VERBS` hit -> `Implementation`. Disproven: "can you
///    check if it's raining tomorrow" crashed.
/// 3. A plain verb plus ANY `TECHNICAL_CONTEXT_WORDS` co-occurrence ->
///    `Implementation`. Disproven: "check my token balance" crashed.
/// 4. The above, plus a narrow "tests"/"release" carve-out for "run the
///    tests"/"build the release" -> `Implementation`. Disproven: "check my
///    blood tests" / "check the release date of the movie" crashed.
/// 5. An unconditional `"help me "` prefix -> `Implementation` (pre-existing
///    on `origin/main`, predating all of the above). Disproven: "help me
///    relax" / "help me plan my week" and 11 more ordinary
///    requests-for-assistance crashed.
/// 6. A hard verb (fix/debug/implement/refactor) plus a
///    `TECHNICAL_CONTEXT_WORDS` co-occurrence -> `Implementation`.
///    Disproven: "fix my gym session", "debug the incident report from the
///    fender bender", "refactor the outage in our friendship", and ~26 more
///    ordinary sentences crashed — using words ALREADY in
///    `TECHNICAL_CONTEXT_WORDS`, proving the failure was structural (any
///    word list combined with any verb is exploitable this way), not a gap
///    in that list's coverage. The regression tests for round 6 were
///    themselves written to confirm the design rather than probe it (they
///    asserted the broken combination didn't occur, rather than searching
///    for sentences where it did) — the signal that this whole approach
///    (verb + word list, however corroborated) cannot converge, which is
///    why round 7 removes word lists from the decision entirely rather than
///    narrowing them further.
///
/// There is no known residual risk of the SAME shape as rounds 1-6: none of
/// the four signals above depend on a word list, so there is no vocabulary
/// gap left to close. Signals 2-4 are syntactic (a file extension, an
/// underscore, a literal `error:`/phrase), not semantic, so they don't carry
/// the polysemy that broke every word-list-based design. This is a
/// structural difference, not a promise that the syntactic checks
/// themselves are flawless in every case (e.g. a snake_case token could in
/// principle appear in non-code prose) — any such case, if found, should be
/// added as a new verified regression test rather than reopening a word
/// list.
/// Test: `tests::*` below covers greetings, closings, self-questions,
/// research verbs, question words, clear task verbs, slash commands, the
/// #4319 long-conversational-input regression, and edge cases;
/// `classifier_regression_tests::*` pins the four decision buckets including the
/// 14-sentence ordinary-noun regression, the 13-phrase "help me" regression,
/// and the 29-phrase hard-verb-polysemy regression — split into two shapes,
/// since they resolve differently: 9 phrases pairing a hard verb with a
/// genuinely non-technical object (no `TECHNICAL_CONTEXT_WORDS` token at
/// all, e.g. "fix a drink") land on `Conversational`; the other 20 pairing a
/// hard verb with an object that happens to contain a
/// `TECHNICAL_CONTEXT_WORDS` token in its everyday sense (e.g. "fix my gym
/// session") land on `Research`. Neither shape ever reaches `Implementation`
/// — the whole verb tier is gone.
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

    // Research signal: starts with question word OR contains a research
    // verb. Checked BEFORE `has_unambiguous_technical_signal` below so a
    // genuine QUESTION about an identifier — "what does run_pm_task do" —
    // still lands on Research instead of the snake_case-identifier check
    // alone forcing Implementation. Contrast "find all uses of
    // delegate_to_agent": no leading question word, so it falls through to
    // the identifier check below and correctly reaches Implementation.
    if starts_with_question_word || has_research_verb {
        return IntentClass::Research;
    }

    // #4319 OWNER DECISION (2026-07-29, final iteration): `has_unambiguous_technical_signal`
    //    — a repo-file-shaped token, a snake_case identifier, or an explicit
    //    error/stack-trace marker — is the LAST of the 4 TOTAL paths to
    //    `Implementation` (slash command is the other) — see
    //    `classify_intent`'s doc comment for the authoritative, exhaustive
    //    enumeration and the full 7-round history of why every word-list-based
    //    alternative (hard verbs, generic context words, code-artifact nouns,
    //    hard verb + context word) was proven unsafe by executing the
    //    classifier. NO verb of any kind is checked here or anywhere else in
    //    this function as evidence for `Implementation` — "how do I fix this
    //    bug" and "debug the auth middleware" now correctly land on `Research`
    //    (formally rescinded as must-work-Implementation cases by the owner;
    //    "fix main.rs" still reaches `Implementation` via the file token
    //    below, and "/fix the thing" via the slash command above).
    if has_unambiguous_technical_signal(trimmed, &words) {
        return IntentClass::Implementation;
    }

    // A trailing question mark is positive evidence of an interrogative, not
    // a coding command -> Research, regardless of length.
    if ends_with_question_mark {
        return IntentClass::Research;
    }

    // A plain verb plus a generic technical-context word ("check my token
    // balance", "check the staging area before the wedding") is AMBIGUOUS,
    // not unambiguous — it goes to Research, never Implementation. Proven
    // regression (second follow-up): treating this combination as
    // sufficient for Implementation still crashed the subprocess pipeline
    // on ordinary sentences using common polysemous nouns (token, session,
    // config, queue, cache, container, timeout, exception, incident,
    // certificate, production, staging, ...). `TECHNICAL_CONTEXT_WORDS`
    // earns its keep here — this is a `Research`-vs-`Conversational` choice
    // only, never `Implementation`.
    if has_action_verb && has_technical_context_word(&words) {
        return IntentClass::Research;
    }

    // A verb-less bug report or incident narrative must not silently drop
    // to Conversational just because it names no `ACTION_VERBS` word — that
    // is WORSE than the original bug (a real coding request answered as
    // chat, with nothing anywhere to signal it happened). Route it to
    // Research instead: in-process, tool-armed, lets the PM decide with
    // real tools available, and — critically — never spawns a subprocess.
    if has_technical_context_word(&words) {
        return IntentClass::Research;
    }

    // No positive evidence of ANY kind -> default to the cheap conversational
    // path. Word count alone is NEVER evidence for `Implementation` (#4319);
    // neither is any verb (hard or plain), any word list, or a `"help me "`
    // prefix — see this function's doc comment for the complete, final
    // 4-signal contract and the full history of why every word-list-based
    // alternative was disproven.
    //
    // Genuine coding requests carrying an unambiguous technical signal or a
    // slash command still classify `Implementation` here and still reach
    // Tcode via `route_task` -> `ProcessPmBridge::run_tcode`. "run the
    // tests"/"build the release" (and, now, "fix the login bug"/"debug the
    // auth middleware") classify `Research` instead — `dispatch_task` stays
    // reachable from `Research` regardless, and `route::route_task` (left
    // UNCHANGED by this round — its word list picks between two safe
    // backends, not a crash surface) carries its own narrow, route.rs-local
    // "tests"/"release" exception so `dispatch_task("run the tests")`
    // still resolves to `Tcode`.
    IntentClass::Conversational
}

#[cfg(test)]
#[path = "classifier_tests.rs"]
mod classifier_tests;

#[cfg(test)]
#[path = "classifier_regression_tests.rs"]
mod classifier_regression_tests;

#[cfg(test)]
#[path = "classifier_property_tests.rs"]
mod classifier_property_tests;

#[cfg(test)]
#[path = "unit_tests.rs"]
mod tests;
