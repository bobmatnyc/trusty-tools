//! The 13 SM method handlers — thin mappings onto existing surfaces (§1A.1).
//!
//! Why: each `sm.*` method maps onto exactly one existing SM surface; keeping the
//! handlers here (separate from the router) makes the "method → surface" table
//! the spec mandates legible at a glance, and keeps every file under the SLOC
//! cap. NO business logic lives here — each handler parses its params, calls the
//! mapped surface, and shapes the result. All errors are typed
//! ([`MethodError`]) so the router maps them to JSON-RPC codes by variant.
//! What: `sm_chat`/`sm_health` → [`SessionManagerAgent`]; `sm_goals_*` →
//! [`SmGoalStore`] (feature-gated); `sm_sessions_*` → [`SessionControl`];
//! `sm_context_get` → the SM-5 context engine (feature-gated on the goal/context
//! availability decision).
//! Test: `tests.rs` round-trips every handler through the router.

use serde_json::{Value, json};

use super::SmDispatcher;
use super::control::{LaunchParams, SessionControlError};

/// A typed handler failure the router maps to a JSON-RPC error (§1A.2).
///
/// Why: the router needs to choose the right JSON-RPC error code per failure
/// class WITHOUT string-matching, and every handler must stay panic-free. A
/// typed enum carries the class + a human message + optional structured data.
/// What: [`InvalidParams`](MethodError::InvalidParams) (bad/missing params →
/// `-32602`), [`NotFound`](MethodError::NotFound) (absent goal/session → a
/// server-defined `-32001`), [`Unavailable`](MethodError::Unavailable) (a surface
/// gated off, e.g. goals without `sm-memory` → `-32002`), and
/// [`Internal`](MethodError::Internal) (any backend failure → `-32603`).
/// Test: `tests.rs` asserts the mapped code for each path.
#[derive(Debug, thiserror::Error)]
pub enum MethodError {
    /// Missing or malformed params → JSON-RPC `INVALID_PARAMS` (-32602).
    #[error("{0}")]
    InvalidParams(String),

    /// A referenced goal/session was not found → server error -32001.
    #[error("{0}")]
    NotFound(String),

    /// A surface is unavailable in this build (feature gated) → server -32002.
    #[error("{0}")]
    Unavailable(String),

    /// Any backend failure → JSON-RPC `INTERNAL_ERROR` (-32603).
    #[error("{0}")]
    Internal(String),
}

/// Server-defined JSON-RPC error code for "referenced entity not found".
pub const CODE_NOT_FOUND: i32 = -32001;
/// Server-defined JSON-RPC error code for "surface unavailable in this build".
pub const CODE_UNAVAILABLE: i32 = -32002;

/// Validate that params is an object or absent/null (else invalid-params).
///
/// Why: `sm.*` methods accept an optional params OBJECT; an array or scalar
/// `params` is malformed and must be a clean invalid-params error rather than a
/// silent miss. Absent/null is allowed (methods with no required fields).
/// What: returns `Ok(())` for `Null`/object params; [`MethodError::InvalidParams`]
/// otherwise.
/// Test: `tests.rs` drives both present and absent params.
fn ensure_obj(params: &Value) -> Result<(), MethodError> {
    match params {
        Value::Null | Value::Object(_) => Ok(()),
        other => Err(MethodError::InvalidParams(format!(
            "params must be a JSON object, got {other}"
        ))),
    }
}

/// Extract a REQUIRED, non-blank string field from params.
///
/// Why: several methods require a string (`message`, `description`, `session_id`,
/// `text`, `id`, `workdir`); a missing/non-string/blank value must be a clean
/// invalid-params error, not a panic. Distinguishing "absent" from
/// "present-but-blank" gives a caller an actionable message instead of a
/// misleading "missing" when they DID supply the key (just whitespace).
/// What: returns the original (untrimmed) string when present and non-blank;
/// otherwise [`MethodError::InvalidParams`] — with a "missing/not a string"
/// message when the field is absent or non-string, and a distinct "must not be
/// blank" message when it is present but trims to empty. Blank strings are still
/// rejected.
/// Test: `tests.rs::*_missing_required_param_is_invalid_params`,
/// `blank_required_param_is_distinct_invalid_params`.
fn req_str(params: &Value, field: &str) -> Result<String, MethodError> {
    ensure_obj(params)?;
    match params.get(field).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
        Some(_) => Err(MethodError::InvalidParams(format!(
            "param `{field}` must not be blank"
        ))),
        None => Err(MethodError::InvalidParams(format!(
            "missing required string param `{field}`"
        ))),
    }
}

/// Extract an OPTIONAL string field from params (absent/null/empty → `None`).
fn opt_str(params: &Value, field: &str) -> Option<String> {
    params
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

// ── sm.chat / sm.health (SessionManagerAgent: SM-7 / §5.3) ──────────────────────

/// `sm.chat { message, conv_id? }` → `{ reply, conv_id, cost? }` (SM-7).
///
/// Why: the headline method — drives one full SM conversational turn via
/// [`SessionManagerAgent::chat`]. The adapter only parses params and shapes the
/// outcome; the §7.5 assembly + provider call live in SM-7.
/// What: parses `message` (required) + `conv_id` (optional), calls `chat`, and
/// returns `{ reply, conv_id, cost }`. A degraded SM (no provider) maps to a
/// graceful [`MethodError::Unavailable`]; any other chat failure →
/// [`MethodError::Internal`].
/// Test: `tests.rs::chat_round_trips`, `chat_degraded_is_unavailable`.
pub async fn sm_chat(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let message = req_str(params, "message")?;
    let conv_id = opt_str(params, "conv_id");
    match d.agent.chat(&message, conv_id.as_deref()).await {
        Ok(outcome) => Ok(json!({
            "reply": outcome.reply,
            "conv_id": outcome.conv_id,
            "cost": outcome.cost_usd,
        })),
        Err(crate::core::sm::SmAgentError::Degraded(notice)) => {
            Err(MethodError::Unavailable(notice))
        }
        Err(e) => Err(MethodError::Internal(e.to_string())),
    }
}

/// `sm.delegate { message }` → the delegation-loop outcome (SM-8, §3.4).
///
/// Why: the capstone method — drives the FULL 6-phase delegation loop
/// ([`SessionManagerAgent::delegate_goal`]) over the dispatcher's session-control
/// and goal-store surfaces. This is the §1A.2 step-1 flow `claude-mpm ⟷ SM ⟷ t-mpm`:
/// a driver sends a goal, the SM creates a tracked goal, launches and delivers
/// session(s), observes, verifies with evidence (the §3.5 gate), and reports.
/// Unlike `sm.chat` (a conversational turn), this runs the launch/observe/verify
/// mechanism end-to-end.
/// What: under `sm-memory`, parses `message` (required), calls `delegate_goal`
/// with the dispatcher's `sessions` + `goals`, and returns
/// `{ reply, goal_id, launched, goal_done }`. A degraded SM (no provider) maps to
/// [`MethodError::Unavailable`]; any other failure → [`MethodError::Internal`].
/// Without `sm-memory` (no goal store) it returns a graceful unavailable error.
/// Test: `tests.rs::delegate_end_to_end_launch_observe_verify_close`,
/// `delegate_gate_blocks_without_evidence`, `delegate_unavailable_without_feature`.
#[cfg(feature = "sm-memory")]
pub async fn sm_delegate(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let message = req_str(params, "message")?;
    match d.agent.delegate_goal(&message, &d.sessions, &d.goals).await {
        Ok(outcome) => Ok(json!({
            "reply": outcome.reply,
            "goal_id": outcome.goal_id,
            "launched": outcome.launched,
            "goal_done": outcome.goal_done,
        })),
        Err(crate::core::sm::agent::DelegationError::Degraded(notice)) => {
            Err(MethodError::Unavailable(notice))
        }
        Err(e) => Err(MethodError::Internal(e.to_string())),
    }
}

/// `sm.delegate` is unavailable without the `sm-memory` feature.
///
/// Why: the delegation loop persists goals through the SM-6 store, which is backed
/// by the SM palace (memory-core); the no-memory build returns a graceful JSON-RPC
/// error rather than failing to compile.
/// What: always returns [`MethodError::Unavailable`].
/// Test: `tests.rs::delegate_unavailable_without_feature`.
#[cfg(not(feature = "sm-memory"))]
pub async fn sm_delegate(_d: &SmDispatcher, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::Unavailable(
        "sm.delegate is not available without the sm-memory feature".to_string(),
    ))
}

/// `sm.health {}` → `{ ok, provider, degraded, model_tiers }` (§5.3).
///
/// Why: a parent driver probes SM readiness before driving turns. Maps directly
/// onto [`SessionManagerAgent::health`] (no network call).
/// What: calls `health` and serializes the [`SmHealth`](crate::core::sm::SmHealth).
/// Test: `tests.rs::health_round_trips`.
pub async fn sm_health(d: &SmDispatcher, _params: &Value) -> Result<Value, MethodError> {
    let health = d.agent.health().await;
    serde_json::to_value(health).map_err(|e| MethodError::Internal(e.to_string()))
}

// ── sm.sessions.* (SessionControl: §2.6) ────────────────────────────────────────

/// `sm.sessions.launch { workdir, model?, prompt?, goal_id? }` → `{ session_id }`.
///
/// Why: maps onto the managed-session spawn surface (§2.6) via [`SessionControl`].
/// When a `goal_id` is supplied the launched session is LINKED to that goal (§9.3)
/// through the goal store — the one place the adapter touches two surfaces, and
/// only to record the spec-mandated link, not to add logic.
/// What: builds [`LaunchParams`] from params, calls `sessions.launch`, then (when
/// `goal_id` is present and the goal store is available) links the new
/// `session_id` to the goal. Returns `{ session_id }` (link failures are reported
/// as a `linked: false` note, never failing an otherwise-successful launch).
/// Test: `tests.rs::launch_round_trips`, `launch_with_goal_links_session`.
pub async fn sm_sessions_launch(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let workdir = req_str(params, "workdir")?;
    let goal_id = opt_str(params, "goal_id");
    let launch = LaunchParams {
        workdir,
        model: opt_str(params, "model"),
        prompt: opt_str(params, "prompt"),
        goal_id: goal_id.clone(),
    };
    let result = d.sessions.launch(launch).await.map_err(map_control_err)?;

    // §9.3: record the goal↔session link when requested. Best-effort: a link
    // failure must not undo a successful launch.
    if let Some(goal_id) = goal_id {
        let session_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let linked = link_session_to_goal(d, &goal_id, &session_id).await;
        if !linked {
            // Best-effort link (§9.3): an unknown goal or a persist failure must
            // NOT undo a successful launch, but it must be observable rather than
            // silently swallowed — `linked: false` rides back in the response too.
            tracing::warn!(
                session_id = %session_id,
                goal_id = %goal_id,
                "sm.sessions.launch: goal↔session link failed (best-effort); session launched but not linked"
            );
        }
        let mut out = result;
        if let Some(obj) = out.as_object_mut() {
            obj.insert("goal_id".to_string(), json!(goal_id));
            obj.insert("linked".to_string(), json!(linked));
        }
        return Ok(out);
    }
    Ok(result)
}

/// `sm.sessions.list {}` → `{ sessions: [...] }` (§2.6).
pub async fn sm_sessions_list(d: &SmDispatcher, _params: &Value) -> Result<Value, MethodError> {
    d.sessions.list().await.map_err(map_control_err)
}

/// `sm.sessions.get { session_id }` → `{ session, ... }` (§2.6).
pub async fn sm_sessions_get(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let session_id = req_str(params, "session_id")?;
    d.sessions.get(&session_id).await.map_err(map_control_err)
}

/// `sm.sessions.send { session_id, text }` → `{ ok }` (§2.6).
pub async fn sm_sessions_send(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let session_id = req_str(params, "session_id")?;
    let text = req_str(params, "text")?;
    d.sessions
        .send(&session_id, &text)
        .await
        .map_err(map_control_err)
}

/// `sm.sessions.stop { session_id }` → `{ ok }` (§2.6).
pub async fn sm_sessions_stop(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let session_id = req_str(params, "session_id")?;
    d.sessions.stop(&session_id).await.map_err(map_control_err)
}

/// `sm.sessions.resume { session_id }` → `{ ok }` (§2.6).
pub async fn sm_sessions_resume(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let session_id = req_str(params, "session_id")?;
    d.sessions
        .resume(&session_id)
        .await
        .map_err(map_control_err)
}

/// `sm.sessions.kill { session_id }` → `{ ok }` (§2.6 force-stop/reap).
pub async fn sm_sessions_kill(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let session_id = req_str(params, "session_id")?;
    d.sessions.kill(&session_id).await.map_err(map_control_err)
}

/// Map a [`SessionControlError`] onto the handler error class.
///
/// Why: the session control surface distinguishes not-found from backend
/// failures; preserving that distinction lets the router pick the right JSON-RPC
/// code.
/// What: `NotFound` → [`MethodError::NotFound`]; `Backend` → [`MethodError::Internal`].
/// Test: `tests.rs::get_unknown_session_is_not_found`.
fn map_control_err(e: SessionControlError) -> MethodError {
    match e {
        SessionControlError::NotFound(msg) => MethodError::NotFound(msg),
        SessionControlError::Backend(msg) => MethodError::Internal(msg),
    }
}

// ── sm.context.get (SM-5 context engine — feature-gated) ────────────────────────

/// `sm.context.get { conv_id? }` → context state (§7.1/§7.5).
///
/// Why: surfaces the rolling-context engine's state — the compressed block, the
/// recent verbatim rounds, the total round count, and the token estimate — so a
/// driver can inspect what context the SM is carrying for a conversation.
/// What: under `sm-memory`, opens the per-`conv_id` engine under the SM data root
/// and returns `{ compressed_context, recent_rounds, total_rounds, token_estimate }`.
/// Without `sm-memory` (or when no `conv_id` is supplied to identify a stored
/// conversation), returns the appropriate graceful error.
/// Test: `tests.rs::context_get_round_trips` (feature),
/// `context_get_unavailable_without_feature` (no feature).
#[cfg(feature = "sm-memory")]
pub async fn sm_context_get(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    use crate::core::sm::SmContextEngine;

    let conv_id = req_str(params, "conv_id")?;
    let engine = SmContextEngine::open(
        &conv_id,
        &d.data_root,
        &d.config.inference,
        &d.config.rounds,
    )
    .map_err(|e| MethodError::Internal(e.to_string()))?;
    let conv = engine.conversation();
    let recent: Vec<Value> = conv
        .recent_rounds
        .iter()
        .map(|r| json!({ "user": r.user, "assistant": r.assistant }))
        .collect();
    Ok(json!({
        "compressed_context": conv.compressed_context,
        "recent_rounds": recent,
        "total_rounds": conv.total_rounds,
        "token_estimate": conv.token_estimate,
    }))
}

/// `sm.context.get` is unavailable without the `sm-memory` feature.
///
/// Why: the context engine state lives alongside the SM palace; the no-memory
/// build returns a graceful JSON-RPC error rather than failing to compile.
/// What: always returns [`MethodError::Unavailable`].
/// Test: `tests.rs::context_get_unavailable_without_feature`.
#[cfg(not(feature = "sm-memory"))]
pub async fn sm_context_get(_d: &SmDispatcher, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::Unavailable(
        "sm.context.get is not available without the sm-memory feature".to_string(),
    ))
}

// ── sm.goals.* (SmGoalStore — feature-gated) ────────────────────────────────────

/// `sm.goals.list { status? }` → `{ goals: [Goal] }` (SM-6).
///
/// Why: lists tracked operator goals, optionally filtered by status. Maps onto the
/// SM-6 goal store.
/// What: under `sm-memory`, reads `store.all()` and (when `status` is supplied)
/// filters by it; returns `{ goals }`. Without the feature → unavailable error.
/// Test: `tests.rs::goals_list_round_trips`, `goals_unavailable_without_feature`.
#[cfg(feature = "sm-memory")]
pub async fn sm_goals_list(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let status = opt_str(params, "status");
    let store = d.goals.lock().await;
    let goals: Vec<_> = store
        .all()
        .into_iter()
        .filter(|g| match &status {
            Some(s) => goal_status_str(g) == s.to_ascii_lowercase(),
            None => true,
        })
        .collect();
    serde_json::to_value(json!({ "goals": goals }))
        .map_err(|e| MethodError::Internal(e.to_string()))
}

/// `sm.goals.create { description, acceptance? }` → `{ goal: Goal }` (SM-6).
#[cfg(feature = "sm-memory")]
pub async fn sm_goals_create(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let description = req_str(params, "description")?;
    let acceptance = params
        .get("acceptance")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut store = d.goals.lock().await;
    let goal = store
        .create(description, acceptance)
        .await
        .map_err(|e| MethodError::Internal(e.to_string()))?;
    serde_json::to_value(json!({ "goal": goal })).map_err(|e| MethodError::Internal(e.to_string()))
}

/// `sm.goals.update { id, status?, progress?, note? }` → `{ goal: Goal }` (SM-6).
///
/// Why: maps onto the SM-6 goal update path. `note` appends a free-form note;
/// `status` transitions the goal (the verification gate applies to `done`).
/// What: under `sm-memory`, applies a `note` (if present) then a `status` change
/// (if present), returning the final goal. A `done` request that fails the
/// verification gate maps to [`MethodError::Internal`] (the gate message). The
/// spec's `progress` field is DERIVED by the store from session states (§9.1) and
/// is not directly settable, so it is accepted-but-ignored with a note.
/// Test: `tests.rs::goals_update_round_trips`.
#[cfg(feature = "sm-memory")]
pub async fn sm_goals_update(d: &SmDispatcher, params: &Value) -> Result<Value, MethodError> {
    let id = req_str(params, "id")?;
    let note = opt_str(params, "note");
    let status = opt_str(params, "status");
    let mut store = d.goals.lock().await;

    if let Some(note) = note {
        store.note(&id, note).await.map_err(map_goal_err)?;
    }
    if let Some(status) = status {
        let parsed = parse_goal_status(&status)?;
        store.set_status(&id, parsed).await.map_err(map_goal_err)?;
    }
    let goal = store
        .get(&id)
        .cloned()
        .ok_or_else(|| MethodError::NotFound(format!("goal not found: {id}")))?;
    serde_json::to_value(json!({ "goal": goal })).map_err(|e| MethodError::Internal(e.to_string()))
}

/// The lowercase wire status string for a goal (matches the camelCase serde).
#[cfg(feature = "sm-memory")]
fn goal_status_str(goal: &crate::core::sm::Goal) -> String {
    use crate::core::sm::GoalStatus;
    match goal.status {
        GoalStatus::Pending => "pending",
        GoalStatus::InProgress => "inprogress",
        GoalStatus::Blocked => "blocked",
        GoalStatus::Done => "done",
        GoalStatus::Abandoned => "abandoned",
    }
    .to_string()
}

/// Parse a wire status string into a [`GoalStatus`].
///
/// Why: `sm.goals.update` accepts a status string; an unknown value must be a
/// clean invalid-params error, not a panic.
/// What: case-insensitively matches the five statuses (accepting both
/// `inprogress` and `in_progress`).
/// Test: `tests.rs::goals_update_round_trips`.
#[cfg(feature = "sm-memory")]
fn parse_goal_status(s: &str) -> Result<crate::core::sm::GoalStatus, MethodError> {
    use crate::core::sm::GoalStatus;
    match s
        .trim()
        .to_ascii_lowercase()
        .replace(['_', '-'], "")
        .as_str()
    {
        "pending" => Ok(GoalStatus::Pending),
        "inprogress" => Ok(GoalStatus::InProgress),
        "blocked" => Ok(GoalStatus::Blocked),
        "done" => Ok(GoalStatus::Done),
        "abandoned" => Ok(GoalStatus::Abandoned),
        other => Err(MethodError::InvalidParams(format!(
            "unknown goal status {other:?}; expected one of: \
             pending, inProgress, blocked, done, abandoned"
        ))),
    }
}

/// Map an [`SmGoalError`](crate::core::sm::SmGoalError) onto a handler error.
///
/// Why: the goal store distinguishes not-found from other failures; preserving it
/// lets the router pick the right JSON-RPC code.
/// What: `NotFound` → [`MethodError::NotFound`]; everything else (palace/cache/
/// gate) → [`MethodError::Internal`].
/// Test: `tests.rs::goals_update_unknown_is_not_found`.
#[cfg(feature = "sm-memory")]
fn map_goal_err(e: crate::core::sm::SmGoalError) -> MethodError {
    match e {
        crate::core::sm::SmGoalError::NotFound(msg) => {
            MethodError::NotFound(format!("not found: {msg}"))
        }
        other => MethodError::Internal(other.to_string()),
    }
}

/// Link a launched session to a goal (best-effort, §9.3).
///
/// Why: `sm.sessions.launch` with a `goal_id` must record the goal↔session link;
/// doing it here keeps the launch handler's two-surface touch minimal. A link
/// failure (unknown goal / persist error) is reported, never fatal to the launch.
/// What: under `sm-memory`, calls `store.link`; returns whether it succeeded.
/// Test: `tests.rs::launch_with_goal_links_session`.
#[cfg(feature = "sm-memory")]
async fn link_session_to_goal(d: &SmDispatcher, goal_id: &str, session_id: &str) -> bool {
    use crate::core::sm::SessionLink;
    let mut store = d.goals.lock().await;
    store
        .link(
            goal_id,
            SessionLink::launched(session_id, "session-manager launched task"),
        )
        .await
        .is_ok()
}

/// No-memory build: there is no goal store, so a launch link is a no-op.
///
/// Why: keeps `sm.sessions.launch` compiling without `sm-memory`; the goal link
/// is simply not recorded (goals are unavailable in that build).
/// What: always returns `false`.
/// Test: `tests.rs` (no-feature build) covers launch without a goal.
#[cfg(not(feature = "sm-memory"))]
async fn link_session_to_goal(_d: &SmDispatcher, _goal_id: &str, _session_id: &str) -> bool {
    false
}

/// `sm.goals.*` are unavailable without the `sm-memory` feature.
///
/// Why: the goal store is backed by the SM palace (memory-core); the no-memory
/// build returns a graceful JSON-RPC error rather than failing to compile.
/// What: always returns [`MethodError::Unavailable`].
/// Test: `tests.rs::goals_unavailable_without_feature`.
#[cfg(not(feature = "sm-memory"))]
pub async fn sm_goals_list(_d: &SmDispatcher, _params: &Value) -> Result<Value, MethodError> {
    goals_unavailable()
}

/// See [`sm_goals_list`] (no-memory build).
#[cfg(not(feature = "sm-memory"))]
pub async fn sm_goals_create(_d: &SmDispatcher, _params: &Value) -> Result<Value, MethodError> {
    goals_unavailable()
}

/// See [`sm_goals_list`] (no-memory build).
#[cfg(not(feature = "sm-memory"))]
pub async fn sm_goals_update(_d: &SmDispatcher, _params: &Value) -> Result<Value, MethodError> {
    goals_unavailable()
}

/// The graceful "goals unavailable" error for the no-memory build.
#[cfg(not(feature = "sm-memory"))]
fn goals_unavailable() -> Result<Value, MethodError> {
    Err(MethodError::Unavailable(
        "sm.goals.* are not available without the sm-memory feature".to_string(),
    ))
}
