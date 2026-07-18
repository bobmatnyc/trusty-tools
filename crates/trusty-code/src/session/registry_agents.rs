//! `SessionRegistry::get_agents` — the live agent-roster query behind
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
//! repaid by moving the fold server-side.
//!
//! **This module went through two designs, and the first one had a bug a
//! code-critic review on the #2962 PR caught as a HIGH.** The first cut
//! folded `SessionRegistry::replay`'s ring-buffer snapshot on every
//! `get_agents` call. That moved WHERE the fold ran (daemon, not client) but
//! not WHAT it could lose: the ring buffer is capacity-bounded
//! (`ring_capacity`, default [`super::DEFAULT_RING_CAPACITY`]) and evicts
//! oldest-first, so a long-running agent's `ToolStarted` could age out of
//! the ring before any LATER attributed event for that same `agent_id`
//! landed — making it vanish from the roster entirely, indistinguishable
//! from "never spawned" even though it may still genuinely be running. That
//! is the exact silent-data-loss shape §5.4's original text warned about;
//! folding server-side did not close it.
//! What: [`get_agents`] instead reads `SessionEntry::agents` — an
//! ALWAYS-RETAINED `Vec<AgentRosterState>`, kept up to date by
//! `SessionEntry::note_agent_activity` (called from
//! `SessionRegistry::record`, the SAME critical section that pushes onto the
//! ring — see that field's own docs in `registry.rs`) and evicted only when
//! the session itself goes away, never by ring capacity. This closes the
//! HIGH: an agent's roster row survives for the life of the session
//! regardless of how much ring-buffer traffic follows it.
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
//! are NOT sources here even though their shape looks purpose-built for a
//! roster: `events.rs`'s own docs and #2898's CHANGELOG entry record that
//! none of them are emitted by any production call site yet, so keying off
//! them would silently return an always-empty roster instead of the real,
//! already-flowing tool-attribution activity `registry.rs::tool_attribution`
//! sources from.
//! Test: `registry_agents::tests::*`; `protocol_agents::tests::*` covers the
//! RPC-level wiring.

use serde::Serialize;

use super::*;

/// `state` value for an agent whose live roster row (`AgentRosterState`) is
/// currently `running: true` — its last known event is an unmatched
/// `ToolStarted`.
const STATE_RUNNING: &str = "running";
/// `state` value for an agent whose live roster row is `running: false` — no
/// tool call is known to be in flight.
const STATE_IDLE: &str = "idle";

/// One entry in `session.get_agents`'s live roster (DOC-39 §5.4).
///
/// Why: the wire shape `session.get_agents` returns per agent — see the
/// module docs for exactly which fields are read from the always-retained
/// `SessionEntry::agents` state and which are deferred defaults.
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

impl SessionRegistry {
    /// `session.get_agents(session_id) -> { agents: [AgentRosterEntry] }`
    /// (DOC-39 §5.4).
    ///
    /// Why: the principled, daemon-owned, EVICTION-SAFE replacement for the
    /// client-side ring-buffer fold §5.4 originally acknowledged as a
    /// time-boxed Phase-1 loan — see the module docs for why reading the
    /// bounded ring directly (this endpoint's first cut) was itself still a
    /// silent-data-loss bug, not a fix.
    /// What: `-32007 session_not_found` if `id` is unknown. Otherwise
    /// projects `SessionEntry::agents` (the always-retained per-agent
    /// roster, in first-seen order) into one [`AgentRosterEntry`] per row. A
    /// session with no tool activity yet returns an empty roster, not an
    /// error — the same "empty means nothing yet" convention
    /// `session.get_goals`/`session.get_transcript` use.
    /// Test: `registry_agents::tests::get_agents_empty_session_returns_empty_roster`,
    /// `registry_agents::tests::get_agents_running_tool_reports_running_state`,
    /// `registry_agents::tests::get_agents_finished_tool_reports_idle_state`,
    /// `registry_agents::tests::get_agents_distinguishes_same_named_concurrent_spawns`,
    /// `registry_agents::tests::get_agents_survives_ring_eviction`,
    /// `registry_agents::tests::get_agents_unknown_session_errors`.
    pub fn get_agents(&self, id: &str) -> Result<Vec<AgentRosterEntry>, RpcError> {
        let sessions = self.lock();
        let entry = sessions
            .get(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        Ok(entry
            .agents
            .iter()
            .map(|a| AgentRosterEntry {
                agent_id: a.agent_id.clone(),
                name: a.name.clone(),
                model: None,
                state: if a.running { STATE_RUNNING } else { STATE_IDLE }.to_string(),
                task: None,
                todos: Vec::new(),
                files_changed: Vec::new(),
            })
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

    /// THE code-critic HIGH this module's second design closes: an agent
    /// whose `ToolStarted` has aged OUT of the (here, tiny) ring buffer must
    /// still appear in `get_agents`, still `"running"` — the ring's bounded
    /// capacity must never be able to make a still-running agent vanish from
    /// the roster. Uses `SessionRegistry::with_capacity` (the same seam
    /// `registry_tests.rs` uses for `ring_buffer_drops_oldest_when_full`) to
    /// force eviction cheaply instead of pushing thousands of events.
    #[test]
    fn get_agents_survives_ring_eviction() {
        let registry = SessionRegistry::with_capacity(2);
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        // Agent A starts a tool call — its `ToolStarted` is recorded at some
        // early `seq`.
        registry
            .record_tool_started(&session.id, "python-engineer", "agent-a", "bash", "c1", "x")
            .unwrap();

        // Agent B then starts and finishes enough tool calls to push A's
        // `ToolStarted` out of the (capacity-2) ring buffer entirely.
        for i in 0..5 {
            let call_id = format!("b-{i}");
            registry
                .record_tool_started(&session.id, "engineer-b", "agent-b", "bash", &call_id, "y")
                .unwrap();
            registry
                .record_tool_finished(
                    &session.id,
                    "engineer-b",
                    "agent-b",
                    "bash",
                    &call_id,
                    true,
                    "ok",
                )
                .unwrap();
        }

        // Sanity check: A's `ToolStarted` really is no longer in the replay
        // — this test would be vacuous otherwise.
        let replay = registry.replay(&session.id).unwrap();
        assert!(
            !replay.iter().any(|e| matches!(
                &e.event,
                crate::events::Event::ToolStarted { agent_id, .. } if agent_id == "agent-a"
            )),
            "test setup bug: agent-a's ToolStarted must have been evicted from the ring"
        );

        // THE assertion: agent-a still appears in the roster, still
        // "running" — the always-retained map survived what the ring did not.
        let roster = registry.get_agents(&session.id).unwrap();
        let agent_a = roster
            .iter()
            .find(|a| a.agent_id == "agent-a")
            .unwrap_or_else(|| panic!("agent-a must survive ring eviction: {roster:?}"));
        assert_eq!(agent_a.state, "running");
        assert_eq!(agent_a.name, "python-engineer");

        let agent_b = roster.iter().find(|a| a.agent_id == "agent-b").unwrap();
        assert_eq!(agent_b.state, "idle");
    }

    /// An unknown session must map to `-32007 session_not_found`.
    #[test]
    fn get_agents_unknown_session_errors() {
        let registry = SessionRegistry::new();
        let err = registry.get_agents("nope").unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
