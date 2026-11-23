//! The ledger task a `SubagentStop` was bound to at dispatch (#11674).
//!
//! Why: the bound id lives on the daemon's [`Delegation`], and `tm hook` must
//! compare it with the subagent's last line. This is the daemon half of that
//! join, returned in the `POST /hooks` response.
//! What: [`stop_keys`] reads the stop's `agent_id` and the dispatch
//! `tool_use_id` the hook added; [`stop_binding`] finds the delegation by
//! `agent_id` first (what `delegation_tracker` matches a stop on), else by
//! `tool_use_id` — a foreground dispatch's `PostToolUse` lands after its stop,
//! so its `agent_id` is not yet known.
//! Test: `subagent_task_binding_tests.rs`.

use serde_json::Value;

use crate::core::agent::Delegation;
use crate::core::session::SessionId;
use crate::core::subagent_task_roundtrip::{DISPATCH_TOOL_USE_ID_KEY, StopBinding};
use crate::daemon::state::DaemonState;

/// The stop's `(agent_id, dispatch tool_use_id)`, each when present.
pub fn stop_keys(payload: &Value) -> (Option<String>, Option<String>) {
    let get = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    (get("agent_id"), get(DISPATCH_TOOL_USE_ID_KEY))
}

/// The binding for one stop; `delegation_found: false` when nothing matches.
///
/// Test: `subagent_task_roundtrip_concurrent_stops_in_reverse_order`,
/// `subagent_task_roundtrip_foreground_stop_joins_by_tool_use_id`.
pub fn stop_binding(
    state: &DaemonState,
    session: SessionId,
    agent_id: Option<&str>,
    tool_use_id: Option<&str>,
) -> StopBinding {
    let all = state.delegations_for(session);
    let by_agent = agent_id.and_then(|a| all.iter().find(|d| d.agent_id.as_deref() == Some(a)));
    let found: Option<&Delegation> = by_agent.or_else(|| {
        tool_use_id.and_then(|t| all.iter().find(|d| d.tool_use_id.as_deref() == Some(t)))
    });
    match found {
        Some(d) => StopBinding {
            delegation_found: true,
            tool_use_id: d.tool_use_id.clone(),
            ledger_task: d.ledger_task.clone(),
        },
        None => StopBinding {
            tool_use_id: tool_use_id.map(str::to_owned),
            ..StopBinding::default()
        },
    }
}

#[cfg(test)]
#[path = "subagent_task_binding_tests.rs"]
mod tests;
