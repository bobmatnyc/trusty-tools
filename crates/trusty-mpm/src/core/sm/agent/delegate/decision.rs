//! The SM decision protocol — structured-text (ReAct-style) action blocks (§3.4).
//!
//! Why: the SM's [`LlmProvider`](crate::core::sm::providers::LlmProvider) surface
//! is non-streaming `complete()` returning TEXT only — it has NO tool/function
//! calling (the provider trait deliberately omits the tool-call shape, see
//! `providers/mod.rs::ChatMessage`). So the §3.4 delegation loop cannot rely on
//! native tool calls; instead the SM emits a STRUCTURED-TEXT decision — a single
//! JSON action block — which the delegation engine ([`super`]) parses and
//! EXECUTES against the session-control + goal surfaces. This is the
//! deterministic, provider-agnostic decision protocol the spec's "verbs the SM
//! calls" (§4.2/SM_TOOLS) maps onto when the provider is text-only. Parsing is
//! lenient (the model may wrap the JSON in prose or a ```json fence) but the
//! resulting action is a strict, typed enum so the engine never interprets free
//! text as an instruction.
//! What: [`SmDecision`] — the closed set of actions the SM may emit at the
//! DECOMPOSE step (`delegate` to launch session-sized tasks, `respond` to talk
//! to the operator, or `do_work` — a PROHIBITION-violating direct-work attempt
//! the engine refuses and redirects, §3.2 SP1–SP5); [`TaskSpec`] — one
//! session-sized task; and [`parse_decision`] — the lenient JSON extractor.
//! Test: `decision_tests.rs` covers fenced/bare/prose-wrapped JSON, each action
//! variant, the empty-tasks guard, and the malformed-input fallback.

use serde::{Deserialize, Serialize};

/// One session-sized task the SM decomposed a goal into (§3.4 DECOMPOSE).
///
/// Why: DECOMPOSE splits a goal into one-or-many tasks, each of which becomes
/// exactly ONE launched session (§3.4: "one task → one launched session"). A task
/// carries everything LAUNCH needs: the working directory/repo, the prompt the
/// session executes, and an optional model/runtime selector.
/// What: `workdir` (required — the repo/dir the session runs against, mapped to
/// the launch `workdir`), `prompt` (required — the task the session performs,
/// DELIVERED to the session per #1299), and an optional `model` selector.
/// Test: `decision_tests.rs::parse_delegate_with_tasks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// The working directory / repository the launched session runs against.
    pub workdir: String,
    /// The task prompt the session executes (delivered via `sessions.send`, #1299).
    pub prompt: String,
    /// Optional model / runtime selector for the launched session.
    #[serde(default)]
    pub model: Option<String>,
}

impl TaskSpec {
    /// Whether this task is well-formed enough to launch.
    ///
    /// Why: a task with a blank `workdir` or `prompt` cannot be launched
    /// meaningfully (LAUNCH needs a target and a task), so the engine drops it
    /// rather than spawning an idle/garbage session.
    /// What: returns `true` iff both `workdir` and `prompt` are non-blank.
    /// Test: `parse_delegate_drops_blank_tasks`.
    pub fn is_launchable(&self) -> bool {
        !self.workdir.trim().is_empty() && !self.prompt.trim().is_empty()
    }
}

/// The closed set of decisions the SM may emit at the DECOMPOSE step (§3.4).
///
/// Why: the engine must NEVER interpret free model text as an instruction — that
/// is how a prohibition (SP5: "answer a work question from your own knowledge")
/// sneaks in. Constraining the SM's decision to this typed enum means the only
/// thing it can ask the engine to do is delegate (launch sessions), talk to the
/// operator, or — explicitly — attempt direct work, which the engine REFUSES and
/// redirects (§3.2). Anything unparseable degrades to [`SmDecision::Respond`]
/// (talk to the operator) — never to silently doing work.
/// What: `Delegate { tasks }` launches one session per [`TaskSpec`];
/// `Respond { message }` is an Allowlist-1 operator-facing reply (triage/status/
/// clarification); `DoWork { summary }` is a flagged direct-work attempt the
/// engine converts into a delegation refusal (the SP guard).
/// Test: `decision_tests.rs` — every variant + the lenient/fallback parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SmDecision {
    /// Delegate the goal to one-or-many launched sessions (the normal path).
    Delegate {
        /// The session-sized tasks (one launched session each).
        #[serde(default)]
        tasks: Vec<TaskSpec>,
    },
    /// Talk to the operator directly (Allowlist 1) — triage, status, a question.
    Respond {
        /// The operator-facing message.
        message: String,
    },
    /// A PROHIBITED direct-work attempt (SP1–SP5). The SM tried to do the work
    /// itself instead of delegating; the engine refuses and redirects to launch.
    DoWork {
        /// The work the SM attempted to do directly (captured for the redirect).
        #[serde(default)]
        summary: String,
    },
}

/// The opening fence/marker the SM may wrap its decision JSON in.
const JSON_FENCE: &str = "```json";
/// A generic code fence the SM may use instead of the `json`-tagged one.
const BARE_FENCE: &str = "```";

/// Parse the SM's reply text into a typed [`SmDecision`] (lenient extraction).
///
/// Why: the model returns TEXT (no tool calls), and may wrap its JSON action
/// block in a ```json fence or surround it with prose. The engine must extract
/// the action robustly but NEVER guess: if no valid action object is found, the
/// safe fallback is to treat the whole reply as an operator-facing message
/// ([`SmDecision::Respond`]) — the SM is talking, not delegating — rather than
/// inventing a delegation or (worse) doing work. This keeps an unparseable reply
/// on the always-safe Allowlist-1 path.
/// What: (1) if a ```json / ``` fenced block is present, extracts its inner
/// content (matching opening→closing fence); (2) otherwise (or if the fenced inner
/// is not an object) runs a BALANCED-BRACE scan from the first `{` that respects
/// JSON string literals, yielding the first complete top-level object regardless of
/// surrounding/trailing prose braces; (3) attempts to deserialize that candidate as
/// [`SmDecision`]; (4) on any failure returns `Respond { message: <trimmed original> }`.
/// Test: `decision_tests.rs::parse_fenced_json`, `parse_bare_json`,
/// `parse_prose_wrapped_json`, `parse_garbage_is_respond`, `parse_do_work_action`,
/// `parse_both_json_and_bare_fence`, `parse_prose_brace_after_json`,
/// `parse_prose_brace_before_json`, `parse_brace_inside_string_value`.
pub fn parse_decision(reply: &str) -> SmDecision {
    let trimmed = reply.trim();

    // (1)/(2)/(3): try each candidate object span in order — the first that
    // deserializes into a valid action wins. Trying successive top-level objects
    // (not just the first) is what lets a prose brace BEFORE the real action
    // object — e.g. `{note}: {"action":...}` — still parse: `{note}` fails to
    // deserialize and the scan moves on to the real object.
    for candidate in candidate_objects(trimmed) {
        if let Ok(decision) = serde_json::from_str::<SmDecision>(&candidate) {
            return decision;
        }
    }

    // (4): no valid action → the SM is talking to the operator (Allowlist 1).
    SmDecision::Respond {
        message: trimmed.to_string(),
    }
}

/// Collect candidate JSON-object spans from the SM's reply, best-first.
///
/// Why: the prior extractor was fragile in two ways the review flagged: (1) the
/// `[JSON_FENCE, BARE_FENCE]` ordering made `BARE_FENCE` re-match a ```json
/// opening as a bare fence and mishandled a reply with BOTH a ```json block AND a
/// later ``` block; and (2) the `first '{' ..= last '}'` span grabbed a WRONG span
/// whenever prose braces surrounded the JSON (e.g. `{...} see {docs}` →
/// `rfind('}')` captured the prose brace → truncated JSON → a spurious `Respond`
/// fallback that silently dropped an intended `Delegate`). Both are now robust.
/// What: returns, in priority order, (1) the first balanced object found INSIDE a
/// ```json (or bare ```) fenced block — the `json` tag is consumed as part of the
/// opening fence so it is never re-read as a bare fence; then (2) successive
/// complete top-level `{ … }` objects scanned over the whole text, each found by a
/// BALANCED-BRACE walk that RESPECTS JSON string literals + escapes (so braces
/// inside strings and prose braces before/after the object do not corrupt the
/// span). The caller deserializes each in turn and keeps the first valid action.
/// Test: exercised via `parse_decision` in `decision_tests.rs`
/// (`parse_both_json_and_bare_fence`, `parse_prose_brace_after_json`,
/// `parse_prose_brace_before_json`, `parse_brace_inside_string_value`, plus the
/// existing fenced/bare/prose/garbage cases).
fn candidate_objects(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // Strategy A: a fenced code block. Find the OPENING fence (prefer the explicit
    // ```json tag; fall back to a bare ```), consume it, then take the first
    // balanced object up to the matching CLOSING ``` fence. Consuming the ```json
    // tag as part of the opening prevents re-matching the same opening as a bare
    // fence (the BOTH-fences regression).
    if let Some(inner) = fenced_inner(text)
        && let Some(obj) = first_balanced_object(&inner)
    {
        candidates.push(obj);
    }

    // Strategy B: every complete top-level `{ … }` object in the whole text, in
    // order. Prose braces before the real object yield candidates that fail to
    // deserialize; the caller skips them and reaches the valid action.
    let mut from = 0;
    while let Some((obj, end)) = next_balanced_object(text, from) {
        if !candidates.contains(&obj) {
            candidates.push(obj);
        }
        from = end;
    }

    candidates
}

/// Return the inner content of the first fenced code block, if any.
///
/// Why: isolates fence handling so [`candidate_objects`] stays readable, and so
/// a ```json opening is matched exactly once (the `json` tag is part of the
/// opening, never re-read as a separate bare fence).
/// What: finds a ```json opening if present (else a bare ```), skips to the end of
/// that line (so the opening fence + optional language tag is fully consumed), then
/// returns the substring up to the next ``` closing fence (or end of text if the
/// block is unterminated). Returns `None` when there is no opening fence.
/// Test: exercised via `parse_decision` (`parse_fenced_json`, `parse_bare_json`,
/// `parse_both_json_and_bare_fence`).
fn fenced_inner(text: &str) -> Option<String> {
    // Prefer the explicit ```json fence; fall back to a bare ``` fence.
    let (open_at, fence_len) = match text.find(JSON_FENCE) {
        Some(i) => (i, JSON_FENCE.len()),
        None => (text.find(BARE_FENCE)?, BARE_FENCE.len()),
    };
    // Consume to the end of the opening-fence line so any trailing language tag on
    // a bare ```json-equivalent (e.g. "```json5") does not leak into the inner.
    let after_open = open_at + fence_len;
    let body_start = match text[after_open..].find('\n') {
        Some(nl) => after_open + nl + 1,
        None => after_open,
    };
    let rest = &text[body_start..];
    let inner = match rest.find(BARE_FENCE) {
        Some(close) => &rest[..close],
        None => rest,
    };
    Some(inner.to_string())
}

/// Return the first COMPLETE top-level JSON object in `text` (balanced scan).
///
/// Why: convenience wrapper for callers (the fenced-inner path) that only need the
/// first object and not the continuation offset.
/// What: returns the object span from [`next_balanced_object`] starting at 0.
/// Test: exercised via `parse_decision` (`parse_fenced_json`, `parse_bare_json`).
fn first_balanced_object(text: &str) -> Option<String> {
    next_balanced_object(text, 0).map(|(obj, _)| obj)
}

/// Find the next COMPLETE top-level `{ … }` object at or after `from`.
///
/// Why: a naive `first '{' ..= last '}'` span breaks when prose braces appear
/// before or after the JSON object, or when braces appear inside a JSON string
/// value. A depth-tracking scan that honours JSON string literals + escapes finds
/// each well-formed top-level object in turn, so the caller can try successive
/// candidates (skipping a leading prose `{…}` that is not a valid action).
/// What: from the first `{` at/after `from`, increments depth on `{` and
/// decrements on `}`, but ONLY when NOT inside a string literal (a `"` toggles
/// string mode; a `\` inside a string escapes the next char so an escaped quote
/// does not end the string). Returns `(object_span, end_offset)` the moment depth
/// returns to zero, where `end_offset` is the absolute index just past the closing
/// `}` (so a follow-up call can resume there). Returns `None` if there is no `{`
/// at/after `from` or the object never closes.
/// Test: `parse_prose_brace_after_json`, `parse_prose_brace_before_json`,
/// `parse_brace_inside_string_value`, `parse_escaped_quote_in_string`.
fn next_balanced_object(text: &str, from: usize) -> Option<(String, usize)> {
    if from >= text.len() {
        return None;
    }
    let rel = text[from..].find('{')?;
    let start = from + rel;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some((text[start..end].to_string(), end));
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
#[path = "decision_tests.rs"]
mod decision_tests;
