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
/// Why: a coordinator message resolves to either a routed command, an LLM
/// answer, or — on the action-capable path (`actions: true`, #1283) — an LLM
/// answer plus an audit trail of the managed verbs the session-manager invoked
/// inline; the caller renders all three from this one shape.
/// What: the `reply` text; `routed_to_session` and `command_output` are
/// populated only when the message was routed to a session by `@prefix:`;
/// `actions_taken` lists the verbs executed this turn (only when `actions: true`
/// routed the SM branch and at least one verb ran), and `conv_id` echoes the SM
/// conversation id so a follow-up turn can continue the same rolling context.
/// Test: `coordinator_chat_outcome_deserializes`,
/// `coordinator_chat_outcome_deserializes_actions`.
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
    /// The managed verbs the SM invoked inline this turn, in order, for the
    /// operator's audit trail. Absent (`None`) on the text-only, legacy, and
    /// prefix paths and when no verb ran; `Some([...])` only when at least one
    /// verb executed on the `actions: true` path.
    #[serde(default)]
    pub actions_taken: Option<Vec<String>>,
    /// The SM conversation id this turn used, echoed so a follow-up can continue
    /// the same rolling context. Present only on the SM (action-capable) path.
    #[serde(default)]
    pub conv_id: Option<String>,
}

// ── Managed session-manager wire types (`/api/v1/sessions/managed/*`) ───────────
//
// These mirror the daemon-side DTOs in `crate::daemon::managed_routes` field-for-
// field so serde round-trips across the HTTP boundary. The struct names carry a
// `Managed` prefix to avoid colliding with the existing `client::result::
// SessionSummary` re-export; serde keys (not Rust type names) are what the wire
// contract pins, so the prefix is cosmetic.

/// One managed session as returned by the list/get endpoints.
///
/// Why: every managed-session UI (the `tm` CLI today, STUI #1272 and TELUI #1433
/// next) renders the same flat, string-typed summary; a shared client type keeps
/// them off the daemon's internal record shape and off hand-rolled structs.
/// What: mirrors `daemon::managed_routes::SessionSummary` field-for-field.
/// Test: `managed_session_summary_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedSessionSummary {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name.
    pub name: String,
    /// Lifecycle state word.
    pub state: String,
    /// Provisioned workspace path, if any.
    #[serde(default)]
    pub workspace_path: Option<String>,
    /// Repository URL the session was provisioned from.
    #[serde(default)]
    pub repo_url: Option<String>,
    /// Git branch or ref checked out.
    #[serde(default)]
    pub branch: Option<String>,
    /// Creation timestamp (RFC 3339), `None` when the daemon omits it.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Last-activity timestamp (RFC 3339), if any.
    #[serde(default)]
    pub last_activity_at: Option<String>,
    /// A pending decision question, if surfaced.
    #[serde(default)]
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    #[serde(default)]
    pub proposed_default: Option<String>,
}

/// Wrapper for `GET /api/v1/sessions/managed` (the list endpoint).
///
/// Why: the list endpoint nests the sessions under a `sessions` key; a typed
/// wrapper deserializes it without an ad-hoc local struct at the call site.
/// What: mirrors `daemon::managed_routes::ListSessionsResponse`.
/// Test: `managed_list_response_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedListResponse {
    /// All managed sessions as summaries.
    #[serde(default)]
    pub sessions: Vec<ManagedSessionSummary>,
}

/// Request body for `POST /api/v1/sessions/managed` (spawn).
///
/// Why: spawning a managed session requires the repo, ref, and task; an optional
/// name hint and runtime selector tune the tmux name and backend.
/// What: mirrors `daemon::managed_routes::SpawnRequest`; `git_ref` serializes as
/// `ref` to match the daemon's `#[serde(rename = "ref")]`.
/// Test: `managed_spawn_request_serializes`.
#[derive(Debug, Clone, Serialize)]
pub struct ManagedSpawnRequest {
    /// Repository URL to provision the session workspace from.
    pub repo_url: String,
    /// Git branch or ref to check out (wire key `ref`).
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Human-readable task description for the session.
    pub task: String,
    /// Optional name hint overriding the auto-generated tmux session name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_hint: Option<String>,
    /// Optional runtime selector (`"claude-code"` | `"tcode"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// Response body for `POST /api/v1/sessions/managed` (spawn).
///
/// Why: the caller needs the new session's identity, state, runtime, and attach
/// command immediately after spawn.
/// What: mirrors `daemon::managed_routes::SpawnResponse`.
/// Test: `managed_spawn_response_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedSpawnResponse {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name.
    pub name: String,
    /// Provisioned workspace path, if any.
    #[serde(default)]
    pub workspace_path: Option<String>,
    /// Repository URL the session was provisioned from.
    #[serde(default)]
    pub repo_url: Option<String>,
    /// Git branch or ref checked out.
    #[serde(default)]
    pub branch: Option<String>,
    /// Current lifecycle state.
    pub state: String,
    /// Creation timestamp (RFC 3339), `None` when the daemon omits it.
    #[serde(default)]
    pub created_at: Option<String>,
    /// tmux attach command string.
    #[serde(default)]
    pub attach_cmd: String,
    /// Runtime backend hosting the session (`"claude-code"` | `"tcode"`).
    #[serde(default)]
    pub runtime: String,
}

/// Request body for `POST /api/v1/sessions/managed/adopt` (#1433).
///
/// Why: adopting an EXISTING tmux session connects the managed surface to a pane
/// the operator already has. The pane's provenance is unknown to the daemon, so
/// `cwd` is REQUIRED; `task`/`runtime` are optional.
/// What: mirrors `daemon::managed_routes::AdoptExistingRequest` field-for-field.
/// Test: `managed_adopt_request_serializes` in `tests.rs`.
#[derive(Debug, Clone, Serialize)]
pub struct ManagedAdoptRequest {
    /// The live tmux session name to adopt (any name; need not be `tmpm-`).
    pub tmux_name: String,
    /// Working directory the adopted session runs in (required).
    pub cwd: String,
    /// Optional human-readable task description (empty/absent allowed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Optional runtime selector (`"claude-code"` | `"tcode"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// Response body for `POST /api/v1/sessions/managed/adopt` (201 Created, #1433).
///
/// Why: the caller needs the new managed record's identity, state, runtime, and
/// attach command immediately after adoption.
/// What: mirrors `daemon::managed_routes::AdoptExistingResponse`.
/// Test: `managed_adopt_response_deserializes` in `tests.rs`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedAdoptResponse {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name that was adopted.
    pub name: String,
    /// Current lifecycle state (`active` immediately after adoption).
    pub state: String,
    /// Working directory the adopted session runs in.
    #[serde(default)]
    pub cwd: String,
    /// Runtime backend hosting the session (`"claude-code"` | `"tcode"`).
    #[serde(default)]
    pub runtime: String,
    /// tmux attach command string.
    #[serde(default)]
    pub attach_cmd: String,
}

/// Request body for `POST /api/v1/sessions/managed/{id}/send`.
///
/// Why: inject operator/agent text into a session's tmux pane.
/// What: mirrors `daemon::managed_routes::SendInputRequest`.
/// Test: `managed_send_request_serializes`.
#[derive(Debug, Clone, Serialize)]
pub struct ManagedSendInputRequest {
    /// Text to inject into the session's tmux pane.
    pub text: String,
}

/// Response body for `POST /api/v1/sessions/managed/{id}/send`.
///
/// Why: confirm the inject succeeded without echoing the full record.
/// What: mirrors `daemon::managed_routes::SendInputResponse`.
/// Test: `managed_send_response_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedSendInputResponse {
    /// True when the text was injected.
    pub sent: bool,
    /// tmux session name the text was sent to.
    #[serde(default)]
    pub tmux_name: String,
}

/// Request body for `POST /api/v1/sessions/managed/{id}/answer`.
///
/// Why: inject an answer to a pending decision the harness is blocked on.
/// What: mirrors `daemon::managed_routes::AnswerRequest`.
/// Test: `managed_answer_request_serializes`.
#[derive(Debug, Clone, Serialize)]
pub struct ManagedAnswerRequest {
    /// The answer text to inject for the pending decision.
    pub answer: String,
}

/// Response body for `POST /api/v1/sessions/managed/{id}/answer`.
///
/// Why: confirm the answer was injected.
/// What: mirrors `daemon::managed_routes::AnswerResponse`.
/// Test: `managed_answer_response_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedAnswerResponse {
    /// True when the answer was injected.
    pub injected: bool,
    /// tmux session name the answer was sent to.
    #[serde(default)]
    pub tmux_name: String,
}

/// Response body for `GET /api/v1/sessions/managed/{id}/attach-cmd`.
///
/// Why: the operator needs the exact tmux command to attach to the pane.
/// What: mirrors `daemon::managed_routes::AttachCmdResponse`.
/// Test: `managed_attach_cmd_response_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedAttachCmdResponse {
    /// tmux attach command string.
    pub attach_cmd: String,
}

/// Response body for `GET /api/v1/sessions/managed/{id}/activity`.
///
/// Why: surface a session's full activity picture without attaching and without
/// requiring an LLM key — the raw pane and structured lifecycle fields are always
/// present; the LLM overlay is populated only when a classifier ran.
/// What: mirrors `daemon::managed_routes::ActivityResponse` field-for-field.
/// Test: `managed_activity_response_deserializes`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedActivityResponse {
    /// Raw pane content (last 60 lines). Always present.
    pub raw_pane: String,
    /// Whether the tmux runtime session is currently alive.
    pub runtime_active: bool,
    /// Activity state from LLM classification, or `"unknown"`.
    pub state: String,
    /// Human-readable summary of what the session is doing.
    pub summary: String,
    /// Confidence of the classification (0.0–1.0); 0.0 when no classifier ran.
    pub confidence: f32,
    /// True when the verdict was served from the content-hash cache.
    pub cache_hit: bool,
    /// Input token count for this check (0 on cache hit or no classifier).
    pub input_tokens: u32,
    /// Output token count for this check (0 on cache hit or no classifier).
    pub output_tokens: u32,
    /// Latency in milliseconds for this check.
    pub latency_ms: u64,
    /// Cumulative input tokens across all checks for this session.
    pub total_input_tokens: u64,
    /// Cumulative output tokens across all checks for this session.
    pub total_output_tokens: u64,
    /// LLM classification result, `None` when no classifier ran.
    #[serde(default)]
    pub classification: Option<String>,
    /// A pending decision question, if surfaced.
    #[serde(default)]
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    #[serde(default)]
    pub proposed_default: Option<String>,
}

/// The daemon's `GET /health` snapshot.
///
/// Why: the `health` verb reports the daemon's liveness word and the
/// catalog-freshness flags (HR-3 / DOC-17) in one typed shape, rather than the
/// bare boolean [`super::DaemonClient::is_healthy`] returns. Mirroring the
/// daemon's `HealthResponse` field names keeps the wire contract type-checked.
/// What: `status` is the liveness word (`"ok"` while up); `catalog_stale` is true
/// when deployed agents/skills drift from the synced catalog; `catalog_unknown`
/// is true when the catalog has never been synced. All non-`status` fields
/// default so an older daemon that returns only `status` still parses.
/// Test: `health_snapshot_deserializes` in `tests.rs`.
#[derive(Debug, Clone, Deserialize)]
pub struct HealthSnapshot {
    /// Liveness word — `"ok"` while the daemon is up.
    pub status: String,
    /// True when deployed content drifts from the synced catalog (HR-3).
    #[serde(default)]
    pub catalog_stale: bool,
    /// True when the catalog has never been synced (nothing to compare).
    #[serde(default)]
    pub catalog_unknown: bool,
}
