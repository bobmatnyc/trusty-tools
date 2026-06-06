//! Persistent agent sessions — conversation history across multiple agent calls.
//!
//! Why: Some workflows (and interactive PM flows) benefit from agents that
//! remember earlier turns, so a later phase can build on what an earlier
//! phase already established. Sessions are scoped by agent name and opt-in
//! per agent via `persistent_session = true` in the agent TOML. They can be
//! explicitly cleared with `--clear-sessions`.
//! What: `AgentSession` holds a `Vec<ChatCompletionRequestMessage>` of chat
//! history; `SessionManager` is an `Arc<Mutex<HashMap<..>>>` keyed by agent
//! name that exposes async `get_history`, `extend_history`, `clear_agent`,
//! and `clear_all`.
//!
//! `HistoryMessage` (the portable `{role, content}` IPC wire type) was moved
//! to `trusty-agents-common::runner` in Wave 2 (issue #867, refs #830/#832)
//! and is re-exported here so all existing `crate::session::HistoryMessage`
//! references resolve unchanged.
//!
//! `history_message_into_typed()` is the async-openai conversion helper that
//! was previously `HistoryMessage::into_typed()`. Because the struct now lives
//! in `trusty-agents-common` (which does not depend on async-openai), the
//! method has been hoisted to a standalone function in this module.
//!
//! Test: See the `tests` module — empty history for new agent, extend +
//! retrieve, isolated clear per agent, and global clear.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_openai::types::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestUserMessageArgs,
};
use tokio::sync::Mutex;

// Why: `HistoryMessage` was extracted to `trusty-agents-common::runner` in
//      Wave 2 (issue #867) so external crates that implement `AgentRunner`
//      can reference the IPC wire type without depending on `trusty-agents`.
//      Re-exporting here preserves every existing `crate::session::HistoryMessage`
//      import in the workspace.
// What: Explicit re-export of the portable struct.
// Test: All existing tests that construct HistoryMessage values still pass.
pub use trusty_agents_common::runner::HistoryMessage;

/// Convert a `HistoryMessage` into async-openai's typed message format.
///
/// Why: This was previously `HistoryMessage::into_typed(self)` but the struct
/// now lives in `trusty-agents-common` which does not depend on `async-openai`.
/// Hoisting the conversion to a standalone function here keeps the async-openai
/// coupling in `trusty-agents` where the dependency already exists.
/// What: Dispatches on role; unknown roles (including "system") default to user.
/// Test: `history_message_typed_round_trip` in `session::tests`.
pub fn history_message_into_typed(msg: HistoryMessage) -> Result<ChatCompletionRequestMessage> {
    match msg.role.as_str() {
        "assistant" => Ok(ChatCompletionRequestAssistantMessageArgs::default()
            .content(msg.content)
            .build()
            .context("failed to build assistant message")?
            .into()),
        _ => Ok(ChatCompletionRequestUserMessageArgs::default()
            .content(msg.content)
            .build()
            .context("failed to build user message")?
            .into()),
    }
}

/// Conversation history for a single agent.
///
/// Why: Wrapping a `Vec<ChatCompletionRequestMessage>` in its own type gives
/// us a narrow API (append_user / append_assistant / clear) and keeps
/// future invariants (e.g. cap length, de-dup) localized.
/// What: Owns the full message history for one agent; cloned out when needed
/// by callers (cheap — messages are small typed values).
/// Test: `append_user_and_assistant_builds_history` verifies order.
#[derive(Debug, Clone, Default)]
pub struct AgentSession {
    pub history: Vec<ChatCompletionRequestMessage>,
}

impl AgentSession {
    /// Clear all history for this agent.
    ///
    /// Why: Needed for `--clear-sessions` and for explicit in-workflow resets.
    /// What: Drops every stored message.
    /// Test: `clear_empties_history`.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.history.clear();
    }

    /// Append a user message to the history.
    ///
    /// Why: Callers (the workflow engine) need to record every turn to keep
    /// context coherent for the next call.
    /// What: Builds a `ChatCompletionRequestMessage::User` and pushes it.
    /// Test: `append_user_and_assistant_builds_history`.
    pub fn append_user(&mut self, content: String) -> Result<()> {
        let msg: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
            .content(content)
            .build()
            .context("failed to build user history message")?
            .into();
        self.history.push(msg);
        Ok(())
    }

    /// Append an assistant message to the history.
    ///
    /// Why: Pairs with `append_user` so sessions carry the full dialog.
    /// What: Builds a `ChatCompletionRequestMessage::Assistant` and pushes it.
    /// Test: `append_user_and_assistant_builds_history`.
    pub fn append_assistant(&mut self, content: String) -> Result<()> {
        let msg: ChatCompletionRequestMessage =
            ChatCompletionRequestAssistantMessageArgs::default()
                .content(content)
                .build()
                .context("failed to build assistant history message")?
                .into();
        self.history.push(msg);
        Ok(())
    }
}

/// Thread-safe session store keyed by agent name.
///
/// Why: The workflow engine and any future interactive runner need to share
/// a single source of truth for per-agent history; an `Arc<Mutex<HashMap>>`
/// is the minimal correct primitive.
/// What: Async getters/mutators that clone history out (so the lock is held
/// only briefly) and extend history in-place.
/// Test: See the `tests` module — every public method has a dedicated test.
#[derive(Debug, Clone, Default)]
pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, AgentSession>>>,
}

impl SessionManager {
    /// Construct an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch a clone of the current history for `agent_name`.
    ///
    /// Why: Callers need to pass the history to the sub-agent without holding
    /// the lock across the subprocess boundary; cloning is cheap relative to
    /// an LLM round-trip.
    /// What: Returns `Vec<ChatCompletionRequestMessage>` (empty if unknown).
    /// Test: `get_history_empty_for_unknown_agent`.
    #[allow(dead_code)]
    pub async fn get_history(&self, agent_name: &str) -> Vec<ChatCompletionRequestMessage> {
        let guard = self.sessions.lock().await;
        guard
            .get(agent_name)
            .map(|s| s.history.clone())
            .unwrap_or_default()
    }

    /// Fetch history in wire-format (HistoryMessage) for IPC.
    ///
    /// Why: `ChatCompletionRequestMessage` doesn't serialize through our IPC;
    /// the wire form is a flat `{role, content}` pair.
    /// What: Converts the typed history to `HistoryMessage` values. System
    /// messages (rare here — the engine never appends system turns) are
    /// flattened to their textual content.
    /// Test: `get_history_wire_matches_extend`.
    pub async fn get_history_wire(&self, agent_name: &str) -> Vec<HistoryMessage> {
        let guard = self.sessions.lock().await;
        let Some(sess) = guard.get(agent_name) else {
            return Vec::new();
        };

        // We only ever append_user / append_assistant ourselves, so each stored
        // message has a known role. Serializing via serde_json and reading back
        // a {role, content} shape is the simplest robust conversion that
        // doesn't break when async-openai tweaks its internal types.
        sess.history
            .iter()
            .filter_map(|m| {
                let v = serde_json::to_value(m).ok()?;
                let role = v.get("role")?.as_str()?.to_string();
                let content = v
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(HistoryMessage { role, content })
            })
            .collect()
    }

    /// Append a (user, assistant) exchange to the named session.
    ///
    /// Why: Every successful agent call in persistent mode records exactly
    /// one turn-pair; centralizing the append here keeps all ordering in one
    /// place.
    /// What: Locks, inserts-or-updates the `AgentSession`, appends the user
    /// message, then the assistant message.
    /// Test: `extend_and_retrieve_round_trip`.
    pub async fn extend_history(
        &self,
        agent_name: &str,
        user_msg: &str,
        assistant_msg: &str,
    ) -> Result<()> {
        let mut guard = self.sessions.lock().await;
        let entry = guard
            .entry(agent_name.to_string())
            .or_insert_with(AgentSession::default);
        entry.append_user(user_msg.to_string())?;
        entry.append_assistant(assistant_msg.to_string())?;
        Ok(())
    }

    /// Clear one agent's session (no-op if absent).
    ///
    /// Why: Callers sometimes want to reset a specific agent mid-run (e.g. a
    /// failing QA retry) without disturbing peers.
    /// What: Removes the entry entirely (equivalent to empty history).
    /// Test: `clear_specific_agent_preserves_others`.
    #[allow(dead_code)]
    pub async fn clear_agent(&self, agent_name: &str) {
        let mut guard = self.sessions.lock().await;
        guard.remove(agent_name);
    }

    /// Clear every agent's session.
    ///
    /// Why: `--clear-sessions` CLI flag; also useful in tests.
    /// What: Replaces the inner map with an empty one.
    /// Test: `clear_all_removes_every_entry`.
    #[allow(dead_code)]
    pub async fn clear_all(&self) {
        let mut guard = self.sessions.lock().await;
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_user_and_assistant_builds_history() {
        let mut s = AgentSession::default();
        s.append_user("hello".into()).unwrap();
        s.append_assistant("hi there".into()).unwrap();
        assert_eq!(s.history.len(), 2);
    }

    #[test]
    fn clear_empties_history() {
        let mut s = AgentSession::default();
        s.append_user("x".into()).unwrap();
        assert_eq!(s.history.len(), 1);
        s.clear();
        assert!(s.history.is_empty());
    }

    #[test]
    fn history_message_typed_round_trip() {
        let u = HistoryMessage::user("hello");
        let a = HistoryMessage::assistant("world");
        let _: ChatCompletionRequestMessage = history_message_into_typed(u).unwrap();
        let _: ChatCompletionRequestMessage = history_message_into_typed(a).unwrap();
    }

    #[tokio::test]
    async fn get_history_empty_for_unknown_agent() {
        let mgr = SessionManager::new();
        let h = mgr.get_history("nobody").await;
        assert!(h.is_empty());
    }

    #[tokio::test]
    async fn extend_and_retrieve_round_trip() {
        let mgr = SessionManager::new();
        mgr.extend_history("coder", "task 1", "answer 1")
            .await
            .unwrap();
        mgr.extend_history("coder", "task 2", "answer 2")
            .await
            .unwrap();
        let h = mgr.get_history("coder").await;
        // two exchanges = four messages (user, assistant, user, assistant)
        assert_eq!(h.len(), 4);
    }

    #[tokio::test]
    async fn get_history_wire_matches_extend() {
        let mgr = SessionManager::new();
        mgr.extend_history("a", "question", "response")
            .await
            .unwrap();
        let wire = mgr.get_history_wire("a").await;
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0].role, "user");
        assert_eq!(wire[0].content, "question");
        assert_eq!(wire[1].role, "assistant");
        assert_eq!(wire[1].content, "response");
    }

    #[tokio::test]
    async fn clear_specific_agent_preserves_others() {
        let mgr = SessionManager::new();
        mgr.extend_history("a", "u1", "r1").await.unwrap();
        mgr.extend_history("b", "u2", "r2").await.unwrap();

        mgr.clear_agent("a").await;

        assert!(mgr.get_history("a").await.is_empty());
        assert_eq!(mgr.get_history("b").await.len(), 2);
    }

    #[tokio::test]
    async fn clear_all_removes_every_entry() {
        let mgr = SessionManager::new();
        mgr.extend_history("a", "u", "r").await.unwrap();
        mgr.extend_history("b", "u", "r").await.unwrap();
        mgr.clear_all().await;
        assert!(mgr.get_history("a").await.is_empty());
        assert!(mgr.get_history("b").await.is_empty());
    }
}
