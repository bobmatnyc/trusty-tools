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
/// What: (1) strips a leading ```json / ``` fence + trailing fence if present;
/// (2) failing that, scans for the first `{`…matching `}` span; (3) attempts to
/// deserialize that span as [`SmDecision`]; (4) on any failure returns
/// `Respond { message: <trimmed original> }`.
/// Test: `decision_tests.rs::parse_fenced_json`, `parse_bare_json`,
/// `parse_prose_wrapped_json`, `parse_garbage_is_respond`,
/// `parse_do_work_action`.
pub fn parse_decision(reply: &str) -> SmDecision {
    let trimmed = reply.trim();

    // (1)/(2): find a candidate JSON object span.
    if let Some(candidate) = extract_json_object(trimmed)
        && let Ok(decision) = serde_json::from_str::<SmDecision>(&candidate)
    {
        return decision;
    }

    // (4): no valid action → the SM is talking to the operator (Allowlist 1).
    SmDecision::Respond {
        message: trimmed.to_string(),
    }
}

/// Extract a candidate JSON-object substring from the SM's reply.
///
/// Why: keeps [`parse_decision`] readable by isolating the three extraction
/// strategies (fenced → bare-fenced → first-brace-span). Returns the substring
/// only; the caller decides whether it deserialises into a valid action.
/// What: if the text contains a ```json (or ```) fence, returns the content up to
/// the closing fence; otherwise returns the span from the first `{` to the last
/// `}` (inclusive), or `None` when no brace pair exists.
/// Test: exercised via `parse_decision` in `decision_tests.rs`.
fn extract_json_object(text: &str) -> Option<String> {
    // Strategy A: a ```json … ``` (or ``` … ```) fenced block.
    for fence in [JSON_FENCE, BARE_FENCE] {
        if let Some(after) = text.find(fence).map(|i| i + fence.len()) {
            let rest = &text[after..];
            if let Some(end) = rest.find(BARE_FENCE) {
                let inner = rest[..end].trim();
                if inner.starts_with('{') {
                    return Some(inner.to_string());
                }
            }
        }
    }

    // Strategy B: the first `{` … last `}` span (prose-wrapped or bare object).
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        Some(text[start..=end].to_string())
    } else {
        None
    }
}

#[cfg(test)]
#[path = "decision_tests.rs"]
mod decision_tests;
