//! `SessionRegistry::set_agent_todos` — the session checklist's write path
//! (#8235). Split out of `registry.rs` for the same 500-SLOC-cap reason as
//! `registry_events.rs`; this is a child module of `registry` (declared via
//! `#[path = ...] mod todo_ops;`), so it shares full access to
//! `SessionRegistry`'s private `lock`/`record` helpers and `SessionEntry`'s
//! fields exactly as if these methods were still defined in that file.
//!
//! Why: DOC-39 §5.4 reserved `AgentRosterEntry.todos` and
//! `session::registry_agents`'s own module docs recorded it as "always `[]` —
//! no event or registry state tracks it today". This is that state. It lives
//! on the roster row (`AgentRosterState.todos`) rather than on the
//! `Transcript` where goal slots live, for three reasons: DOC-39 §4.5
//! requires checklists to be PER-AGENT, the roster is the read path #8235
//! names, and a `Transcript` only exists after the session's first `task.run`
//! (see `registry_goals`'s "no transcript yet" error) whereas a roster row is
//! created by the first attributed event of any agent.
//! What: [`SessionRegistry::set_agent_todos`] find-or-inserts the writer's
//! roster row, replaces its list whole, and then records
//! `Event::TodosChanged` so an attached client learns of the change without
//! polling. The write is keyed by `agent_id`, so two agents in one session
//! own two lists and neither can rewrite the other's; two SESSIONS are
//! already isolated by `SessionEntry`.
//! Test: `tests::*`; `session::protocol_agents::tests::get_agents_reports_a_written_checklist`
//! covers the RPC read path and `task::todo_store::tests::*` the tool round
//! trip.

use super::*;
use crate::events::TodoItem;

impl SessionRegistry {
    /// Replace `agent_id`'s checklist in session `id`, then stream the change
    /// (#8235).
    ///
    /// Why: the ONE write path onto the roster's `todos`, so the state and
    /// the `Event::TodosChanged` that announces it can never disagree — a
    /// second call site could update one without the other.
    /// What: `Err(session_not_found)` if `id` is unknown;
    /// `Err(invalid_argument)` for an empty `agent_id`, which has no roster
    /// row to key on and would therefore make the write silently invisible.
    /// Otherwise find-or-inserts the row (a new row starts `running: false` —
    /// writing a checklist is not a tool call in flight), replaces `todos`
    /// whole, releases the lock, and records the event. An existing row keeps
    /// its `running` state and its `name` is refreshed, matching
    /// `SessionEntry::note_agent_activity`'s last-writer-wins convention.
    /// Test: `tests::set_agent_todos_writes_the_roster_row`,
    /// `tests::set_agent_todos_replaces_the_whole_list`,
    /// `tests::set_agent_todos_keeps_two_agents_lists_apart`,
    /// `tests::set_agent_todos_keeps_two_sessions_lists_apart`,
    /// `tests::set_agent_todos_streams_a_change_event`,
    /// `tests::set_agent_todos_unknown_session_errors`,
    /// `tests::set_agent_todos_empty_agent_id_errors`,
    /// `tests::set_agent_todos_leaves_running_state_alone`.
    pub fn set_agent_todos(
        &self,
        id: &str,
        agent: &str,
        agent_id: &str,
        todos: Vec<TodoItem>,
    ) -> Result<(), RpcError> {
        if agent_id.is_empty() {
            return Err(RpcError::invalid_argument(
                "a checklist needs a non-empty agent_id to attribute it to".to_string(),
            ));
        }
        {
            let mut sessions = self.lock();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| RpcError::session_not_found(id))?;
            match entry.agents.iter_mut().find(|a| a.agent_id == agent_id) {
                Some(existing) => {
                    existing.name = agent.to_string();
                    existing.todos = todos.clone();
                }
                None => entry.agents.push(AgentRosterState {
                    agent_id: agent_id.to_string(),
                    name: agent.to_string(),
                    running: false,
                    todos: todos.clone(),
                }),
            }
        }
        self.record(
            id,
            Event::TodosChanged {
                session_id: id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                todos,
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::TodoStatus;

    /// `(content, status)` pairs as [`TodoItem`]s.
    fn items(pairs: &[(&str, TodoStatus)]) -> Vec<TodoItem> {
        pairs
            .iter()
            .map(|(content, status)| TodoItem {
                content: (*content).to_string(),
                status: *status,
            })
            .collect()
    }

    fn registry_with_session() -> (SessionRegistry, String) {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        (registry, session.id)
    }

    /// A write on a session with no prior tool activity still lands: it mints
    /// the roster row, so a checklist written before the first tool call is
    /// not lost.
    #[test]
    fn set_agent_todos_writes_the_roster_row() {
        let (registry, id) = registry_with_session();

        registry
            .set_agent_todos(
                &id,
                "pm",
                "pm-1",
                items(&[("add the flag", TodoStatus::InProgress)]),
            )
            .expect("write");

        let roster = registry.get_agents(&id).expect("roster");
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].agent_id, "pm-1");
        assert_eq!(roster[0].todos.len(), 1);
        assert_eq!(roster[0].todos[0].content, "add the flag");
        assert_eq!(roster[0].todos[0].status, TodoStatus::InProgress);
    }

    /// The second write REPLACES the first list rather than appending to it —
    /// the whole-list-replace semantics #8235 specifies.
    #[test]
    fn set_agent_todos_replaces_the_whole_list() {
        let (registry, id) = registry_with_session();
        registry
            .set_agent_todos(
                &id,
                "pm",
                "pm-1",
                items(&[("a", TodoStatus::Pending), ("b", TodoStatus::Pending)]),
            )
            .expect("first write");

        registry
            .set_agent_todos(&id, "pm", "pm-1", items(&[("a", TodoStatus::Completed)]))
            .expect("second write");

        let roster = registry.get_agents(&id).expect("roster");
        assert_eq!(roster[0].todos.len(), 1, "append, not replace");
        assert_eq!(roster[0].todos[0].status, TodoStatus::Completed);
    }

    /// An empty list clears the checklist.
    #[test]
    fn set_agent_todos_empty_list_clears() {
        let (registry, id) = registry_with_session();
        registry
            .set_agent_todos(&id, "pm", "pm-1", items(&[("a", TodoStatus::Pending)]))
            .expect("write");

        registry
            .set_agent_todos(&id, "pm", "pm-1", Vec::new())
            .expect("clear");

        let roster = registry.get_agents(&id).expect("roster");
        assert!(roster[0].todos.is_empty());
    }

    /// THE subagent boundary: a delegated agent writing its own checklist
    /// leaves the PM's plan untouched, because the list is keyed by
    /// `agent_id`.
    #[test]
    fn set_agent_todos_keeps_two_agents_lists_apart() {
        let (registry, id) = registry_with_session();
        registry
            .set_agent_todos(
                &id,
                "pm",
                "pm-1",
                items(&[("route the work", TodoStatus::InProgress)]),
            )
            .expect("pm write");

        registry
            .set_agent_todos(
                &id,
                "engineer",
                "eng-1",
                items(&[("write the code", TodoStatus::Pending)]),
            )
            .expect("engineer write");

        let roster = registry.get_agents(&id).expect("roster");
        let pm = roster
            .iter()
            .find(|a| a.agent_id == "pm-1")
            .expect("pm row");
        let eng = roster
            .iter()
            .find(|a| a.agent_id == "eng-1")
            .expect("engineer row");
        assert_eq!(pm.todos.len(), 1);
        assert_eq!(pm.todos[0].content, "route the work");
        assert_eq!(eng.todos[0].content, "write the code");
    }

    /// Two concurrent sessions never see each other's lists — #8235's closure
    /// condition, and the property trusty-memory's palace-scoped task slots
    /// could not offer.
    #[test]
    fn set_agent_todos_keeps_two_sessions_lists_apart() {
        let registry = SessionRegistry::new();
        let a = registry.create("a".to_string(), None, crate::binding::ProjectBinding::None);
        let b = registry.create("b".to_string(), None, crate::binding::ProjectBinding::None);
        registry
            .set_agent_todos(&a.id, "pm", "pm-1", items(&[("in a", TodoStatus::Pending)]))
            .expect("write a");

        registry
            .set_agent_todos(&b.id, "pm", "pm-1", items(&[("in b", TodoStatus::Pending)]))
            .expect("write b");

        assert_eq!(
            registry.get_agents(&a.id).expect("roster a")[0].todos[0].content,
            "in a"
        );
        assert_eq!(
            registry.get_agents(&b.id).expect("roster b")[0].todos[0].content,
            "in b"
        );
    }

    /// The write streams `Event::TodosChanged` carrying the same list, so an
    /// attached client needs no follow-up read.
    #[test]
    fn set_agent_todos_streams_a_change_event() {
        let (registry, id) = registry_with_session();

        registry
            .set_agent_todos(
                &id,
                "pm",
                "pm-1",
                items(&[("add a test", TodoStatus::InProgress)]),
            )
            .expect("write");

        let replay = registry.replay(&id).expect("replay");
        let streamed = replay
            .iter()
            .find_map(|e| match &e.event {
                Event::TodosChanged {
                    agent,
                    agent_id,
                    todos,
                    ..
                } => Some((agent.clone(), agent_id.clone(), todos.clone())),
                _ => None,
            })
            .expect("a todos_changed event must be recorded");
        assert_eq!(streamed.0, "pm");
        assert_eq!(streamed.1, "pm-1");
        assert_eq!(streamed.2[0].content, "add a test");
    }

    /// Writing a checklist must not report an agent as running — only a
    /// `ToolStarted` does that.
    #[test]
    fn set_agent_todos_leaves_running_state_alone() {
        let (registry, id) = registry_with_session();
        registry
            .record_tool_started(&id, "pm", "pm-1", "bash", "c1", "ls")
            .expect("tool started");

        registry
            .set_agent_todos(&id, "pm", "pm-1", items(&[("a", TodoStatus::Pending)]))
            .expect("write");

        let roster = registry.get_agents(&id).expect("roster");
        assert_eq!(
            roster[0].state, "running",
            "a checklist write must not clear an in-flight tool call"
        );
    }

    /// An unknown session maps to `-32007 session_not_found`.
    #[test]
    fn set_agent_todos_unknown_session_errors() {
        let registry = SessionRegistry::new();
        let err = registry
            .set_agent_todos("nope", "pm", "pm-1", Vec::new())
            .expect_err("unknown session");
        assert_eq!(err.code, -32007);
    }

    /// An empty `agent_id` is refused rather than silently dropped.
    #[test]
    fn set_agent_todos_empty_agent_id_errors() {
        let (registry, id) = registry_with_session();

        let err = registry
            .set_agent_todos(&id, "pm", "", items(&[("a", TodoStatus::Pending)]))
            .expect_err("empty agent_id");

        assert!(
            err.message.contains("agent_id"),
            "not actionable: {}",
            err.message
        );
        assert!(registry.get_agents(&id).expect("roster").is_empty());
    }
}
