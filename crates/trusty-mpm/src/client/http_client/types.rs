//! Wire types for the daemon HTTP API.
//!
//! Why: keeping the serde structs in a dedicated file lets the client impl stay
//! focused on HTTP logic and keeps each file under the 500-SLOC cap.
//! What: all request/response structs deserialized from the daemon's JSON API.
//! Test: `session_row_deserializes_tmux_name` and the other struct tests in
//! `tests.rs` exercise these shapes.

use serde::{Deserialize, Serialize};

use crate::core::session::{SessionId, SessionStatus};

/// One session row as returned by `GET /sessions`.
///
/// Why: the UIs render sessions and resolve action targets from this shape.
/// What: mirrors the daemon's `Session` serde output, keeping only the fields
/// every UI consumes.
/// Test: `session_row_deserializes_tmux_name`.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionRow {
    /// Session id (UUID), serialized by the daemon as a bare string.
    pub id: SessionId,
    /// Working directory.
    pub workdir: String,
    /// Lifecycle status.
    pub status: SessionStatus,
    /// Number of active delegations.
    #[serde(default)]
    pub active_delegations: u32,
    /// Friendly tmux session name (`tmpm-<adjective>-<noun>`).
    ///
    /// Why: session action endpoints resolve their `{id}` path segment against
    /// this friendly name; the UIs use it as the action target rather than the
    /// raw UUID.
    /// Test: `session_row_deserializes_tmux_name`.
    #[serde(default)]
    pub tmux_name: String,
    /// Last-seen timestamp from the daemon, serialized as
    /// `{"secs_since_epoch": u64, "nanos_since_epoch": u32}`.
    ///
    /// Why: recency tie-breaking for `connect` workdir-prefix resolution.
    /// What: deserialized from the daemon's `SystemTime` serde output; defaults
    /// to `{"secs_since_epoch":0}` when absent.
    #[serde(default)]
    pub last_seen: LastSeen,
}

/// Serde shape for `SystemTime` as emitted by the daemon.
///
/// Why: `serde` serializes `SystemTime` as a struct, not a plain integer; only
/// the seconds component is needed for recency comparison.
/// What: a single `secs_since_epoch` field, defaulting to zero.
/// Test: covered by `session_row_deserializes_tmux_name`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LastSeen {
    /// Whole seconds since the Unix epoch.
    #[serde(default)]
    pub secs_since_epoch: u64,
}

/// One hook-event row as returned by `GET /events`.
///
/// Why: the dashboard's event panel renders the daemon's live hook feed.
/// What: mirrors the serde output of `HookEventRecord`.
/// Test: `events_deserialize_from_record_shape`.
#[derive(Debug, Clone, Deserialize)]
pub struct EventRow {
    /// Originating session id (UUID, serialized by the daemon as a bare string).
    pub session: SessionId,
    /// Claude Code hook event (e.g. `PreToolUse`).
    pub event: crate::core::hook::HookEvent,
    /// RFC3339 timestamp the daemon received the event.
    pub at: String,
    /// Opaque event payload; defaults to `Null` when the daemon omits it.
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// One circuit-breaker row as returned by `GET /breakers`.
///
/// Why: the dashboard's breaker panel shows which agents have tripped.
/// What: the agent name plus the flattened breaker state and failure count.
/// Test: `breakers_deserialize_from_api_shape`.
#[derive(Debug, Clone, Deserialize)]
pub struct BreakerRow {
    /// Agent name the breaker guards.
    pub agent: String,
    /// Breaker state: `closed` / `open` / `half_open`.
    pub state: String,
    /// Consecutive failures observed since the last success.
    pub consecutive_failures: u32,
}

/// One tmux session row as returned by `GET /tmux/sessions`.
///
/// Why: the Telegram `/tmux` command lists every tmux session on the host and
/// offers an "Adopt" button for the ones trusty-mpm does not yet manage.
/// What: the session name plus whether trusty-mpm manages it.
/// Test: `tmux_session_row_accepts_name`.
#[derive(Debug, Clone)]
pub struct TmuxSessionRow {
    /// tmux session name.
    pub name: String,
    /// True when the session's origin is `trusty_mpm` (already managed).
    pub managed: bool,
}

/// One discovered Claude Code project as returned by `GET /projects/discover`.
///
/// Why: the Telegram `/projects` command lists projects mined from
/// `~/.claude/projects/` for one-tap registration.
/// What: the absolute project path, its recorded session count, and the
/// ISO-8601 last-session time when present.
/// Test: covered by the executor's projects test.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveredProjectRow {
    /// Absolute project path.
    pub path: String,
    /// Number of recorded Claude Code sessions for the project.
    #[serde(default)]
    pub session_count: usize,
    /// ISO-8601 last-session timestamp, or `None` when the project has none.
    #[serde(default)]
    pub last_session: Option<String>,
}

/// One Claude Code config recommendation from `GET /claude-config`.
///
/// Why: the `/config` command surfaces analyzer recommendations to the operator.
/// What: the recommendation id and its human-readable message.
/// Test: covered by the executor's config tests.
#[derive(Debug, Clone)]
pub struct ConfigRecommendation {
    /// Stable recommendation id (used to apply it).
    pub id: String,
    /// Human-readable description of the recommendation.
    pub message: String,
}

/// Overseer status as returned by `GET /overseer`.
///
/// Why: the `/overseer` command reports whether oversight is active.
/// What: the enabled flag, the handler name, and the recent decision counts.
/// Test: covered by the executor's overseer test.
#[derive(Debug, Clone)]
pub struct OverseerSnapshot {
    /// Whether the overseer is enabled.
    pub enabled: bool,
    /// Active overseer strategy name.
    pub handler: String,
    /// Recent allow / block / flag decision counts.
    pub decisions: (u64, u64, u64),
}

/// Response body of `POST /pair/request`.
///
/// Why: `tm pair` shows the code and its TTL to the operator.
/// What: the generated pairing code and its lifetime in seconds.
/// Test: covered by the executor's pairing test.
#[derive(Debug, Clone, Deserialize)]
pub struct PairRequest {
    /// One-time pairing code (six uppercase alphanumeric characters).
    pub code: String,
    /// Seconds until the code expires.
    #[serde(default)]
    pub expires_in_seconds: u64,
}

/// Response body of `POST /pair/confirm`.
///
/// Why: the bot's `/pair` flow reports success or the failure reason.
/// What: the success flag, the registered chat id, and an optional error.
/// Test: covered by the executor's pairing test.
#[derive(Debug, Clone, Deserialize)]
pub struct PairConfirm {
    /// Whether the code was valid and the chat is now paired.
    pub success: bool,
    /// The chat id that was registered, when `success` is true.
    #[serde(default)]
    pub chat_id: Option<i64>,
    /// Failure reason, when `success` is false.
    #[serde(default)]
    pub error: Option<String>,
}

/// Response body of `GET /pair/status`.
///
/// Why: the `/start` command branches on whether the daemon is already paired.
/// What: the paired flag and the registered chat id when present.
/// Test: covered by the executor's pairing test.
#[derive(Debug, Clone, Deserialize)]
pub struct PairStatus {
    /// Whether a chat is currently paired with the daemon.
    pub paired: bool,
    /// The paired chat id, when `paired` is true.
    #[serde(default)]
    pub chat_id: Option<i64>,
}

/// One message in an LLM chat conversation.
///
/// Why: the `/chat` command (TUI) and free-text Telegram messages route to the
/// daemon's `POST /llm/chat`, which keeps no chat state of its own — the UI
/// holds the rolling history and sends it with each turn.
/// What: a `role` (`"user"` or `"assistant"`) and the message `content`,
/// wire-compatible with the daemon's `ChatMessage`.
/// Test: `llm_chat_message_round_trips`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Message role: `"user"` or `"assistant"`.
    pub role: String,
    /// Message text content.
    pub content: String,
}

impl ChatMessage {
    /// A user-authored chat message.
    ///
    /// Why: UIs threading a rolling conversation window need to append the
    /// operator's turn; a named constructor keeps `role` strings out of call
    /// sites.
    /// What: builds a `ChatMessage` with `role = "user"`.
    /// Test: `chat_message_constructors_set_role`.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    /// An assistant-authored chat message.
    ///
    /// Why: the counterpart to [`Self::user`] for appending the reply turn.
    /// What: builds a `ChatMessage` with `role = "assistant"`.
    /// Test: `chat_message_constructors_set_role`.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

/// Outcome of a `POST /llm/chat` call.
///
/// Why: the caller needs both the assistant's reply and the updated history so
/// it can persist the conversation window for the next turn.
/// What: the assistant `reply` text and the updated `history`.
/// Test: `llm_chat_response_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmChatOutcome {
    /// The assistant's reply text.
    pub reply: String,
    /// The updated conversation history, ready for the next turn.
    #[serde(default)]
    pub history: Vec<ChatMessage>,
}

/// One session row inside a [`CoordinatorContext`].
///
/// Why: the TUI/GUI coordinator sidebar renders each session's name, status,
/// and a recent-output excerpt; this mirrors the daemon's `SessionSummary`.
/// What: identity fields plus the captured tail of the session's tmux pane.
/// Test: `coordinator_context_deserializes`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CoordinatorSession {
    /// Session id (UUID string).
    pub id: String,
    /// tmux session name, e.g. `tmpm-aipowerranking`.
    pub name: String,
    /// Short routing prefix, e.g. `aipowerranking`.
    pub prefix: String,
    /// Working directory the session runs in.
    pub workdir: String,
    /// Lifecycle status word: `Active` / `Paused` / `Stopped` / ….
    pub status: String,
    /// Number of active delegations the session has running.
    #[serde(default)]
    pub active_delegations: u32,
    /// Recent lines captured from the session's pane.
    #[serde(default)]
    pub recent_output: Vec<String>,
    /// The latest daemon-cached LLM summary for this session, if one exists.
    ///
    /// Why: the sessions TUI renders a per-session summary bullet (DOC-16 §4.3)
    /// from the daemon's cached summary (#1275). `#[serde(default)]` keeps the
    /// client tolerant of an OLDER daemon that omits the field — it deserializes
    /// to `None` rather than failing.
    /// What: an optional single-line summary string.
    /// Test: `coordinator_session_tolerates_missing_summary_fields`.
    #[serde(default)]
    pub last_summary: Option<String>,
    /// Whether an inference call for this session is currently in flight.
    ///
    /// Why: the TUI blinks the bullet while a session is actively summarizing
    /// (DOC-16 §3.3, D1). `#[serde(default)]` defaults this to `false` against an
    /// older daemon that does not emit it (never blink — §3.3 error condition).
    /// What: a boolean in-flight flag.
    /// Test: `coordinator_session_tolerates_missing_summary_fields`.
    #[serde(default)]
    pub summarizing: bool,
}

/// Snapshot returned by `GET /api/v1/sessions/context`.
///
/// Why: the coordinator UI displays the per-session summaries that the daemon's
/// coordinator reasons over; this is the deserialized view of that snapshot.
/// What: the per-session summaries (the `recent_events` field is intentionally
/// ignored — the UIs only need the session list).
/// Test: `coordinator_context_deserializes`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CoordinatorContext {
    /// Per-session activity summaries.
    #[serde(default)]
    pub sessions: Vec<CoordinatorSession>,
}

/// Outcome of a `POST /api/v1/sessions/chat` call.
///
/// Why: a coordinator message resolves to either a routed command or an LLM
/// answer; the caller renders both from this one shape.
/// What: the `reply` text; `routed_to_session` and `command_output` are
/// populated only when the message was routed to a session by `@prefix:`.
/// Test: `coordinator_chat_outcome_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct CoordinatorChatOutcome {
    /// The assistant reply, or a note about the routed command.
    pub reply: String,
    /// tmux name of the session a prefixed message was routed to, if any.
    #[serde(default)]
    pub routed_to_session: Option<String>,
    /// Captured pane output from a routed command, if any.
    #[serde(default)]
    pub command_output: Option<String>,
}
