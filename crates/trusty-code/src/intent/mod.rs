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
//! See `tests` module below — fixes #199, #203.

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
    "what is open-mpm",
    "what is open mpm",
    "what do you do",
    "what's your name",
    "whats your name",
    "are you there",
    "you there",
];

/// Strip surrounding/embedded punctuation for matching.
///
/// Why: Users write "Hello!" / "hi." / "hey," — comparing to plain "hello"
/// requires normalization. We keep apostrophes (so "what's" stays whole)
/// and internal hyphens (so "open-mpm" stays whole).
/// What: Lowercases and replaces ASCII punctuation (except `'`, `-`, `_`)
/// with spaces, then collapses runs of whitespace. Underscores are preserved
/// so identifiers like `run_pm_task_with_session` remain a single token
/// rather than fragmenting into "run" / "task" (which would falsely match
/// ACTION_VERBS).
/// Test: Covered indirectly by classifier tests — "Hello!!!" must classify
/// the same as "hello".
fn normalize(input: &str) -> String {
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
/// closing, self-question, research-question, action-verb scan, length-based
/// fallback.
/// Test: `tests::*` below covers greetings, closings, self-questions, research
/// verbs, question words, clear task verbs, slash commands, and edge cases.
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

    // Action verbs ALWAYS win — even over question words and research verbs.
    // "how do I fix this bug" -> Implementation (because "fix" is concrete work).
    // "explain how to write a test" -> Implementation (because "write" wins).
    if has_action_verb {
        return IntentClass::Implementation;
    }

    // Research signal: starts with question word OR contains a research verb,
    // and lacks an action verb (checked above).
    if starts_with_question_word || has_research_verb {
        return IntentClass::Research;
    }

    // Question mark on a short input with no action verb -> Research
    // (e.g. "is bedrock enabled?", "does this support tokio?").
    if ends_with_question_mark && word_count <= 15 {
        return IntentClass::Research;
    }

    // "help me ..." is an implementation request even though "help" alone
    // isn't a verb we list (to avoid catching "help?").
    if normalized.starts_with("help me ") || normalized == "help me" {
        return IntentClass::Implementation;
    }

    // Short input, no action/research/question signal -> probably conversational.
    if word_count <= 4 {
        return IntentClass::Conversational;
    }

    // Long input without action verbs but past the threshold -> Implementation.
    if word_count > 10 {
        return IntentClass::Implementation;
    }

    // Ambiguous middle range (5-10 words, no action verb): treat as
    // conversational. The user can re-issue with an action verb if they
    // actually wanted delegation.
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
