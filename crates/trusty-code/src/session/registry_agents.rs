//! `SessionRegistry::get_agents` — the live agent-roster fold behind
//! `session.get_agents` (DOC-39 §5.4). Split out of `registry.rs` purely to
//! keep that production file under the crate's 500-SLOC cap — this is a
//! child module of `registry` (declared via `#[path = ...] mod agents;`), so
//! it shares full access to `SessionRegistry`'s private helpers exactly as
//! if these methods were still defined in that file.
//!
//! Why: DOC-39 §3.2/§5.4 documents that "who is running?" was daemon-owned
//! state a UI client had to reconstruct itself by folding the SSE replay —
//! the shape §2.1 forbids (C-1: the daemon owns all logic; a client never
//! derives). §5.4 names `session.get_agents` as "the principled endpoint;
//! the fold is a Phase-1 loan, not a design" and directs that loan to be
//! repaid by moving the fold server-side. This module is that repayment: the
//! SAME fold, now performed once by the daemon over its own ring buffer
//! rather than N times by N clients.
//! What: [`get_agents`] folds a session's ring-buffer replay
//! (`SessionRegistry::replay`, the same envelopes `session.attach`/the SSE
//! route already expose) over the tool-attribution events every agent-loop
//! tool dispatch already emits (`ToolStarted`/`ToolFinished`/`ToolError`/
//! `SearchPerformed`/`MemoryRecalled` — all carry `agent` + `agent_id` since
//! #2898/DOC-39 AC-13), producing one [`AgentRosterEntry`] per distinct
//! `agent_id` in first-seen order. `state` is derived from the LAST
//! attribution event seen for that `agent_id`: `"running"` if it was a
//! `ToolStarted` with no later event for that id, `"idle"` otherwise.
//!
//! **What is NOT populated, and why (§5.4 AC-15.1 is only partially met):**
//! - `model` — always `None`. No production event ties a resolved model
//!   string to a stable `agent_id` yet; `LlmRequested`/`LlmResponded` carry
//!   `model`, but only `agent_name` (a string DOC-39 AC-13.2 explicitly
//!   retires as a correlation key), and folding by name would silently
//!   misattribute the model when two same-named agents run concurrently —
//!   exactly the bug #2898 existed to close. Closing this gap needs `model`
//!   threaded onto an `agent_id`-carrying event, which is out of this
//!   endpoint's scope.
//! - `task` — always `None`. `PmDelegating.agent`/`task_preview` predates
//!   `agent_id` and is keyed by name only, so it cannot be joined onto a
//!   specific spawn without the same name-collision risk as `model` above.
//! - `todos`, `files_changed` — always `[]`. No event or registry state
//!   tracks either today; §5.4 lists them in the target result shape but
//!   they are net-new domain state, not a folding gap.
//!
//! `AgentSpawned`/`AgentStarted`/`AgentDone`/`AgentFailed` (DOC-39 AC-13)
//! are NOT folded here even though their shape looks purpose-built for a
//! roster: `events.rs`'s own docs and #2898's CHANGELOG entry record that
//! none of them are emitted by any production call site yet, so folding them
//! would silently return an always-empty roster instead of the real,
//! already-flowing tool-attribution activity.
//! Test: `registry_agents::tests::*`; `protocol_agents::tests::*` covers the
//! RPC-level wiring.

use std::collections::HashMap;

use serde::Serialize;

use super::*;
use crate::events::Event;

/// `state` value for an agent whose most recent attributed event is a
/// `ToolStarted` with no later attributed event on record — a tool call is
/// (as far as the ring buffer shows) still in flight.
const STATE_RUNNING: &str = "running";
/// `state` value for an agent whose most recent attributed event is a
/// completion (`ToolFinished`/`ToolError`/`SearchPerformed`/
/// `MemoryRecalled`) — no tool call is known to be in flight.
const STATE_IDLE: &str = "idle";

/// One entry in `session.get_agents`'s live roster (DOC-39 §5.4).
///
/// Why: the wire shape `session.get_agents` returns per agent — see the
/// module docs for exactly which fields are folded from real state and which
/// are deferred defaults.
/// What: `agent_id` is the DOC-39 AC-13 stable per-spawn id; `name` is the
/// human-readable agent name (`"pm"` for the root loop, an agent config name
/// like `"python-engineer"` for a delegation). See module docs for
/// `model`/`task`/`todos`/`files_changed`.
/// Test: `registry_agents::tests::*`.
#[derive(Debug, Clone, Serialize)]
pub struct AgentRosterEntry {
    pub agent_id: String,
    pub name: String,
    pub model: Option<String>,
    pub state: String,
    pub task: Option<String>,
    pub todos: Vec<String>,
    pub files_changed: Vec<String>,
}

/// Extract `(agent, agent_id, is_running)` from the attribution events the
/// roster fold cares about, or `None` for every other `Event` variant.
///
/// Why: centralises which events count as roster-relevant activity — see the
/// module docs for the full attribution list and the tool-dispatch ordering
/// (`ToolStarted` -> `ToolFinished`/`ToolError` -> optional telemetry) that
/// makes `is_running` well-defined from event KIND alone, with no need to
/// correlate `call_id`.
/// What: `true` only for `ToolStarted`; every other attributed kind fires
/// only after its tool call already completed.
fn tool_attribution(event: &Event) -> Option<(&str, &str, bool)> {
    match event {
        Event::ToolStarted {
            agent, agent_id, ..
        } => Some((agent, agent_id, true)),
        Event::ToolFinished {
            agent, agent_id, ..
        } => Some((agent, agent_id, false)),
        Event::ToolError {
            agent, agent_id, ..
        } => Some((agent, agent_id, false)),
        Event::SearchPerformed {
            agent, agent_id, ..
        } => Some((agent, agent_id, false)),
        Event::MemoryRecalled {
            agent, agent_id, ..
        } => Some((agent, agent_id, false)),
        _ => None,
    }
}

impl SessionRegistry {
    /// `session.get_agents(session_id) -> { agents: [AgentRosterEntry] }`
    /// (DOC-39 §5.4).
    ///
    /// Why: the principled, daemon-owned replacement for the client-side
    /// ring-buffer fold §5.4 acknowledges as a time-boxed Phase-1 loan — see
    /// the module docs.
    /// What: `-32007 session_not_found` if `id` is unknown (via
    /// [`Self::replay`]). Otherwise folds the ring-buffer replay into one
    /// [`AgentRosterEntry`] per distinct `agent_id`, in the order each
    /// `agent_id` was first seen. A session with no tool activity yet
    /// returns an empty roster, not an error — the same "empty means nothing
    /// yet" convention `session.get_goals`/`session.get_transcript` use.
    /// Test: `registry_agents::tests::get_agents_empty_session_returns_empty_roster`,
    /// `registry_agents::tests::get_agents_running_tool_reports_running_state`,
    /// `registry_agents::tests::get_agents_finished_tool_reports_idle_state`,
    /// `registry_agents::tests::get_agents_unknown_session_errors`.
    pub fn get_agents(&self, id: &str) -> Result<Vec<AgentRosterEntry>, RpcError> {
        let envelopes = self.replay(id)?;
        let mut order: Vec<String> = Vec::new();
        let mut roster: HashMap<String, AgentRosterEntry> = HashMap::new();

        for envelope in &envelopes {
            let Some((agent, agent_id, running)) = tool_attribution(&envelope.event) else {
                continue;
            };
            if agent_id.is_empty() {
                // Pre-#2898 recorded events (or a test double) with no
                // stable id — nothing to key a roster row on.
                continue;
            }
            let entry = roster.entry(agent_id.to_string()).or_insert_with(|| {
                order.push(agent_id.to_string());
                AgentRosterEntry {
                    agent_id: agent_id.to_string(),
                    name: agent.to_string(),
                    model: None,
                    state: STATE_IDLE.to_string(),
                    task: None,
                    todos: Vec::new(),
                    files_changed: Vec::new(),
                }
            });
            entry.name = agent.to_string();
            entry.state = if running { STATE_RUNNING } else { STATE_IDLE }.to_string();
        }

        Ok(order
            .into_iter()
            .filter_map(|id| roster.remove(&id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session with no recorded tool activity returns an empty roster, not
    /// an error.
    #[test]
    fn get_agents_empty_session_returns_empty_roster() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let roster = registry.get_agents(&session.id).unwrap();

        assert!(roster.is_empty());
    }

    /// An agent whose most recent event is `ToolStarted` reports `"running"`.
    #[test]
    fn get_agents_running_tool_reports_running_state() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        registry
            .record_tool_started(
                &session.id,
                "python-engineer",
                "eng-1",
                "bash",
                "c1",
                "cargo test",
            )
            .unwrap();

        let roster = registry.get_agents(&session.id).unwrap();

        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].agent_id, "eng-1");
        assert_eq!(roster[0].name, "python-engineer");
        assert_eq!(roster[0].state, "running");
        assert_eq!(roster[0].model, None);
        assert_eq!(roster[0].task, None);
        assert!(roster[0].todos.is_empty());
        assert!(roster[0].files_changed.is_empty());
    }

    /// Once the started tool call finishes, that agent's state flips to
    /// `"idle"`.
    #[test]
    fn get_agents_finished_tool_reports_idle_state() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        registry
            .record_tool_started(&session.id, "pm", "pm-1", "bash", "c1", "echo hi")
            .unwrap();
        registry
            .record_tool_finished(&session.id, "pm", "pm-1", "bash", "c1", true, "hi")
            .unwrap();

        let roster = registry.get_agents(&session.id).unwrap();

        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].state, "idle");
    }

    /// Two distinct `agent_id`s produce two roster rows, in first-seen order,
    /// even when they share the same agent NAME — the DOC-39 AC-13 case this
    /// endpoint exists to make queryable.
    #[test]
    fn get_agents_distinguishes_same_named_concurrent_spawns() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        registry
            .record_tool_started(&session.id, "python-engineer", "spawn-a", "bash", "c1", "x")
            .unwrap();
        registry
            .record_tool_started(&session.id, "python-engineer", "spawn-b", "bash", "c2", "y")
            .unwrap();

        let roster = registry.get_agents(&session.id).unwrap();

        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].agent_id, "spawn-a");
        assert_eq!(roster[1].agent_id, "spawn-b");
        assert!(roster.iter().all(|a| a.name == "python-engineer"));
    }

    /// An unknown session must map to `-32007 session_not_found`.
    #[test]
    fn get_agents_unknown_session_errors() {
        let registry = SessionRegistry::new();
        let err = registry.get_agents("nope").unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
