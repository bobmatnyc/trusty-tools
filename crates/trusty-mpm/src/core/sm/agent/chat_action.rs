//! The action-capable SM chat turn: inline verb execution within one turn (#1283).
//!
//! Why: Phase 2 makes the coordinator chat ACTION-CAPABLE. The text-only
//! [`chat`](super::chat) turn relays a single reply; this turn lets the SM
//! INVOKE managed-session verbs INLINE — a synchronous read→decide→execute→
//! feed-back→answer loop — so the operator can say "stop the stuck session" and
//! the SM actually does it within the turn. It is deliberately SEPARATE from the
//! goal-shaped background delegation of [`super::delegate`] (which launches
//! sessions and returns a tracking outcome): here verbs run NOW, in-process,
//! against the injected [`SessionControl`], and their results steer the next
//! step. Keeping the loop in its own file (the parse/prompt/dispatch helpers live
//! in [`super::chat_action_protocol`]) keeps every file under the SLOC cap.
//! What: [`SmChatActionOutcome`] (reply + conv_id + cost + the human-readable
//! list of verbs invoked) and the `impl` block adding
//! [`super::SessionManagerAgent::chat_with_actions`] — the bounded loop that
//! composes the §7.5 working prompt + the action-instruction block, calls the
//! orchestration provider, parses each reply into a verb-call or a final answer,
//! executes verbs against the control surface, accumulates a transcript, and
//! terminates on a final answer or the max-iteration guard.
//! Test: `chat_action_tests.rs` — one-verb-then-final, the max-iteration guard,
//! the prose-fallback (no verb), and degraded provider.

use std::sync::Arc;

use uuid::Uuid;

use crate::core::sm::context::SmContextEngine;
use crate::core::sm::control::SessionControl;
use crate::core::sm::prompt::resolve_sm_prompt_default;
use crate::core::sm::providers::{ChatMessage, LlmRequest, SmModelTier};

use super::SessionManagerAgent;
use super::chat::{SmAgentError, degraded_notice, map_resolve_error};
use super::chat_action_protocol::{Decided, action_instructions, execute_verb, parse_action};

/// Hard ceiling on action-loop iterations within ONE chat turn (#1283 safety).
///
/// Why: the loop runs inside the HTTP handler; an unbounded read→act loop could
/// spin forever (a model that always emits a verb-call). A small ceiling bounds
/// the turn's cost and latency while leaving ample room for a multi-step
/// observe→act→answer sequence.
/// What: when the loop reaches this many provider calls without a final answer,
/// it forces a final-answer synthesis instead of calling another verb.
/// Test: `chat_action_tests.rs::max_iterations_guard_terminates`.
const MAX_ACTION_ITERATIONS: usize = 8;

/// Generation cap for one action-loop reply (mirrors the chat turn's ceiling).
const ACTION_MAX_TOKENS: u32 = 4_096;

/// Max chars of a single verb result rendered into the fed-back transcript.
///
/// Why: a verb result (e.g. a large `sessions.list`) is embedded raw into the
/// transcript line that is fed back to the model on the next iteration. An
/// unbounded result would grow the prompt on every iteration and can overflow the
/// model's context window. Capping each rendered result bounds total transcript
/// growth to `MAX_ACTION_ITERATIONS × ACTION_RESULT_MAX_CHARS`.
/// What: results longer than this are truncated on a char boundary with a clear
/// `… [truncated N chars]` marker appended.
/// Test: `chat_action_loop_tests.rs::large_verb_result_is_truncated`.
const ACTION_RESULT_MAX_CHARS: usize = 2_000;

/// The result of one action-capable SM chat turn (#1283).
///
/// Why: the endpoint needs the reply, the conversation id (so a follow-up can
/// continue the rolling context), the summed per-turn cost (every provider call
/// in the loop), AND an audit trail of which verbs actually executed — the safety
/// requirement that the operator can always see what the SM did on their behalf.
/// What: `reply` is the final operator-facing text; `conv_id` is the (possibly
/// minted) conversation id; `cost_usd` sums every `complete()` call this turn;
/// `actions_taken` lists each invoked verb in order (e.g. `["sessions.list",
/// "sessions.send abc123"]`).
/// Test: `chat_action_tests.rs::loop_executes_verb_then_answers`.
#[derive(Debug, Clone, PartialEq)]
pub struct SmChatActionOutcome {
    /// The assistant's final reply text.
    pub reply: String,
    /// The conversation id this turn used (echoed so callers can continue it).
    pub conv_id: String,
    /// Summed estimated USD cost of every provider call in the loop (§5.5).
    pub cost_usd: f64,
    /// Human-readable list of the verbs invoked this turn, in order.
    pub actions_taken: Vec<String>,
}

impl SessionManagerAgent {
    /// Drive one ACTION-CAPABLE SM chat turn: invoke verbs inline, then answer.
    ///
    /// Why: this is Phase 2's headline deliverable — a self-aware chat turn that
    /// knows the managed-session verb catalog and can EXECUTE those verbs inline
    /// (read→decide→execute→feed-back→answer), as opposed to the text-only
    /// [`chat`](super::chat) path. Verbs run IN-PROCESS through the injected
    /// `control` (never over HTTP back to the daemon), so there is no loopback.
    /// What: (1) resolves the conversation id; (2) opens the per-`conv_id`
    /// [`SmContextEngine`]; (3) recalls SM-palace memory; (4) composes the working
    /// prompt = SM system prompt + the [`action_instructions`] block (verb list
    /// rendered from the catalog SoT) + the engine's §7.5 context + the message;
    /// (5) loops up to [`MAX_ACTION_ITERATIONS`]: call the orchestration provider,
    /// [`parse_action`] the reply, and on a verb-call [`execute_verb`] against
    /// `control` and append a `(verb-call → result)` pair to a per-turn transcript
    /// that is fed back as the next user turn; (6) on a final answer (or when the
    /// guard trips) records the round and returns the reply, conv_id, summed cost,
    /// and the audited `actions_taken`. With no runtime (the inert agent) returns
    /// [`SmAgentError::Degraded`].
    /// Test: `chat_action_tests.rs` — verb-then-final, the max-iter guard,
    /// prose-fallback, and degraded.
    pub async fn chat_with_actions(
        &self,
        message: &str,
        conv_id: Option<&str>,
        control: &Arc<dyn SessionControl>,
    ) -> Result<SmChatActionOutcome, SmAgentError> {
        let Some(runtime) = self.runtime_ref() else {
            return Err(SmAgentError::Degraded(degraded_notice()));
        };

        let conv_id = conv_id
            .filter(|c| !c.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let inference = &self.config().inference;
        let rounds = &self.config().rounds;

        // #1309: serialize turns for the SAME conv_id across the whole turn
        // (open → record → save) so a concurrent turn cannot last-write-wins on the
        // context-engine state file and lose this turn's round. Different conv_ids
        // never block one another.
        let _turn = self.conv_locks.acquire(&conv_id).await;

        let mut engine = SmContextEngine::open(&conv_id, &runtime.data_root, inference, rounds)?;

        // System prompt = the SM identity/floor + the inline-action instructions
        // (verb surface rendered from the catalog single source of truth).
        let system_prompt = format!(
            "{}\n\n---\n\n{}",
            resolve_sm_prompt_default(),
            action_instructions()
        );

        let recall = self.delegate_recall(runtime, message).await;

        // Assemble the §7.5 base prompt once; the per-iteration transcript is
        // appended as a trailing synthetic user turn so the model always sees the
        // verb results it has gathered so far this turn.
        let base = engine.assemble_working_prompt(&system_prompt, recall.as_deref(), message);
        let (system, base_turns) = split_system_message(base);

        let mut transcript: Vec<String> = Vec::new();
        let mut actions_taken: Vec<String> = Vec::new();
        let mut total_cost = 0.0_f64;
        let mut final_reply: Option<String> = None;

        for iteration in 0..MAX_ACTION_ITERATIONS {
            let is_final_iteration = iteration + 1 == MAX_ACTION_ITERATIONS;

            let mut turns = base_turns.clone();
            if !transcript.is_empty() {
                turns.push(ChatMessage {
                    role: "user".to_string(),
                    content: format!(
                        "Verb results so far this turn:\n{}\n\nDecide your next action \
                         (call another verb, or give your final answer).",
                        transcript.join("\n")
                    ),
                });
            }
            // On the last allowed iteration, tell the model to stop calling verbs so
            // it has the chance to yield a real `final` reply. This is only a HINT —
            // a misbehaving model may still emit a verb-call, which is handled below
            // by NOT executing it (see the `is_final_iteration` branch).
            if is_final_iteration {
                turns.push(ChatMessage {
                    role: "user".to_string(),
                    content: "You have reached the action limit for this turn. Do NOT \
                              call another verb — reply now with the `final` answer shape."
                        .to_string(),
                });
            }

            let resolved = runtime
                .resolver
                .resolve(inference, SmModelTier::Orchestration)
                .await
                .map_err(map_resolve_error)?;
            let req = LlmRequest {
                model: resolved.model.clone(),
                system: system.clone(),
                messages: turns,
                temperature: inference.temperature,
                max_tokens: ACTION_MAX_TOKENS,
            };
            let response = resolved
                .provider
                .complete(req)
                .await
                .map_err(SmAgentError::Inference)?;
            total_cost += response.cost_usd;

            match parse_action(&response.text) {
                Decided::Final { message } => {
                    final_reply = Some(message);
                    break;
                }
                Decided::Verb { verb, args } => {
                    // The action cap is HARD: on the final iteration we must NOT
                    // execute another verb (that would run one step beyond the cap).
                    // A verb-call here means the model ignored the stop-hint, so we
                    // break to the forced-summary path without executing it — the cap
                    // bounds verbs at exactly MAX_ACTION_ITERATIONS - 1 executions.
                    if is_final_iteration {
                        break;
                    }
                    let (label, result_line) = run_one_verb(control, &verb, &args).await;
                    actions_taken.push(label);
                    transcript.push(result_line);
                }
            }
        }

        // If the loop ended without a `final` answer — either the model never
        // emitted one, or it emitted a verb-call on the capped final iteration
        // (which we refuse to execute) — synthesize a transcript summary rather than
        // spin or panic. This ALWAYS yields an operator-facing reply.
        let reply = final_reply.unwrap_or_else(|| forced_summary(&transcript));

        // Record the round (message + final reply) so the action turn persists
        // exactly like the text-only chat turn (data-integrity invariant).
        self.record_round(runtime, &mut engine, message, &reply)
            .await?;

        Ok(SmChatActionOutcome {
            reply,
            conv_id,
            cost_usd: total_cost,
            actions_taken,
        })
    }
}

/// Execute one verb and render its `(audit label, transcript line)` pair.
///
/// Why: keeps the loop body terse and gives the verb-result rendering — both the
/// human-readable `actions_taken` label and the JSON line fed back to the model —
/// one home. A verb error is NOT fatal: it is fed back to the model (so it can
/// recover or answer) and recorded in the audit trail, never panicking.
/// What: runs [`execute_verb`]; on success returns `(label, "<verb> -> <json>")`;
/// on error returns `(label, "<verb> -> error: <e>")`. The rendered result is
/// bounded by [`truncate_result`] so a large verb output cannot grow the prompt
/// unbounded. The label appends the session id (when present in args) so the
/// operator sees the target.
/// Test: `chat_action_tests.rs::loop_executes_verb_then_answers`,
/// `chat_action_loop_tests.rs::large_verb_result_is_truncated`.
async fn run_one_verb(
    control: &Arc<dyn SessionControl>,
    verb: &str,
    args: &serde_json::Value,
) -> (String, String) {
    let label = match args.get("session_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => format!("{verb} {id}"),
        _ => verb.to_string(),
    };
    match execute_verb(control, verb, args).await {
        Ok(result) => (
            label,
            format!("{verb} -> {}", truncate_result(&result.to_string())),
        ),
        Err(e) => (
            label,
            format!("{verb} -> error: {}", truncate_result(&e.to_string())),
        ),
    }
}

/// Bound a rendered verb result to [`ACTION_RESULT_MAX_CHARS`] on a char boundary.
///
/// Why: each verb result is fed back into the next iteration's prompt; without a
/// cap a single large result (a long `sessions.list`) grows the prompt unbounded
/// and can overflow the model's context window on later iterations.
/// What: returns `rendered` unchanged when it is within the cap; otherwise returns
/// the first [`ACTION_RESULT_MAX_CHARS`] characters (split on a char boundary so
/// multibyte input never panics) followed by a `… [truncated N chars]` marker
/// where `N` is the number of characters dropped.
/// Test: `chat_action_loop_tests.rs::large_verb_result_is_truncated`,
/// `truncate_result_keeps_short_input` and `truncate_result_is_char_safe`.
fn truncate_result(rendered: &str) -> String {
    let total = rendered.chars().count();
    if total <= ACTION_RESULT_MAX_CHARS {
        return rendered.to_string();
    }
    let kept: String = rendered.chars().take(ACTION_RESULT_MAX_CHARS).collect();
    let dropped = total - ACTION_RESULT_MAX_CHARS;
    format!("{kept}… [truncated {dropped} chars]")
}

/// Synthesize a final reply from the transcript when the model produced none.
///
/// Why: the max-iteration guard must ALWAYS yield an operator-facing reply — even
/// in the pathological case where the model never emits a `final` shape — rather
/// than returning an empty string or spinning.
/// What: returns a short note plus the accumulated verb transcript (or a generic
/// "no actions" note when the transcript is empty).
/// Test: `chat_action_tests.rs::max_iterations_guard_terminates`.
fn forced_summary(transcript: &[String]) -> String {
    if transcript.is_empty() {
        "I reached the action limit for this turn without completing the request.".to_string()
    } else {
        format!(
            "I reached the action limit for this turn. Here is what I did:\n{}",
            transcript.join("\n")
        )
    }
}

/// Split the assembled §7.5 messages into the out-of-band system string and turns.
///
/// Why: [`SmContextEngine::assemble_working_prompt`] consolidates the system
/// sections into ONE leading `system`-role message, but [`LlmRequest`] carries
/// the system prompt as a dedicated field. This adapts one shape to the other
/// (the same split the text-only chat turn performs).
/// What: if the first message is `system`-role, returns `(its content, the rest)`;
/// otherwise `(String::new(), all messages)`.
/// Test: covered via the loop tests (the mock asserts the system prompt).
fn split_system_message(mut messages: Vec<ChatMessage>) -> (String, Vec<ChatMessage>) {
    if messages.first().is_some_and(|m| m.role == "system") {
        let system = messages.remove(0).content;
        (system, messages)
    } else {
        (String::new(), messages)
    }
}

#[cfg(test)]
#[path = "chat_action_loop_tests.rs"]
mod chat_action_loop_tests;
