//! Non-opt-in delegation tracking from Claude Code hooks (#2864 S2).
//!
//! Why: delegation tracking had two defects that together made it useless as a
//! basis for "which agents are in flight?". (1) It was opt-in — a record existed
//! only if the PM chose to call the `agent_delegate` MCP tool, so a PM that
//! dispatched work with the native subagent tool alone left no trace. (2) Status
//! never moved: the only production construction is
//! [`Delegation::new`](crate::core::agent::Delegation::new), which yields
//! `Queued`, and nothing ever wrote `Running`/`Completed`/`Failed`. So
//! [`has_live_children`](crate::daemon::idle_nudge::has_live_children) — which
//! treats `Queued` and `Running` as live — reported *every* delegation as live
//! forever, permanently suppressing the idle nudge for any session that had ever
//! delegated once.
//!
//! This module fixes both by observing hooks the daemon already receives: a
//! `PreToolUse` with `matcher: "*"` is installed on every managed session, so
//! every native subagent dispatch already arrives here. No new hook, no opt-in.
//!
//! # Correlation model (empirically established, #2864 Step 0)
//!
//! Captured from live Claude Code 2.1.220 stdin with two *concurrent*
//! subagents — the case that breaks any "most recent delegation" guess:
//!
//! ```text
//! PreToolUse   tool_name=Agent  tool_use_id=toolu_01DA…   (no agent id yet)
//! PostToolUse  tool_name=Agent  tool_use_id=toolu_01DA…
//!              tool_response={ isAsync: true,
//!                              status: "async_launched",
//!                              agentId: "a403cdbc078b5c474" }
//! SubagentStop                  agent_id="a403cdbc078b5c474"
//! ```
//!
//! Two consequences drive the whole design, and **neither may be "simplified"
//! away without re-running the probe**:
//!
//! 1. **`PostToolUse` does NOT mean the subagent finished.** For an async
//!    dispatch it fires ~1 ms after launch (`duration_ms: 1`) with
//!    `status: "async_launched"`, while the subagent runs on for minutes.
//!    Terminalizing there would mark every delegation `Completed` almost
//!    immediately and report "no agents in flight" while they are all still
//!    running — strictly worse than not tracking at all. `PostToolUse` is
//!    therefore a *join* step (it teaches us `agentId`), and terminalizes only
//!    when the dispatch genuinely returned synchronously.
//! 2. **`SubagentStop` DOES carry an exact correlation key** — `agent_id`,
//!    identical to `tool_response.agentId`. It is the authoritative terminal
//!    signal, not an advisory one. Because the key is exact, closing one of two
//!    concurrent delegations cannot close the other.
//!
//! Absent an `agent_id` (an older Claude Code, or a dispatch whose `PostToolUse`
//! was missed) a `SubagentStop` terminalizes **nothing**. Guessing "the most
//! recent Running delegation" would close the wrong one under concurrency and
//! manufacture a false idle, which is the specific failure this design exists to
//! prevent.
//!
//! # Cost
//!
//! [`observe`] runs inside the synchronous `PreToolUse` hook, which has a 5 s
//! timeout and sits in front of *every* tool call in *every* managed session.
//! The non-delegation path — which is essentially all traffic — is one
//! `payload.get("tool")` plus an exact string compare against two constants, and
//! then returns. The delegation path is in-memory `DashMap` work only: no disk,
//! no network, no async, no blocking lock.
//!
//! # Known limitation
//!
//! Records are keyed by the session id the event arrives under: the Claude
//! session UUID for hook-observed records (matching
//! [`crate::daemon::idle_nudge::maybe_nudge_parked_session`]), but whatever id
//! the PM passed for `agent_delegate` records. When those differ, [`dedup`]
//! cannot match and a declaration plus its observation remain two records. That
//! id ambiguity predates #2864 and is not addressed here.
//!
//! Test: the `#[cfg(test)]` suite below.

use chrono::Utc;
use serde_json::Value;

use crate::core::agent::{
    Delegation, DelegationId, DelegationSource, DelegationStatus, ModelTier,
    is_subagent_dispatch_tool,
};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::state::DaemonState;

/// How long after an `agent_delegate` call an observed dispatch may be treated
/// as the *same* delegation.
///
/// Why: a PM that both declares (`agent_delegate`) and dispatches (native tool)
/// must produce one record, not two. The declaration immediately precedes the
/// dispatch in practice, so a generous-but-finite window merges the pair while
/// preventing a long-stale `Queued` record from swallowing an unrelated later
/// dispatch to the same agent.
/// Test: `dedups_declaration_and_observation`, `dedup_window_expires`.
const DEDUP_WINDOW_SECS: i64 = 300;

/// Observe one hook event and update delegation tracking.
///
/// Why: the single entry point [`crate::daemon::services::hook_service::HookService::process`]
/// calls, so the hook pipeline gains delegation tracking by way of one line and
/// all the correlation rules stay here.
/// What: routes a subagent-dispatch `PreToolUse` to [`on_dispatch`], its
/// `PostToolUse`/`PostToolUseFailure` to [`on_launched`], and
/// `SubagentStop`/`SubagentStopFailure` to [`on_subagent_stop`]. Every other
/// event returns immediately. Never panics and never blocks; a payload missing
/// the fields it needs is skipped silently (fail-open — tracking is
/// observational and must never affect the hook verdict).
/// Test: `ignores_unrelated_tools`, `pre_tool_use_creates_running_delegation`,
/// and the rest of the suite below.
pub fn observe(state: &DaemonState, session: SessionId, event: HookEvent, payload: &Value) {
    match event {
        // Hot path: the overwhelming majority of PreToolUse events are ordinary
        // tools. Bail on the tool-name compare before touching any state.
        HookEvent::PreToolUse => {
            if dispatch_tool(payload).is_some() {
                on_dispatch(state, session, payload);
            }
        }
        HookEvent::PostToolUse | HookEvent::PostToolUseFailure => {
            if dispatch_tool(payload).is_some() {
                on_launched(state, session, payload, event);
            }
        }
        HookEvent::SubagentStop | HookEvent::SubagentStopFailure => {
            on_subagent_stop(state, session, payload, event);
        }
        _ => {}
    }
}

/// The tool name, when this payload names a subagent-dispatch tool.
///
/// Why: the hot-path early-out — one map lookup and one exact string compare.
/// Test: `ignores_unrelated_tools`.
fn dispatch_tool(payload: &Value) -> Option<&str> {
    payload
        .get("tool")
        .and_then(Value::as_str)
        .filter(|t| is_subagent_dispatch_tool(t))
}

/// Read a `&str` field from a payload, treating empty as absent.
fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// `PreToolUse` on a dispatch tool: a subagent is starting now.
///
/// Why: this is the moment tracking must record a *live* child, so a PM with
/// work in flight is never mistaken for idle.
/// What: reads `input.subagent_type` (agent) and `input.description` (task).
/// Idempotent on `tool_use_id` — a redelivered hook updates nothing twice. Tries
/// [`dedup`] first so a matching `agent_delegate` record is promoted in place to
/// `Running`; otherwise inserts a fresh
/// [`Delegation::observed`](crate::core::agent::Delegation::observed). Records
/// `cwd` and `transcript_path` when present.
/// Test: `pre_tool_use_creates_running_delegation`,
/// `dedups_declaration_and_observation`, `duplicate_pre_tool_use_is_idempotent`.
fn on_dispatch(state: &DaemonState, session: SessionId, payload: &Value) {
    let input = payload.get("input");
    let agent = input
        .and_then(|i| i.get("subagent_type"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let task = input
        .and_then(|i| i.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_use_id = field(payload, "tool_use_id").map(str::to_string);

    // Idempotence: an already-tracked tool_use_id means this hook was redelivered.
    if let Some(id) = tool_use_id.as_deref()
        && state
            .find_delegation(session, |d| d.tool_use_id.as_deref() == Some(id))
            .is_some()
    {
        return;
    }

    let cwd = field(payload, "cwd").map(std::path::PathBuf::from);
    let transcript = field(payload, "transcript_path").map(std::path::PathBuf::from);

    // Merge with a matching `agent_delegate` declaration when one is pending.
    if let Some(existing) = dedup(state, session, agent) {
        state.mutate_delegation(existing, |d| {
            d.status = DelegationStatus::Running;
            d.started_at = Some(Utc::now());
            d.tool_use_id = tool_use_id.clone();
            d.cwd = cwd.clone();
            d.transcript_path = transcript.clone();
            if d.task.is_empty() && !task.is_empty() {
                d.task = task.to_string();
            }
        });
        return;
    }

    let mut delegation = Delegation::observed(session, agent, task, tool_use_id);
    delegation.cwd = cwd;
    delegation.transcript_path = transcript;
    state.upsert_delegation(delegation);
}

/// Find a pending `agent_delegate` declaration this dispatch should merge into.
///
/// Why: a PM instructed to call `agent_delegate` *and* actually dispatch would
/// otherwise produce two records for one subagent, inflating the live-children
/// count and leaving an immortal `Queued` orphan (nothing would ever terminalize
/// the declaration, since only the observed record carries the correlation
/// keys).
/// What: the most recent same-session, same-agent delegation that is still
/// `Queued`, was `McpDeclared`, has not already been bound to a `tool_use_id`,
/// and was created within [`DEDUP_WINDOW_SECS`].
/// Test: `dedups_declaration_and_observation`, `dedup_window_expires`,
/// `dedup_ignores_different_agent`.
fn dedup(state: &DaemonState, session: SessionId, agent: &str) -> Option<DelegationId> {
    let cutoff = Utc::now() - chrono::Duration::seconds(DEDUP_WINDOW_SECS);
    state
        .delegations_for(session)
        .into_iter()
        .filter(|d| {
            d.source == DelegationSource::McpDeclared
                && d.status == DelegationStatus::Queued
                && d.tool_use_id.is_none()
                && d.agent == agent
                && d.created_at >= cutoff
        })
        .max_by_key(|d| d.created_at)
        .map(|d| d.id)
}

/// `PostToolUse` on a dispatch tool: learn `agentId`, terminalize only if the
/// dispatch really returned.
///
/// Why: see the module note — an async dispatch returns in ~1 ms while the
/// subagent runs on, so this is primarily the *join* that binds `tool_use_id` to
/// the `agent_id` that `SubagentStop` will quote.
/// What: locates the delegation by `tool_use_id`, stores
/// `tool_response.agentId`, and refines the tier from `resolvedModel`. Leaves
/// the status `Running` when the response says `isAsync`/`async_launched`;
/// otherwise terminalizes — `Failed` on a `PostToolUseFailure` event or an
/// `is_error` response, else `Completed`.
/// Test: `async_launch_keeps_delegation_running`,
/// `synchronous_post_tool_use_completes_delegation`,
/// `post_tool_use_failure_marks_failed`.
fn on_launched(state: &DaemonState, session: SessionId, payload: &Value, event: HookEvent) {
    let Some(tool_use_id) = field(payload, "tool_use_id") else {
        return;
    };
    let Some(id) =
        state.find_delegation(session, |d| d.tool_use_id.as_deref() == Some(tool_use_id))
    else {
        return;
    };

    let response = payload.get("tool_response");
    let agent_id = response
        .and_then(|r| r.get("agentId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let tier = response
        .and_then(|r| r.get("resolvedModel"))
        .and_then(Value::as_str)
        .map(ModelTier::from_model_id);

    // Still running? An async dispatch reports launch, not completion.
    let still_running = response.is_some_and(|r| {
        r.get("isAsync").and_then(Value::as_bool).unwrap_or(false)
            || r.get("status").and_then(Value::as_str) == Some("async_launched")
    });
    let errored = event == HookEvent::PostToolUseFailure
        || response
            .and_then(|r| r.get("is_error"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

    state.mutate_delegation(id, |d| {
        if agent_id.is_some() {
            d.agent_id = agent_id.clone();
        }
        if let Some(t) = tier {
            d.tier = t;
        }
        if !still_running {
            d.status = if errored {
                DelegationStatus::Failed
            } else {
                DelegationStatus::Completed
            };
            d.ended_at = Some(Utc::now());
        }
    });
}

/// `SubagentStop`: terminalize exactly the delegation that ended.
///
/// Why: this is the authoritative completion signal, and the only one that
/// arrives when the subagent actually finishes.
/// What: resolves `payload.agent_id` against the stored `agent_id` and
/// terminalizes that record alone (`Failed` for `SubagentStopFailure`, else
/// `Completed`). When `agent_id` is absent, or matches nothing, **nothing is
/// terminalized** — see the module note on why a "most recent" fallback is
/// forbidden.
/// Test: `subagent_stop_completes_matching_delegation`,
/// `subagent_stop_without_agent_id_terminalizes_nothing`,
/// `concurrent_delegations_terminalize_independently`.
fn on_subagent_stop(state: &DaemonState, session: SessionId, payload: &Value, event: HookEvent) {
    let Some(agent_id) = field(payload, "agent_id") else {
        tracing::trace!(
            "SubagentStop without agent_id — no delegation terminalized (#2864); a \
             'most recent' guess would close the wrong one under concurrency"
        );
        return;
    };
    let Some(id) = state.find_delegation(session, |d| {
        d.agent_id.as_deref() == Some(agent_id) && !is_terminal(d.status)
    }) else {
        return;
    };
    let status = if event == HookEvent::SubagentStopFailure {
        DelegationStatus::Failed
    } else {
        DelegationStatus::Completed
    };
    state.terminate_delegation(id, status);
}

/// Is this status terminal (i.e. the delegation is no longer live)?
///
/// Why: keeps the "live" definition aligned with
/// [`crate::daemon::idle_nudge::has_live_children`], which counts
/// `Queued`/`Running` as live.
/// Test: exercised via `subagent_stop_is_idempotent`.
fn is_terminal(status: DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Completed | DelegationStatus::Failed | DelegationStatus::Cancelled
    )
}

#[cfg(test)]
#[path = "delegation_tracker_tests.rs"]
mod tests;
