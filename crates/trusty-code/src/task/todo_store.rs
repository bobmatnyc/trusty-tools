//! Concrete [`TodoStore`](crate::tools::TodoStore) writing the `todo_write`
//! tool's list onto one session's roster row (#8235).
//!
//! Why: `tools::checklist` must not depend on `session`, and the tool must not
//! learn which session or agent it serves — the same layering, and the same
//! reason, as `task::sink`'s `SessionToolEventSink` (see
//! `agent_loop::sink`'s module docs). This is the glue: `task` is the
//! daemon-execution layer that knows both halves.
//! What: [`SessionTodoStore`] holds an `Arc<SessionRegistry>` plus the
//! session id and the WRITER's `agent`/`agent_id`, so the tool's every write
//! is attributed to the loop that registered it and cannot address another
//! agent's list. A registry error becomes
//! [`TodoError::Unavailable`](crate::tools::TodoError::Unavailable), which
//! the tool reports to the model as a recoverable failure over an unchanged
//! list.
//! Test: `tests::*` — the tool-to-`session.get_agents` round trip #8235 asks
//! for, asserted through the read path rather than the store's own state.

use std::sync::Arc;

use crate::events::TodoItem;
use crate::session::SessionRegistry;
use crate::tools::{TodoError, TodoStore};

/// Writes one agent's checklist into one session's roster (#8235).
///
/// Why: see module docs.
/// What: every field is fixed at construction — the tool passes only the
/// list, so it can neither retarget the session nor forge another agent's
/// identity.
/// Test: `tests::tool_write_is_visible_through_get_agents`.
pub struct SessionTodoStore {
    registry: Arc<SessionRegistry>,
    session_id: String,
    agent: String,
    agent_id: String,
}

impl SessionTodoStore {
    /// Build a store targeting `session_id`, attributing writes to
    /// `agent`/`agent_id`.
    pub fn new(
        registry: Arc<SessionRegistry>,
        session_id: impl Into<String>,
        agent: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            session_id: session_id.into(),
            agent: agent.into(),
            agent_id: agent_id.into(),
        }
    }
}

impl TodoStore for SessionTodoStore {
    /// Forward the validated list to `SessionRegistry::set_agent_todos`.
    ///
    /// Why: an `RpcError` here means the session went away mid-run, which the
    /// model can act on (stop planning, finish) — so it is reported rather
    /// than logged and swallowed. A swallowed failure would be the fail-open
    /// shape #8235 forbids: a success over an unchanged list.
    /// Test: `tests::a_vanished_session_is_reported_not_swallowed`.
    fn replace(&self, todos: Vec<TodoItem>) -> Result<(), TodoError> {
        self.registry
            .set_agent_todos(&self.session_id, &self.agent, &self.agent_id, todos)
            .map_err(|e| TodoError::Unavailable(e.message))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::binding::ProjectBinding;
    use crate::events::TodoStatus;
    use crate::tools::{TodoWriteTool, ToolExecutor};

    fn session() -> (Arc<SessionRegistry>, String) {
        let registry = Arc::new(SessionRegistry::new());
        let created = registry.create("t".to_string(), None, ProjectBinding::None);
        (registry, created.id)
    }

    /// THE #8235 round trip: a `todo_write` call by the model is visible in
    /// `session.get_agents`'s `todos` — asserted through the read path, not by
    /// inspecting the tool or the store.
    #[tokio::test]
    async fn tool_write_is_visible_through_get_agents() {
        let (registry, id) = session();
        let tool = TodoWriteTool::new(Arc::new(SessionTodoStore::new(
            Arc::clone(&registry),
            &id,
            "pm",
            "pm-1",
        )));

        let result = tool
            .execute(json!({"todos": [
                {"content": "add the --json flag", "status": "completed"},
                {"content": "add a test", "status": "in_progress"},
                {"content": "add a changelog line", "status": "pending"}
            ]}))
            .await;

        assert!(!result.is_error(), "{result:?}");
        let roster = registry.get_agents(&id).expect("roster");
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].agent_id, "pm-1");
        let todos = &roster[0].todos;
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[1].content, "add a test");
        assert_eq!(todos[1].status, TodoStatus::InProgress);
    }

    /// A refused call leaves the previously stored list exactly as it was —
    /// the fail-open check, asserted at the read path.
    #[tokio::test]
    async fn a_refused_call_leaves_the_stored_list_unchanged() {
        let (registry, id) = session();
        let tool = TodoWriteTool::new(Arc::new(SessionTodoStore::new(
            Arc::clone(&registry),
            &id,
            "pm",
            "pm-1",
        )));
        tool.execute(json!({"todos": [{"content": "plan", "status": "in_progress"}]}))
            .await;

        let refused = tool
            .execute(json!({"todos": [
                {"content": "a", "status": "in_progress"},
                {"content": "b", "status": "in_progress"}
            ]}))
            .await;

        assert!(refused.is_error());
        let roster = registry.get_agents(&id).expect("roster");
        assert_eq!(roster[0].todos.len(), 1);
        assert_eq!(roster[0].todos[0].content, "plan");
    }

    /// A store whose session has gone reports the failure to the model rather
    /// than returning success over a dropped write.
    #[tokio::test]
    async fn a_vanished_session_is_reported_not_swallowed() {
        let registry = Arc::new(SessionRegistry::new());
        let tool = TodoWriteTool::new(Arc::new(SessionTodoStore::new(
            registry, "gone", "pm", "pm-1",
        )));

        let result = tool
            .execute(json!({"todos": [{"content": "a", "status": "pending"}]}))
            .await;

        assert!(result.is_error(), "{result:?}");
    }

    /// A sub-agent's store writes its OWN roster row; the PM's plan is
    /// untouched. This is the scope boundary #8235's dispatch asks to be
    /// pinned: a delegated agent handed this tool cannot rewrite the PM's
    /// checklist, because its store carries its own `agent_id`.
    #[tokio::test]
    async fn a_sub_agent_cannot_rewrite_the_pms_list() {
        let (registry, id) = session();
        let pm = TodoWriteTool::new(Arc::new(SessionTodoStore::new(
            Arc::clone(&registry),
            &id,
            "pm",
            "pm-1",
        )));
        let engineer = TodoWriteTool::new(Arc::new(SessionTodoStore::new(
            Arc::clone(&registry),
            &id,
            "engineer",
            "eng-1",
        )));
        pm.execute(json!({"todos": [{"content": "route the work", "status": "in_progress"}]}))
            .await;

        engineer
            .execute(json!({"todos": [{"content": "write the code", "status": "in_progress"}]}))
            .await;

        let roster = registry.get_agents(&id).expect("roster");
        let pm_row = roster
            .iter()
            .find(|a| a.agent_id == "pm-1")
            .expect("pm row");
        assert_eq!(pm_row.todos.len(), 1);
        assert_eq!(pm_row.todos[0].content, "route the work");
        let eng_row = roster
            .iter()
            .find(|a| a.agent_id == "eng-1")
            .expect("engineer row");
        assert_eq!(eng_row.todos[0].content, "write the code");
    }
}
