//! The 6-phase SM↔session delegation loop (DOC-14 §3.4) — SM-8 capstone.
//!
//! Why: this is the mechanism that brings the Session Manager to life. SM-1..SM-7
//! built the parts (config, multi-provider inference, system prompt, memory,
//! rolling context, goal store, chat turn, stdio surface); SM-8 wires them into
//! the spec's prime directive (§3.1): the SM does NO work itself — it turns an
//! operator goal into a tracked [`Goal`](crate::core::sm::Goal), DECOMPOSEs it
//! into session-sized tasks, LAUNCHes one session per task, OBSERVEs them,
//! VERIFIes with observed evidence (the §3.5 BLOCKING gate), and REPORTs +
//! PERSISTs. The only action surfaces the loop touches are session control
//! ([`SessionControl`]), goals ([`SmGoalStore`]), and memory — there are NO
//! edit/read-source/build/test tools, so the prohibitions (§3.2 SP1–SP7) are
//! enforced STRUCTURALLY; on top of that, a direct-work *attempt* by the model is
//! refused and redirected (the SP guard, [`verify::redirect_direct_work`]).
//!
//! DECISION PROTOCOL (NORMATIVE for this impl): the SM provider is text-only
//! (`complete()`, no tool/function calls — see `providers/mod.rs`), so the loop
//! uses a STRUCTURED-TEXT (ReAct-style) protocol: the SM emits ONE JSON action
//! block at DECOMPOSE which [`decision::parse_decision`] turns into a typed
//! [`SmDecision`]; the engine then mechanically executes launch→observe→verify.
//! Observation/verification need NO LLM (deterministic pane heuristics, §3.4),
//! keeping the whole loop unit-testable with a mock provider + mock control.
//!
//! CONCURRENCY (#1309): this loop drives the first code that fans out to multiple
//! sessions and makes repeated goal-store writes. Goal-store mutations are already
//! ATOMIC and serialized — every mutator goes through the single
//! `Arc<Mutex<SmGoalStore>>` and SM-6 snapshots+rolls-back on a failed persist —
//! so the loop introduces NO new last-write-wins race in the goal store. The loop
//! does NOT touch the per-`conv_id` rolling-context engine (that path stays in
//! `chat`), so it does not widen the #1309 context-engine race either. TODO(#1309):
//! if `delegate_goal` ever becomes reachable concurrently for the SAME goal (e.g.
//! re-driven by two operator turns), add a per-goal serialization seam here; today
//! each call creates a fresh goal at INTAKE, so concurrent calls operate on
//! disjoint goals and cannot interleave.
//!
//! What: [`DelegationOutcome`] (the loop result), [`DelegationError`] (typed
//! failures), and the `impl` block adding
//! [`SessionManagerAgent::delegate_goal`]. Split into `decision` (the action
//! protocol), `observe` (pane interpretation + evidence), and `verify` (the gate
//! plus the SP guard) to respect the 500-SLOC cap.
//! Test: `delegate_tests.rs` — launch+link, observe→progress, the verification
//! gate (no Done without evidence), the prohibition redirect, and task delivery
//! (#1299).

mod decision;
mod observe;
mod verify;

#[cfg(test)]
pub mod mock_control;

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::core::sm::control::{LaunchParams, SessionControl};
use crate::core::sm::goals::{GoalStatus, SessionLink, SessionUpdate, SmGoalError, SmGoalStore};
use crate::core::sm::providers::{LlmRequest, SmModelTier};

use super::SessionManagerAgent;

pub use decision::{SmDecision, TaskSpec, parse_decision};
use observe::interpret_session;
use verify::redirect_direct_work;

/// Generation cap for the SM's DECOMPOSE decision reply.
///
/// Why: the decompose step emits a compact JSON action block, not prose; a tight
/// ceiling keeps the turn cheap and predictable (§5.4).
/// What: passed as [`LlmRequest::max_tokens`] for the decompose call.
/// Test: `delegate_tests.rs` asserts the mock saw a bounded request.
const DECOMPOSE_MAX_TOKENS: u32 = 2_048;

/// The result of running the delegation loop for one operator goal (§3.4).
///
/// Why: the caller (the stdio/chat surface) needs the operator-facing reply, the
/// tracked goal id (so follow-ups can reference it), and an auditable record of
/// what the loop did — which sessions launched, and whether the goal closed
/// through the verification gate. Returning all of it as one value keeps the call
/// site terse and the behaviour assertable.
/// What: `reply` is the status-first operator message; `goal_id` is the tracked
/// goal; `launched` are the session ids spawned; `goal_done` is whether the
/// verification gate passed and the goal closed; `goal_status` is the goal's actual
/// lifecycle label (`Pending`/`InProgress`/`Blocked`/`Done`/`Abandoned`).
/// Test: `delegate_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationOutcome {
    /// The operator-facing, status-first reply (§3.6).
    pub reply: String,
    /// The tracked goal this loop created/advanced.
    pub goal_id: String,
    /// The session ids launched for this goal (one per decomposed task).
    pub launched: Vec<String>,
    /// Whether the verification gate passed and the goal reached `Done` (§3.5).
    pub goal_done: bool,
    /// The goal's actual lifecycle status label (§9.1).
    ///
    /// Why: `goal_done` alone is AMBIGUOUS — a caller can misread
    /// `goal_done == false` as "failed" when it really means "still in progress"
    /// (or "blocked", awaiting an operator decision). Carrying the real status
    /// label lets the caller distinguish these WITHOUT a follow-up `sm.goals.list`.
    /// It is additive: `goal_done` stays for back-compat.
    /// What: the PascalCase label from
    /// [`GoalStatus::label`](crate::core::sm::GoalStatus::label) read back from the
    /// store after the loop (falling back to `"Pending"` only if the goal vanished).
    pub goal_status: String,
}

/// A failure surfaced by [`SessionManagerAgent::delegate_goal`].
///
/// Why: the caller must distinguish a graceful *degraded* state (no provider for
/// the decompose reasoning — §5.3) from a goal-store failure or a session-control
/// failure, so it can render each appropriately and stay panic-free.
/// What: [`Degraded`](DelegationError::Degraded) — no inference for decompose;
/// [`Goal`](DelegationError::Goal) — a goal-store error (create/link/update/gate);
/// [`Control`](DelegationError::Control) — a session-control error during launch.
/// Test: `delegate_tests.rs::delegate_degraded_without_provider`.
#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    /// No inference provider resolved for the DECOMPOSE reasoning (graceful).
    #[error("{0}")]
    Degraded(String),

    /// A goal-store operation failed (create/link/update/close gate). This is the
    /// only fatal class: the goal store is the SM's source of truth, so a create
    /// failure (no tracked goal) cannot be recovered. Session-control failures, by
    /// contrast, are best-effort per task (a launch/deliver failure for one task is
    /// noted and the fan-out continues — see [`SessionManagerAgent::launch_tasks`]).
    #[error("session-manager goal store failed: {0}")]
    Goal(#[from] SmGoalError),
}

impl SessionManagerAgent {
    /// Run the 6-phase delegation loop for one operator goal (§3.4) — SM-8.
    ///
    /// Why: the capstone — the single entry the stdio/chat surface calls to turn
    /// operator intent into delegated, verified work. It enforces the prime
    /// directive (§3.1): the SM delegates everything; the only side effects are
    /// launching sessions, tracking goals, and writing memory. The verification
    /// gate (§3.5) and the prohibition guard (§3.2) are applied here.
    /// What: (1) INTAKE — create a tracked goal from `message` (recall is folded
    /// into the decompose prompt, best-effort); (2) DECOMPOSE — assemble the SM
    /// system prompt + the decision instructions + the goal, call the
    /// orchestration-tier provider, and parse a typed [`SmDecision`]; a
    /// [`SmDecision::DoWork`] is REFUSED and redirected (the SP guard), a
    /// [`SmDecision::Respond`] returns the operator message with the goal still
    /// `Pending`; (3) LAUNCH — for each launchable [`TaskSpec`] spawn a session via
    /// `control`, link it to the goal, and DELIVER the task prompt via
    /// `control.send` (#1299); (4) OBSERVE — poll each session and update its link
    /// state + the goal progress; (5) VERIFY — mark a link `Verified` ONLY with
    /// observed evidence, then attempt the gated close (§3.5); (6) REPORT —
    /// summarize, status-first. A no-provider decompose returns
    /// [`DelegationError::Degraded`].
    ///
    /// SCOPE NOTE (single-pass observation): SM-8 delivers the loop MECHANISM with
    /// ONE observation pass — it polls each launched session once (phase 4) and
    /// applies the gate (phase 5) synchronously. A goal therefore reaches `Done` in
    /// this call only if its session(s) already carry evidence at observe time
    /// (e.g. a fast/mocked session, or a re-driven goal). Long-running sessions
    /// stay `InProgress` and are re-observed on a later drive. A continuous
    /// poll-until-terminal observer (and a resume path that re-attaches to an open
    /// goal instead of creating a new one) is deliberate follow-up — the gate and
    /// evidence wiring here are what make that future loop correct.
    /// Test: `delegate_tests.rs` — the full loop, the gate, the SP redirect, and
    /// task delivery.
    pub async fn delegate_goal(
        &self,
        message: &str,
        control: &Arc<dyn SessionControl>,
        goals: &Arc<Mutex<SmGoalStore>>,
    ) -> Result<DelegationOutcome, DelegationError> {
        // ── (1) INTAKE — create the tracked goal. ───────────────────────────────
        let goal_id = {
            let mut store = goals.lock().await;
            store.create(message.to_string(), Vec::new()).await?.id
        };

        // ── (2) DECOMPOSE — ask the SM for a typed decision. ────────────────────
        let decision = self.decompose(message, &goal_id, goals).await?;
        let tasks = match decision {
            SmDecision::Delegate { tasks } => tasks,
            SmDecision::Respond { message } => {
                // Allowlist 1: the SM is talking to the operator; the goal stays
                // Pending with no fan-out (nothing delegated this turn).
                let goal_status = self.goal_status_label(&goal_id, goals).await;
                return Ok(DelegationOutcome {
                    reply: message,
                    goal_id,
                    launched: Vec::new(),
                    goal_done: false,
                    goal_status,
                });
            }
            SmDecision::DoWork { summary } => {
                // §3.2 SP guard: a direct-work attempt is REFUSED and redirected
                // to "launch a session". Record the refusal as a goal note so the
                // attempt is auditable, mark the goal Blocked (awaiting the operator
                // to re-issue it as a delegation — it is NOT progressing on its own,
                // so leaving it Pending forever would orphan it), then return the
                // redirect message.
                let reply = redirect_direct_work(&summary);
                let goal_status = {
                    let mut store = goals.lock().await;
                    let _ = store.note(&goal_id, reply.clone()).await;
                    let _ = store.set_status(&goal_id, GoalStatus::Blocked).await;
                    store
                        .get(&goal_id)
                        .map(|g| g.status.label().to_string())
                        .unwrap_or_else(|| GoalStatus::Blocked.label().to_string())
                };
                return Ok(DelegationOutcome {
                    reply,
                    goal_id,
                    launched: Vec::new(),
                    goal_done: false,
                    goal_status,
                });
            }
        };

        // ── (3) LAUNCH — one session per launchable task; link + DELIVER (#1299).
        let launched = self.launch_tasks(&tasks, &goal_id, control, goals).await;
        if launched.is_empty() {
            let goal_status = {
                let mut store = goals.lock().await;
                let _ = store
                    .note(
                        &goal_id,
                        "decompose produced no launchable task".to_string(),
                    )
                    .await;
                store
                    .get(&goal_id)
                    .map(|g| g.status.label().to_string())
                    .unwrap_or_else(|| GoalStatus::Pending.label().to_string())
            };
            return Ok(DelegationOutcome {
                reply: "No launchable task was produced for this goal; nothing delegated."
                    .to_string(),
                goal_id,
                launched,
                goal_done: false,
                goal_status,
            });
        }

        // ── (4)+(5) OBSERVE + VERIFY — poll, update progress, apply the gate. ───
        self.observe_and_verify(&launched, &goal_id, control, goals)
            .await?;

        // Attempt the BLOCKING gated close (§3.5): succeeds ONLY when every linked
        // task is Verified (with evidence). A VerificationGate rejection is NOT an
        // error — the goal simply stays open and the report says so. But a real
        // persistence failure (palace/cache) must NOT be silently conflated with a
        // benign gate rejection: it is logged so the infra failure is visible, while
        // the loop still reports the goal as not-done (it could not be durably closed).
        let goal_done = {
            let mut store = goals.lock().await;
            match store.close(&goal_id).await {
                Ok(_) => true,
                Err(SmGoalError::VerificationGate { .. }) => false,
                Err(e) => {
                    tracing::warn!(%goal_id, "delegate: gated close failed (not a gate rejection): {e}");
                    false
                }
            }
        };

        // ── (6) REPORT — status-first summary for the operator. ─────────────────
        let reply = self.report(&goal_id, &launched, goal_done, goals).await;
        let goal_status = self.goal_status_label(&goal_id, goals).await;
        Ok(DelegationOutcome {
            reply,
            goal_id,
            launched,
            goal_done,
            goal_status,
        })
    }

    /// DECOMPOSE: assemble the decision prompt, call the provider, parse a decision.
    ///
    /// Why: phase 2 is the only LLM call in the loop; isolating it keeps
    /// `delegate_goal` readable and the prompt assembly auditable. The SM is
    /// instructed to emit ONE JSON action block (the structured-text protocol);
    /// recall (SM-4) is folded in best-effort to inform decomposition.
    /// What: resolves the orchestration tier (degraded → [`DelegationError::Degraded`]),
    /// builds the system prompt + decision instructions + recall + the goal, calls
    /// `complete`, and returns the parsed [`SmDecision`]. The decompose round is
    /// NOT recorded into the rolling-context engine here (the loop is a single
    /// goal-scoped action, not a conversational turn — context recording stays in
    /// `chat`).
    /// Test: `delegate_tests.rs` (every decision path), `delegate_degraded_*`.
    async fn decompose(
        &self,
        message: &str,
        goal_id: &str,
        goals: &Arc<Mutex<SmGoalStore>>,
    ) -> Result<SmDecision, DelegationError> {
        let runtime = self
            .runtime_ref()
            .ok_or_else(|| DelegationError::Degraded(super::chat::degraded_notice()))?;

        let resolved = runtime
            .resolver
            .resolve(&self.config.inference, SmModelTier::Orchestration)
            .await
            .map_err(|e| {
                if e.is_degraded() {
                    DelegationError::Degraded(super::chat::degraded_notice())
                } else {
                    DelegationError::Degraded(e.to_string())
                }
            })?;

        let recall = self.delegate_recall(runtime, message).await;
        let system = decision_system_prompt();
        let user = decision_user_prompt(message, goal_id, recall.as_deref());

        let req = LlmRequest {
            model: resolved.model.clone(),
            system,
            messages: vec![crate::core::sm::providers::ChatMessage {
                role: "user".to_string(),
                content: user,
            }],
            temperature: self.config.inference.temperature,
            max_tokens: DECOMPOSE_MAX_TOKENS,
        };
        let response = resolved
            .provider
            .complete(req)
            .await
            .map_err(|e| DelegationError::Degraded(e.to_string()))?;

        let decision = parse_decision(&response.text);
        // Record the decision as a goal note for auditability (best-effort).
        {
            let mut store = goals.lock().await;
            let _ = store
                .note(goal_id, format!("decompose decision: {decision:?}"))
                .await;
        }
        Ok(decision)
    }

    /// LAUNCH: spawn one session per launchable task, link it, and DELIVER it.
    ///
    /// Why: phase 3 turns each [`TaskSpec`] into exactly one launched, linked,
    /// and TASK-DELIVERED session. Delivery (#1299) is explicit: `tm ticket`
    /// spawn does NOT auto-inject the prompt into the pane, so the SM must
    /// `sessions.send` the task after launch or the session sits idle. We do that
    /// here so a launched session actually receives its work.
    /// What: for each launchable task, calls `control.launch`; a launch FAILURE is
    /// best-effort (noted on the goal, the fan-out continues with the remaining
    /// tasks — aborting would orphan sessions already launched). On success it
    /// extracts the `session_id` (skipping with a note + warning if the control
    /// surface returned no id), links it to the goal via `store.link` (best-effort —
    /// a link failure is noted, never fatal), then `control.send`s the task prompt
    /// to the session (best-effort delivery — a send failure is noted). Returns the
    /// successfully launched session ids (infallible — no fatal control error).
    /// Test: `delegate_tests.rs::delegate_launches_and_links`,
    /// `delegate_delivers_task_to_session`.
    async fn launch_tasks(
        &self,
        tasks: &[TaskSpec],
        goal_id: &str,
        control: &Arc<dyn SessionControl>,
        goals: &Arc<Mutex<SmGoalStore>>,
    ) -> Vec<String> {
        let mut launched = Vec::new();
        // WARNING (future editors): the goal-store lock (`goals.lock().await`) MUST
        // be DROPPED before any `control.*` await below. Every store access in this
        // loop is taken in its OWN tight scope so the `MutexGuard` is released before
        // the next `control.launch`/`control.send` await — holding it across that
        // async I/O would serialize the whole fan-out and risks a deadlock if the
        // control surface ever re-enters the goal store. Keep the guards scoped.
        for task in tasks.iter().filter(|t| t.is_launchable()) {
            let params = LaunchParams {
                workdir: task.workdir.clone(),
                model: task.model.clone(),
                prompt: Some(task.prompt.clone()),
                goal_id: Some(goal_id.to_string()),
            };
            // Per-task best-effort: a single launch failure must NOT abort the whole
            // fan-out (which would orphan sessions already launched for earlier
            // tasks). Record it as a goal note and continue with the remaining tasks.
            let result = match control.launch(params).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(%goal_id, "delegate: launch failed for a task: {e}");
                    let mut store = goals.lock().await;
                    let _ = store
                        .note(goal_id, format!("launch failed for a task: {e}"))
                        .await;
                    continue;
                }
            };
            let session_id = result
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if session_id.is_empty() {
                // The control surface returned Ok but no string session id. We cannot
                // link/deliver/track without one; warn so an out-of-contract impl
                // (or a spawned-but-unidentified session) is visible rather than
                // silently dropped.
                tracing::warn!(
                    %goal_id,
                    "delegate: launch returned no session_id; skipping link+delivery for this task"
                );
                let mut store = goals.lock().await;
                let _ = store
                    .note(goal_id, "launch returned no session_id".to_string())
                    .await;
                continue;
            }

            // Link the session to the goal (§9.3). Best-effort: a link failure must
            // not undo a successful launch (it is recorded as a note instead).
            {
                let mut store = goals.lock().await;
                if let Err(e) = store
                    .link(goal_id, SessionLink::launched(&session_id, &task.prompt))
                    .await
                {
                    tracing::warn!(%session_id, %goal_id, "delegate: goal link failed: {e}");
                }
            }

            // DELIVER the task to the session pane (#1299). Spawn does not
            // auto-inject the prompt, so without this the session sits idle.
            if let Err(e) = control.send(&session_id, &task.prompt).await {
                tracing::warn!(%session_id, "delegate: task delivery failed: {e}");
                let mut store = goals.lock().await;
                let _ = store
                    .note(
                        goal_id,
                        format!("task delivery to {session_id} failed: {e}"),
                    )
                    .await;
            }

            launched.push(session_id);
        }
        launched
    }

    /// OBSERVE + VERIFY: poll each session, update its link state + goal progress.
    ///
    /// Why: phases 4–5 interpret each launched session's pane (deterministically,
    /// no LLM) and fold the result into the goal store so progress is DERIVED from
    /// observed state and a link reaches `Verified` ONLY with evidence (§3.5).
    /// What: for each session id, calls `control.get`, interprets it via
    /// [`interpret_session`], and applies a [`SessionUpdate`] (state + evidence)
    /// through `store.update` — which recomputes goal progress. A `get` failure or
    /// an update failure is noted (best-effort) but does not abort the whole sweep.
    /// Test: `delegate_tests.rs::delegate_observes_and_updates_progress`,
    /// `delegate_gate_blocks_without_evidence`.
    async fn observe_and_verify(
        &self,
        launched: &[String],
        goal_id: &str,
        control: &Arc<dyn SessionControl>,
        goals: &Arc<Mutex<SmGoalStore>>,
    ) -> Result<(), DelegationError> {
        for session_id in launched {
            let observed = match control.get(session_id).await {
                Ok(json) => interpret_session(&json),
                Err(e) => {
                    let mut store = goals.lock().await;
                    let _ = store
                        .note(goal_id, format!("observe {session_id} failed: {e}"))
                        .await;
                    continue;
                }
            };
            let update = SessionUpdate {
                session_id: session_id.clone(),
                state: Some(observed.state),
                evidence: observed.evidence.clone(),
                note: observed
                    .evidence
                    .as_ref()
                    .map(|e| format!("verified {session_id}: {e}")),
            };
            let mut store = goals.lock().await;
            if let Err(e) = store.update(goal_id, update).await {
                tracing::warn!(%session_id, %goal_id, "delegate: goal update failed: {e}");
            }
        }
        Ok(())
    }

    /// Read the goal's current lifecycle status label from the store.
    ///
    /// Why: every [`DelegationOutcome`] carries the goal's REAL status (§9.1) so a
    /// caller never has to disambiguate `goal_done == false` between "in progress"
    /// and "blocked"/"failed" with a follow-up call. This reads it back at the
    /// moment of return.
    /// What: locks the store, returns
    /// [`GoalStatus::label`](crate::core::sm::GoalStatus::label) for the goal, or
    /// `"Pending"` if the goal is (unexpectedly) absent.
    /// Test: `delegate_tests.rs` asserts the label for the in-progress and done
    /// paths.
    async fn goal_status_label(&self, goal_id: &str, goals: &Arc<Mutex<SmGoalStore>>) -> String {
        let store = goals.lock().await;
        store
            .get(goal_id)
            .map(|g| g.status.label().to_string())
            .unwrap_or_else(|| GoalStatus::Pending.label().to_string())
    }

    /// REPORT: build the status-first operator summary for the goal (§3.6).
    ///
    /// Why: phase 6 reports the verified outcome to the operator — status-first,
    /// no forbidden "should be done" phrasing (§3.5). The summary states the goal,
    /// the launched session count, and whether the gate passed WITH the count of
    /// verified links, so the operator sees evidence-backed status, not optimism.
    /// What: reads the goal back from the store and formats a one-paragraph,
    /// status-first summary; on a missing goal (should not happen) returns a terse
    /// fallback.
    /// Test: `delegate_tests.rs` asserts the report mentions the goal + status.
    async fn report(
        &self,
        goal_id: &str,
        launched: &[String],
        goal_done: bool,
        goals: &Arc<Mutex<SmGoalStore>>,
    ) -> String {
        let store = goals.lock().await;
        let Some(goal) = store.get(goal_id) else {
            return format!("Goal {goal_id}: launched {} session(s).", launched.len());
        };
        let verified = goal
            .sessions
            .iter()
            .filter(|s| s.state.is_verified())
            .count();
        let total = goal.sessions.len();
        if goal_done {
            format!(
                "Goal {goal_id} DONE: {verified}/{total} task(s) verified with observed evidence. \
                 Launched {} session(s).",
                launched.len()
            )
        } else {
            format!(
                "Goal {goal_id} in progress: {verified}/{total} task(s) verified \
                 (not yet done — verification evidence required for the rest). \
                 Launched {} session(s).",
                launched.len()
            )
        }
    }
}

/// The SM system prompt fragment that constrains the decompose DECISION.
///
/// Why: the structured-text protocol needs the model to emit EXACTLY one JSON
/// action block. This fragment (appended to the SM identity/prohibitions prompt)
/// pins the action schema so [`parse_decision`] can rely on it, and reiterates the
/// prohibition (do NOT do the work — delegate) so the model picks `delegate`, not
/// `do_work`, for real work.
/// What: returns the SM identity/prohibitions/workflow prompt
/// ([`resolve_sm_prompt_default`](crate::core::sm::prompt::resolve_sm_prompt_default))
/// followed by the decision-schema instructions.
/// Test: `delegate_tests.rs` asserts the mock saw the schema instructions.
fn decision_system_prompt() -> String {
    let base = crate::core::sm::prompt::resolve_sm_prompt_default();
    format!("{base}\n\n---\n\n{DECISION_INSTRUCTIONS}")
}

/// The decision-schema instructions appended to the SM system prompt.
const DECISION_INSTRUCTIONS: &str = "\
# DECISION (machine-read)

Reply with EXACTLY ONE JSON object (optionally inside a ```json fence) and no
other commands. Choose ONE action:

- To delegate real work (the default — you have no hands of your own):
  {\"action\":\"delegate\",\"tasks\":[{\"workdir\":\"<repo-or-dir>\",\"prompt\":\"<task>\",\"model\":\"<optional>\"}]}
  One task = one launched session. Split a goal into session-sized tasks.

- To talk to the operator (ask a question / give status — Allowlist 1):
  {\"action\":\"respond\",\"message\":\"<operator-facing text>\"}

You MUST NOT do the work yourself (SP1-SP5): never edit files, read project
source, run builds/tests, or answer a work request from your own knowledge.
If you are tempted to do the work directly, emit a delegate action instead.";

/// Build the decompose USER prompt: the goal + recall, asking for a decision.
///
/// Why: the user turn carries the operator's goal, its tracked id, and any
/// recalled SM-palace context, and asks the SM to DECOMPOSE into session-sized
/// tasks. Keeping it a free function keeps `decompose` terse and the prompt
/// auditable.
/// What: formats the goal id + message and (when present) a recall block, then the
/// explicit decompose ask.
/// Test: `delegate_tests.rs` asserts the mock saw the goal text.
fn decision_user_prompt(message: &str, goal_id: &str, recall: Option<&str>) -> String {
    let recall_block = match recall {
        Some(r) if !r.trim().is_empty() => format!("\n\nRelevant prior context:\n{r}"),
        _ => String::new(),
    };
    format!(
        "Operator goal ({goal_id}): {message}{recall_block}\n\n\
         Decompose this goal into session-sized tasks and reply with your decision."
    )
}

#[cfg(test)]
#[path = "delegate_tests.rs"]
mod delegate_tests;
