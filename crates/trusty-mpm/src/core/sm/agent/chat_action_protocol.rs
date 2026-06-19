//! The action-chat decision protocol: prompt surface, parse, and verb dispatch.
//!
//! Why: Phase 2 (#1283) makes the coordinator chat ACTION-CAPABLE — the SM may
//! invoke managed-session verbs INLINE within one chat turn (a synchronous
//! read→decide→execute→feed-back loop), not the goal-shaped background
//! delegation of [`super::delegate`]. That loop needs three pieces kept apart
//! from the loop driver itself (so each file stays under the SLOC cap): the
//! structured-text decision shape the model emits, the lenient parser that turns
//! the model's text into that shape, and the verb-name → [`SessionControl`]
//! dispatch. They live here; [`super::chat_action`] owns the loop.
//! What: [`ActionDecision`] (a verb-call or a final answer), [`parse_action`]
//! (lenient JSON extraction reusing the balanced-brace style of
//! [`super::delegate`]'s `parse_decision`, falling back to a final answer on any
//! parse miss), [`action_instructions`] (the prompt block advertising the inline
//! verb surface — its verb list rendered from the [`catalog`](crate::client::catalog)
//! single source of truth, NOT hand-listed), and [`execute_verb`] (maps a chosen
//! verb name onto the matching [`SessionControl`] method and returns its JSON).
//! Test: `chat_action_tests.rs` — verb-call parse, final-answer parse, prose
//! fallback, prompt lists every catalog verb, and each verb dispatches.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::catalog::sm_session_verbs;
use crate::core::sm::control::{LaunchParams, SessionControl, SessionControlError};

/// The sentinel `action` value the model emits to END the action loop.
const FINAL_ACTION: &str = "final";

/// One decision the SM emits per action-loop iteration (#1283).
///
/// Why: the text-only provider has no native tool calling, so the inline action
/// loop is driven by a structured-text decision the model emits and the loop
/// parses — exactly ONE of: call a managed-session verb (and feed its result
/// back), or stop with a final operator-facing answer. Modelling it as one flat
/// struct (`action` + optional `args`/`message`) avoids serde's tagged/untagged
/// ambiguity and lets [`ActionDecision::classify`] decide verb-vs-final from the
/// `action` value; anything unparseable degrades to the final answer (the
/// always-safe terminal — the SM is talking, not acting).
/// What: `action` is the verb name or the `"final"` sentinel; `args` carries a
/// verb's arguments (ignored for `final`); `message` carries the final reply
/// (ignored for a verb). [`ActionDecision::classify`] maps it to [`Decided`].
/// Test: `chat_action_tests.rs::parse_verb_call`, `parse_final_answer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDecision {
    /// The verb name (e.g. `sessions.list`) or the `"final"` sentinel.
    pub action: String,
    /// The verb's arguments (verb-specific; may be absent/empty).
    #[serde(default)]
    pub args: Value,
    /// The operator-facing final reply (present on the `final` action).
    #[serde(default)]
    pub message: Option<String>,
}

/// A classified [`ActionDecision`]: either run a verb or stop with a final answer.
///
/// Why: the loop driver wants a clean two-way split, not raw string-matching on
/// `action` at the call site. Classifying once here keeps the loop readable.
/// What: [`Verb`](Decided::Verb) carries the verb name + args to execute;
/// [`Final`](Decided::Final) carries the operator-facing reply that ends the turn.
/// Test: `chat_action_tests.rs::parse_verb_call`, `parse_final_answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decided {
    /// Execute this verb with these args, then feed the result back.
    Verb {
        /// The catalog verb name (e.g. `sessions.list`).
        verb: String,
        /// The verb's arguments.
        args: Value,
    },
    /// Stop the loop and answer the operator with this text.
    Final {
        /// The operator-facing final reply.
        message: String,
    },
}

impl ActionDecision {
    /// Classify this decision into a verb call or a final answer.
    ///
    /// Why: the loop needs a clean enum to branch on; a `"final"` action (or any
    /// blank action) is the terminal, everything else is a verb call.
    /// What: returns [`Decided::Final`] when `action` is the `"final"` sentinel
    /// (or blank), carrying `message` (or an empty string); otherwise
    /// [`Decided::Verb`] with the verb name + args.
    /// Test: `chat_action_tests.rs::parse_verb_call`, `parse_final_answer`.
    pub fn classify(self) -> Decided {
        if is_final_action(self.action.trim()) || self.action.trim().is_empty() {
            Decided::Final {
                message: self.message.unwrap_or_default(),
            }
        } else {
            Decided::Verb {
                verb: self.action,
                args: self.args,
            }
        }
    }
}

/// Parse the model's reply into a [`Decided`] action (lenient extraction).
///
/// Why: the model returns TEXT (no tool calls) and may wrap its JSON action in a
/// ```json fence or surround it with prose. The loop must extract robustly but
/// NEVER guess: if no valid action object is found, the safe terminal is to treat
/// the whole reply as the final answer (the SM is talking, not acting). This
/// mirrors [`super::delegate`]'s `parse_decision` philosophy.
/// What: scans `reply` for the first balanced top-level `{ … }` object (honoring
/// fenced blocks and JSON string literals) that deserializes into an
/// [`ActionDecision`], then [`classify`](ActionDecision::classify)es it; on any
/// miss returns `Decided::Final { message: <trimmed reply> }`.
/// Test: `chat_action_tests.rs::parse_verb_call`, `parse_final_answer`,
/// `parse_prose_is_final`, `parse_fenced_verb`.
pub fn parse_action(reply: &str) -> Decided {
    let trimmed = reply.trim();
    for candidate in candidate_objects(trimmed) {
        if let Ok(decision) = serde_json::from_str::<ActionDecision>(&candidate) {
            let decided = decision.classify();
            // A bare object that happens to deserialize (every field defaulting)
            // but names no real verb and carries no message is NOT a decision —
            // skip it so the prose fallback can win.
            if let Decided::Verb { ref verb, .. } = decided
                && verb.trim().is_empty()
            {
                continue;
            }
            return decided;
        }
    }
    Decided::Final {
        message: trimmed.to_string(),
    }
}

/// Collect candidate JSON-object spans from the model's reply, best-first.
///
/// Why: the action JSON may sit inside a ```json fence or amid prose; a naive
/// `first '{' ..= last '}'` span breaks on prose braces. A balanced-brace walk
/// that respects string literals yields each well-formed top-level object so the
/// caller can try them in turn (mirrors `parse_decision`'s extractor).
/// What: returns the first object inside a fenced block (if any), then every
/// complete top-level object scanned over the whole text, de-duplicated.
/// Test: exercised via [`parse_action`] in `chat_action_tests.rs`.
fn candidate_objects(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(inner) = fenced_inner(text)
        && let Some((obj, _)) = next_balanced_object(&inner, 0)
    {
        candidates.push(obj);
    }
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
/// Why: the model may wrap its action in a ```json (or bare ```) fence; isolating
/// fence handling keeps [`candidate_objects`] readable.
/// What: finds a ```json opening (else a bare ```), skips to the end of that line,
/// and returns the substring up to the next ``` (or end of text). `None` when
/// there is no opening fence.
/// Test: `chat_action_tests.rs::parse_fenced_verb`.
fn fenced_inner(text: &str) -> Option<String> {
    const JSON_FENCE: &str = "```json";
    const BARE_FENCE: &str = "```";
    let (open_at, fence_len) = match text.find(JSON_FENCE) {
        Some(i) => (i, JSON_FENCE.len()),
        None => (text.find(BARE_FENCE)?, BARE_FENCE.len()),
    };
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

/// Find the next COMPLETE top-level `{ … }` object at or after `from`.
///
/// Why: a depth-tracking scan that honors JSON string literals + escapes finds
/// each well-formed top-level object in turn, so prose braces before/after the
/// real object (or braces inside string values) do not corrupt the span.
/// What: from the first `{` at/after `from`, tracks brace depth outside string
/// literals and returns `(object_span, end_offset)` when depth returns to zero;
/// `None` if there is no `{` or it never closes.
/// Test: exercised via [`parse_action`] in `chat_action_tests.rs`.
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

/// Render the action-chat instruction block advertising the inline verb surface.
///
/// Why: the chat agent is "self-aware" — it must know the full managed-session
/// verb catalog and that those verbs execute INLINE in this turn (not as
/// background delegation). The verb list is rendered from the
/// [`catalog`](crate::client::catalog) single source of truth so it can never
/// drift from the executable surface; this is SEPARATE from the goal-shaped
/// `DECISION_INSTRUCTIONS` (delegate/respond), which must NOT be reused here.
/// What: returns a fixed prose block that (1) lists every
/// [`sm_session_verbs`] verb as a callable signature, (2) defines the two
/// response shapes — a verb-call `{"action":"<verb>","args":{…}}` and a final
/// answer `{"action":"final","message":"…"}` — and (3) states the verbs run
/// inline this turn and are read→decide→execute→feed-back.
/// Test: `chat_action_tests.rs::prompt_lists_every_catalog_verb`.
pub fn action_instructions() -> String {
    let mut out = String::from(
        "# INLINE ACTIONS (machine-read)\n\n\
         You can INVOKE managed-session verbs INLINE within this single chat turn. \
         A verb you call EXECUTES NOW (synchronously, in-process) and its JSON result \
         is fed straight back to you so you can decide your next step. This is NOT \
         background delegation: there is no tracking goal, you act and observe in this \
         very turn.\n\n\
         Reply with EXACTLY ONE JSON object (optionally inside a ```json fence) and no \
         other text. Choose ONE of two shapes:\n\n\
         1. Call a verb (it runs now; you will see its result and may call another):\n   \
         {\"action\":\"<verb>\",\"args\":{ … }}\n\n\
         2. Give your FINAL answer to the operator (ends the turn):\n   \
         {\"action\":\"final\",\"message\":\"<operator-facing reply>\"}\n\n\
         The verbs available to you (call by exact name):\n",
    );
    for spec in sm_session_verbs() {
        out.push_str(&format!(
            "- `{}` — {}\n",
            spec.prompt_signature(),
            spec.summary
        ));
    }
    out.push_str(
        "\nArgument conventions: pass a session id as `args.session_id`; \
         `sessions.send` also takes `args.text`; `sessions.launch` takes \
         `args.workdir` (required) plus optional `args.model`, `args.prompt`, \
         `args.goal_id`. When you have gathered enough to answer, emit the `final` \
         shape — do not keep calling verbs once you can answer.",
    );
    out
}

/// Execute one chosen verb against the in-process [`SessionControl`].
///
/// Why: the action loop maps the model's chosen verb name onto the matching
/// `SessionControl` method and runs it IN-PROCESS (never over HTTP back to the
/// daemon, which would loop back on itself). Centralising the verb→method mapping
/// here keeps the loop driver thin and the catalog-name → method table in one
/// auditable place.
/// What: dispatches `verb` (a catalog `sessions.*` name) to the corresponding
/// `control` method, parsing `args` for the ids/text/launch params each needs;
/// returns the verb's JSON result. An unknown verb is a
/// [`SessionControlError::Backend`] so the loop can feed the error back to the
/// model rather than panicking.
/// Test: `chat_action_tests.rs::execute_dispatches_list`, `execute_send_reads_args`,
/// `execute_unknown_verb_errors`.
pub async fn execute_verb(
    control: &Arc<dyn SessionControl>,
    verb: &str,
    args: &Value,
) -> Result<Value, SessionControlError> {
    match verb {
        "sessions.list" => control.list().await,
        "sessions.get" => control.get(require_session_id(args)?).await,
        "sessions.send" => {
            let id = require_session_id(args)?;
            let text = args.get("text").and_then(Value::as_str).ok_or_else(|| {
                SessionControlError::Backend("sessions.send requires args.text".to_string())
            })?;
            control.send(id, text).await
        }
        "sessions.stop" => control.stop(require_session_id(args)?).await,
        "sessions.resume" => control.resume(require_session_id(args)?).await,
        "sessions.kill" => control.kill(require_session_id(args)?).await,
        "sessions.launch" => {
            let workdir = args
                .get("workdir")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SessionControlError::Backend(
                        "sessions.launch requires args.workdir".to_string(),
                    )
                })?
                .to_string();
            let params = LaunchParams {
                workdir,
                model: str_arg(args, "model"),
                prompt: str_arg(args, "prompt"),
                goal_id: str_arg(args, "goal_id"),
            };
            control.launch(params).await
        }
        other => Err(SessionControlError::Backend(format!(
            "unknown verb `{other}`; valid verbs: {}",
            valid_verb_names()
        ))),
    }
}

/// Whether `action` names the loop-terminating final answer.
///
/// Why: the loop driver checks the decision's tag to decide whether to execute a
/// verb or stop; centralising the sentinel keeps the magic string in one place.
/// What: returns `true` iff `action` equals the `"final"` sentinel.
/// Test: covered transitively by the loop tests (`parse_final_answer`).
pub fn is_final_action(action: &str) -> bool {
    action == FINAL_ACTION
}

/// Extract a required `args.session_id` string, or a typed error.
///
/// Why: every per-session verb needs an id; a missing/blank id is fed back to the
/// model as an error rather than panicking.
/// What: returns `args.session_id` as `&str`, or [`SessionControlError::NotFound`].
/// Test: `chat_action_tests.rs::execute_get_requires_id`.
fn require_session_id(args: &Value) -> Result<&str, SessionControlError> {
    args.get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| SessionControlError::NotFound("missing args.session_id".to_string()))
}

/// Read an optional string arg, treating blank as absent.
fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// A comma-joined list of valid verb names for the unknown-verb error.
fn valid_verb_names() -> String {
    sm_session_verbs()
        .iter()
        .map(|c| c.name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "chat_action_tests.rs"]
mod chat_action_tests;
