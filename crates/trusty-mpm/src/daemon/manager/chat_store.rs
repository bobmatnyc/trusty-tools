//! Conversation-keyed turn store for the portfolio chat loop (WI-4, #2581).
//!
//! Why: DOC-36 §3.2 keys `POST /manager/chat` on a caller-supplied
//! `conversation_key`, "the same shape as SessionProxy's focus-map keying"
//! (`client/proxy.rs`, a `Mutex<HashMap<String, _>>` keyed by conversation key).
//! The chat loop needs recent turns as context so a multi-turn conversation is
//! coherent, but must stay bounded so a long-lived key cannot grow without limit.
//! This is the in-memory half of that state; the durable half is the portfolio
//! palace dual-write (WI-5), which degrades silently when unavailable.
//! What: [`ChatStore`] wraps a `Mutex<HashMap<String, Vec<ChatTurn>>>` keyed
//! IDENTICALLY to the proxy focus map (plain `String` conversation key), with a
//! bounded [`ChatStore::history`]/[`ChatStore::record_exchange`] surface. Each
//! [`ChatTurn`] carries a [`TurnRole`] and its text; the window keeps the most
//! recent [`DEFAULT_MAX_TURNS`] messages per conversation.
//! Test: `record_and_history_round_trips`, `history_is_bounded`,
//! `distinct_keys_are_isolated` in `chat_store_tests.rs`.

use std::collections::HashMap;
use std::sync::Mutex;

/// Maximum stored messages (user + assistant) retained per conversation.
///
/// Why: bounds memory for a long-lived key while keeping enough recent context
/// for a coherent multi-turn reply; 20 messages ≈ 10 exchanges, ample for a
/// portfolio Q&A loop.
/// What: the per-key cap enforced by [`ChatStore::record_exchange`].
/// Test: `history_is_bounded`.
pub const DEFAULT_MAX_TURNS: usize = 20;

/// The speaker of a stored conversation turn.
///
/// Why: replaying history as [`trusty_common::inference::ChatMessage`]s needs the
/// role; a typed enum avoids stringly-typed role bugs at the call site.
/// What: the two roles a read-only portfolio chat produces (no tool/system turns
/// are persisted — those are rebuilt fresh from live state each request).
/// Test: `record_and_history_round_trips`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRole {
    /// A caller message.
    User,
    /// A manager reply.
    Assistant,
}

/// One stored conversation turn.
///
/// Why: the unit of replayed context; keeping role + content together makes
/// [`ChatStore::history`] a direct source for rebuilding the prompt history.
/// What: the [`TurnRole`] and the turn's text.
/// Test: `record_and_history_round_trips`.
#[derive(Debug, Clone)]
pub struct ChatTurn {
    /// Who produced this turn.
    pub role: TurnRole,
    /// The turn's text content.
    pub content: String,
}

/// Bounded, conversation-keyed store of recent portfolio chat turns.
///
/// Why: the shared, daemon-owned context store threaded through
/// [`crate::daemon::manager::ManagerState`]; keying it exactly like the L2 proxy
/// focus map keeps the L3 chat surface channel-agnostic and consistent with the
/// existing conversation-keying convention.
/// What: a `Mutex`-guarded map from conversation key to a capped `Vec<ChatTurn>`.
/// Cheap to read/write (a short vector clone); no async, no I/O.
/// Test: `record_and_history_round_trips`, `history_is_bounded`, `distinct_keys_are_isolated`.
#[derive(Debug)]
pub struct ChatStore {
    /// conversation key → recent turns (most-recent-last), capped at `max_turns`.
    conversations: Mutex<HashMap<String, Vec<ChatTurn>>>,
    /// Per-conversation retention cap.
    max_turns: usize,
}

impl Default for ChatStore {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TURNS)
    }
}

impl ChatStore {
    /// Construct an empty store with the given per-conversation retention cap.
    ///
    /// Why: the cap is injectable so a test can prove the bounding without pushing
    /// 20 turns.
    /// What: an empty map plus the retention cap.
    /// Test: `history_is_bounded`.
    pub fn new(max_turns: usize) -> Self {
        Self {
            conversations: Mutex::new(HashMap::new()),
            max_turns: max_turns.max(2),
        }
    }

    /// The recent turns for a conversation key, oldest-first.
    ///
    /// Why: the chat handler replays these as prior history before the new user
    /// message so the reply is contextual.
    /// What: clones the capped turn vector under `key`, or an empty vector.
    /// Test: `record_and_history_round_trips`.
    pub fn history(&self, key: &str) -> Vec<ChatTurn> {
        self.conversations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// Append a completed user→assistant exchange, trimming to the cap.
    ///
    /// Why: a turn is only recorded after a successful reply, so a failed/degraded
    /// request never pollutes history. Trimming from the front keeps the window on
    /// the most recent turns.
    /// What: pushes the user then assistant turn under `key`, then truncates the
    /// front so at most `max_turns` remain. Returns the resulting turn count.
    /// Test: `record_and_history_round_trips`, `history_is_bounded`.
    pub fn record_exchange(&self, key: &str, user: &str, assistant: &str) -> usize {
        let mut map = self.conversations.lock().unwrap_or_else(|p| p.into_inner());
        let turns = map.entry(key.to_string()).or_default();
        turns.push(ChatTurn {
            role: TurnRole::User,
            content: user.to_string(),
        });
        turns.push(ChatTurn {
            role: TurnRole::Assistant,
            content: assistant.to_string(),
        });
        if turns.len() > self.max_turns {
            let overflow = turns.len() - self.max_turns;
            turns.drain(0..overflow);
        }
        turns.len()
    }
}

#[cfg(test)]
#[path = "chat_store_tests.rs"]
mod tests;
