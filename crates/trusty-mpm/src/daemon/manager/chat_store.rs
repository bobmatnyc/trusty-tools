//! Conversation-keyed turn store for the portfolio chat loop (WI-4, #2581).
//!
//! Why: DOC-36 §3.2 keys `POST /manager/chat` on a caller-supplied
//! `conversation_key`, "the same shape as SessionProxy's focus-map keying"
//! (`client/proxy.rs`, a `Mutex<HashMap<String, _>>` keyed by conversation key).
//! The chat loop needs recent turns as context so a multi-turn conversation is
//! coherent, but must stay bounded so a long-lived key cannot grow without limit.
//! This is the in-memory half of that state; the durable half is the portfolio
//! palace dual-write (WI-5), which degrades silently when unavailable.
//! What: [`ChatStore`] wraps a `Mutex<LruCache<String, Vec<ChatTurn>>>` keyed
//! IDENTICALLY to the proxy focus map (plain `String` conversation key), with a
//! bounded [`ChatStore::history`]/[`ChatStore::record_exchange`] surface. Each
//! [`ChatTurn`] carries a [`TurnRole`] and its text; the per-key window keeps the
//! most recent [`DEFAULT_MAX_TURNS`] messages, and the key SET itself is capped at
//! [`DEFAULT_MAX_KEYS`] distinct conversations, least-recently-used evicted (#2602)
//! — a local/authenticated-only caller couldn't mint enough distinct keys to
//! matter, but phase 4 fronts this endpoint with public channels (Telegram/Slack),
//! where every inbound chat could carry a fresh key and grow the map forever
//! without this cap.
//! Test: `record_and_history_round_trips`, `history_is_bounded`,
//! `distinct_keys_are_isolated`, `key_set_is_bounded_lru_eviction`,
//! `evicted_key_reads_as_empty_and_can_restart` in `chat_store_tests.rs`.

use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;

/// Maximum stored messages (user + assistant) retained per conversation.
///
/// Why: bounds memory for a long-lived key while keeping enough recent context
/// for a coherent multi-turn reply; 20 messages ≈ 10 exchanges, ample for a
/// portfolio Q&A loop.
/// What: the per-key cap enforced by [`ChatStore::record_exchange`].
/// Test: `history_is_bounded`.
pub const DEFAULT_MAX_TURNS: usize = 20;

/// Maximum number of distinct conversation keys retained at once.
///
/// Why: #2602 — the per-key turn cap bounds memory PER conversation, but never
/// bounded the KEY SET itself; any caller minting distinct keys (e.g. one per
/// inbound message from an unauthenticated public channel) could grow daemon
/// memory forever. 512 distinct live conversations is generously above any
/// expected phase-4 concurrent-channel load while keeping worst-case memory
/// small (512 keys × 20 turns × a short message ≈ low single-digit MB).
/// What: the capacity passed to the backing [`LruCache`]; the least-recently-used
/// key is evicted once a `record_exchange` for a NEW key would exceed it.
/// Test: `key_set_is_bounded_lru_eviction`.
pub const DEFAULT_MAX_KEYS: usize = 512;

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
/// existing conversation-keying convention. Bounding the key set itself (#2602)
/// with an LRU cache — not just the per-key turn window — keeps daemon memory
/// flat under public-channel-scale key churn once phase 4 lands.
/// What: a `Mutex`-guarded [`LruCache`] from conversation key to a capped
/// `Vec<ChatTurn>`. Cheap to read/write (a short vector clone); no async, no I/O.
/// Test: `record_and_history_round_trips`, `history_is_bounded`,
/// `distinct_keys_are_isolated`, `key_set_is_bounded_lru_eviction`,
/// `evicted_key_reads_as_empty_and_can_restart`.
#[derive(Debug)]
pub struct ChatStore {
    /// conversation key → recent turns (most-recent-last), capped at
    /// `max_turns`; the map itself holds at most `max_keys` entries, evicting
    /// least-recently-used keys on overflow.
    conversations: Mutex<LruCache<String, Vec<ChatTurn>>>,
    /// Per-conversation retention cap.
    max_turns: usize,
}

impl Default for ChatStore {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TURNS, DEFAULT_MAX_KEYS)
    }
}

impl ChatStore {
    /// Construct an empty store with the given per-conversation turn cap and
    /// distinct-key cap.
    ///
    /// Why: both caps are injectable so a test can prove either bound without
    /// pushing hundreds of turns/keys.
    /// What: an empty LRU-backed map with capacity `max_keys` (floored at 1, since
    /// [`LruCache`] requires a `NonZeroUsize`), plus the per-key turn retention
    /// cap (floored at 2 so a single exchange always fits).
    /// Test: `history_is_bounded`, `key_set_is_bounded_lru_eviction`.
    pub fn new(max_turns: usize, max_keys: usize) -> Self {
        let capacity = NonZeroUsize::new(max_keys.max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            conversations: Mutex::new(LruCache::new(capacity)),
            max_turns: max_turns.max(2),
        }
    }

    /// The recent turns for a conversation key, oldest-first.
    ///
    /// Why: the chat handler replays these as prior history before the new user
    /// message so the reply is contextual. Reading via [`LruCache::get`] also
    /// promotes `key` to most-recently-used, so an active conversation survives
    /// eviction as long as it keeps being read or written.
    /// What: clones the capped turn vector under `key`, or an empty vector if the
    /// key was never recorded OR has since been LRU-evicted.
    /// Test: `record_and_history_round_trips`, `evicted_key_reads_as_empty_and_can_restart`.
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
    /// the most recent turns; using [`LruCache::get_mut`]/[`LruCache::push`] keeps
    /// `key` promoted to most-recently-used so a live conversation is never the
    /// eviction candidate while it's still exchanging turns. Logging every
    /// eviction at debug level (#2602 review) gives an observable signal that the
    /// key-set cap is actually being hit in production, without needing a new
    /// metrics seam for what should be a rare event under normal load.
    /// What: pushes the user then assistant turn under `key` (inserting a fresh
    /// entry — and possibly evicting the least-recently-used OTHER key — if `key`
    /// is new), then truncates the front so at most `max_turns` remain. Returns
    /// the resulting turn count.
    ///
    /// Trade-off: `lru` 0.12.5's `get_or_insert_mut` would collapse the
    /// check-then-insert below into one lookup, but it doesn't surface the
    /// evicted entry on overflow — and the eviction log needs exactly that entry.
    /// Eviction observability wins over shaving one lookup, so this stays the
    /// explicit two-step `get_mut` + `push` form.
    /// Test: `record_and_history_round_trips`, `history_is_bounded`,
    /// `key_set_is_bounded_lru_eviction`.
    pub fn record_exchange(&self, key: &str, user: &str, assistant: &str) -> usize {
        let mut map = self.conversations.lock().unwrap_or_else(|p| p.into_inner());
        if map.get_mut(key).is_none() {
            // `push` returns `Some((old_key, old_value))` both when it evicts the
            // least-recently-used entry to make room, AND when `key` itself
            // already existed (a same-key value replace). We only reach this
            // branch after `get_mut` just confirmed `key` is ABSENT, so any
            // `Some` here is necessarily a DIFFERENT key that the LRU cap
            // evicted — the `evicted_key != key` guard documents that invariant
            // defensively rather than relying on it silently.
            if let Some((evicted_key, _evicted_turns)) = map.push(key.to_string(), Vec::new())
                && evicted_key != key
            {
                tracing::debug!(
                    evicted_key = %evicted_key,
                    "chat_store: LRU-evicted conversation key to stay within the key-set cap"
                );
            }
        }
        let Some(turns) = map.get_mut(key) else {
            return 0;
        };
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
