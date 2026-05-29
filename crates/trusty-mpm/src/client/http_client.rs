//! Unified daemon HTTP client.
//!
//! Why: every trusty-mpm UI (TUI, Telegram bot, CLI) is a separate process from
//! the daemon and must reach it over HTTP. Before this crate the transport was
//! reimplemented per UI; [`DaemonClient`] is the single shared wrapper so a new
//! endpoint is wired exactly once.
//! What: [`DaemonClient`] holds a base URL plus a shared `reqwest::Client` and
//! exposes one async method per daemon endpoint the UIs need — session listing
//! and lifecycle, the event feed, breaker state, the overseer / tmux / config
//! analyzer views, and the pairing handshake.
//! Test: `cargo test -p trusty-mpm-client` checks URL construction and wire-shape
//! deserialization; live HTTP is exercised by the executor tests against an
//! in-process test daemon and by the daemon's own API tests.

use serde::{Deserialize, Serialize};

use crate::core::session::{SessionId, SessionStatus};

/// HTTP client for one trusty-mpm daemon.
///
/// Why: a thin wrapper so any UI can be pointed at any daemon address.
/// What: holds the base URL and a shared `reqwest::Client`.
/// Test: `base_url_is_stored`.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    /// Base URL of the daemon, e.g. `http://127.0.0.1:7880`.
    base: String,
    /// Shared connection-pooling HTTP client.
    http: reqwest::Client,
}

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
/// What: the session name plus whether trusty-mpm manages it. The daemon's
/// payload may be a plain string or an origin-tagged object; both are accepted,
/// with a plain string treated as external (`managed = false`).
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
}

/// Snapshot returned by `GET /api/v1/coordinator/context`.
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

/// Outcome of a `POST /api/v1/coordinator/chat` call.
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

impl DaemonClient {
    /// Build a client targeting `base` (e.g. `http://127.0.0.1:7880`).
    ///
    /// Why: a UI is pointed at a daemon address resolved from a flag or the
    /// service lock file.
    /// What: stores the base URL and a fresh pooled `reqwest::Client`.
    /// Test: `base_url_is_stored`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            http: reqwest::Client::new(),
        }
    }

    /// The base URL this client targets.
    ///
    /// Why: tests and diagnostics need to read back the configured address.
    /// What: returns the stored base URL string.
    /// Test: `base_url_is_stored`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Re-point this client at a new daemon base URL.
    ///
    /// Why: the daemon may bind a fresh ephemeral port across a restart, so a
    /// long-lived UI (the TUI) must be able to follow it to the address recorded
    /// in the lock file instead of being stuck on a stale URL and reporting
    /// "daemon unreachable" forever. The pooled `reqwest::Client` is kept; only
    /// the target address changes.
    /// What: overwrites [`Self::base`] with `base`.
    /// Test: `set_base_url_repoints_client`.
    pub fn set_base_url(&mut self, base: impl Into<String>) {
        self.base = base.into();
    }

    /// Fetch the current session list from the daemon.
    ///
    /// Why: every UI's session view refreshes from this.
    /// What: `GET /sessions`, returns the `sessions` array deserialized.
    /// Test: covered by the daemon API tests and the executor tests.
    pub async fn sessions(&self) -> anyhow::Result<Vec<SessionRow>> {
        #[derive(Deserialize)]
        struct Body {
            sessions: Vec<SessionRow>,
        }
        let url = format!("{}/sessions", self.base);
        let body: Body = self.http.get(&url).send().await?.json().await?;
        Ok(body.sessions)
    }

    /// Fetch the recent hook-event feed from the daemon.
    ///
    /// Why: the dashboard's event panel refreshes from this. The push-based
    /// SSE feed lives at `GET /events`; this method polls the legacy snapshot
    /// at `GET /events/poll` for callers that don't stream.
    /// What: `GET /events/poll`, returns the `events` array deserialized.
    /// Test: `events_deserialize_from_record_shape` covers the wire shape.
    pub async fn events(&self) -> anyhow::Result<Vec<EventRow>> {
        #[derive(Deserialize)]
        struct Body {
            events: Vec<EventRow>,
        }
        let url = format!("{}/events/poll", self.base);
        let body: Body = self.http.get(&url).send().await?.json().await?;
        Ok(body.events)
    }

    /// Fetch one session's recent hook events.
    ///
    /// Why: the `/status` command shows a session's last events. The
    /// push-based SSE feed lives at `GET /sessions/{id}/events`; this method
    /// polls the legacy snapshot at `GET /sessions/{id}/events/poll`.
    /// What: `GET /sessions/{id}/events/poll`, returns the `events` array.
    /// Test: covered by the executor's status test.
    pub async fn session_events(&self, id: &str) -> anyhow::Result<Vec<EventRow>> {
        #[derive(Deserialize)]
        struct Body {
            events: Vec<EventRow>,
        }
        let url = format!("{}/sessions/{id}/events/poll", self.base);
        let body: Body = self.http.get(&url).send().await?.json().await?;
        Ok(body.events)
    }

    /// Fetch every agent's circuit-breaker state from the daemon.
    ///
    /// Why: the dashboard's breaker panel needs the latest breaker snapshot.
    /// What: `GET /breakers`, flattening the nested `breaker` object into a
    /// flat [`BreakerRow`] per agent.
    /// Test: `breakers_deserialize_from_api_shape` covers the wire shape.
    pub async fn breakers(&self) -> anyhow::Result<Vec<BreakerRow>> {
        #[derive(Deserialize)]
        struct WireBreaker {
            state: String,
            consecutive_failures: u32,
        }
        #[derive(Deserialize)]
        struct WireRow {
            agent: String,
            breaker: WireBreaker,
        }
        #[derive(Deserialize)]
        struct Body {
            breakers: Vec<WireRow>,
        }
        let url = format!("{}/breakers", self.base);
        let body: Body = self.http.get(&url).send().await?.json().await?;
        Ok(body
            .breakers
            .into_iter()
            .map(|r| BreakerRow {
                agent: r.agent,
                state: r.breaker.state,
                consecutive_failures: r.breaker.consecutive_failures,
            })
            .collect())
    }

    /// Probe whether the daemon is reachable.
    ///
    /// Why: the TUI greys out its panels when the daemon is down.
    /// What: `GET /health`, true on any 2xx response.
    /// Test: covered by the daemon API tests.
    pub async fn is_healthy(&self) -> bool {
        let url = format!("{}/health", self.base);
        matches!(self.http.get(&url).send().await, Ok(r) if r.status().is_success())
    }

    /// Pause a session via `POST /sessions/{id}/pause`.
    ///
    /// Why: the dashboard's `p` key pauses the selected session in place.
    /// What: POSTs `{"summary": null}` and returns the `summary` field.
    /// Test: live HTTP is covered by the daemon's session-lifecycle tests.
    pub async fn pause_session(&self, id: &str) -> anyhow::Result<String> {
        let url = format!("{}/sessions/{id}/pause", self.base);
        let body: serde_json::Value = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "summary": serde_json::Value::Null }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Resume a session via `POST /sessions/{id}/resume`.
    ///
    /// Why: the dashboard's `r` key resumes the selected paused session.
    /// What: POSTs to the resume endpoint and discards the response body.
    /// Test: live HTTP is covered by the daemon's session-lifecycle tests.
    pub async fn resume_session(&self, id: &str) -> anyhow::Result<()> {
        let url = format!("{}/sessions/{id}/resume", self.base);
        self.http.post(&url).send().await?.error_for_status()?;
        Ok(())
    }

    /// Stop a session via `DELETE /sessions/{id}`.
    ///
    /// Why: the dashboard's `x` key and the `/kill` command stop a session.
    /// What: sends a DELETE to the session endpoint; returns `Ok(true)` when the
    /// session existed, `Ok(false)` on a 404, `Err` on transport failure.
    /// Test: covered by the executor's kill test.
    pub async fn kill_session(&self, id: &str) -> anyhow::Result<bool> {
        let url = format!("{}/sessions/{id}", self.base);
        let resp = self.http.delete(&url).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        resp.error_for_status()?;
        Ok(true)
    }

    /// Stop a session, discarding the found/missing distinction.
    ///
    /// Why: the TUI's `x` key only needs success-or-error feedback.
    /// What: calls [`Self::kill_session`] and maps the result to `()`.
    /// Test: covered by the executor's kill test.
    pub async fn stop_session(&self, id: &str) -> anyhow::Result<()> {
        self.kill_session(id).await.map(|_| ())
    }

    /// Capture recent session output via `GET /sessions/{id}/output`.
    ///
    /// Why: the dashboard's `o` key snapshots the selected session's pane.
    /// What: `GET /sessions/{id}/output?lines={lines}`, returns the `output`
    /// field from the 200 response.
    /// Test: live HTTP is covered by the daemon's session-lifecycle tests.
    pub async fn session_output(&self, id: &str, lines: u32) -> anyhow::Result<String> {
        let url = format!("{}/sessions/{id}/output", self.base);
        let body: serde_json::Value = self
            .http
            .get(&url)
            .query(&[("lines", lines.to_string())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body
            .get("output")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Send a command into a session's tmux pane via `POST /sessions/{id}/command`.
    ///
    /// Why: the Telegram `/send` command and the TUI's `/send` drive a running
    /// Claude Code session remotely — type a prompt, read back the pane.
    /// What: POSTs `{ command }`; returns `Ok(Some(output))` with the captured
    /// pane text on success, `Ok(None)` when the session is unknown (`404`), and
    /// `Err` on transport failure.
    /// Test: covered by the daemon's session-command tests.
    pub async fn send_session_command(
        &self,
        id: &str,
        command: &str,
    ) -> anyhow::Result<Option<String>> {
        let url = format!("{}/sessions/{id}/command", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "command": command }))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body: serde_json::Value = resp.error_for_status()?.json().await?;
        Ok(Some(
            body.get("output")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ))
    }

    /// Fetch the overseer status via `GET /overseer`.
    ///
    /// Why: the `/overseer` command reports oversight status.
    /// What: returns the enabled flag, handler name, and decision counts.
    /// Test: covered by the executor's overseer test.
    pub async fn overseer_status(&self) -> anyhow::Result<OverseerSnapshot> {
        let url = format!("{}/overseer", self.base);
        let body: serde_json::Value = self.http.get(&url).send().await?.json().await?;
        let o = &body["overseer"];
        let decisions = &o["decisions"];
        Ok(OverseerSnapshot {
            enabled: o["enabled"].as_bool().unwrap_or(false),
            handler: o["handler"].as_str().unwrap_or("?").to_string(),
            decisions: (
                decisions["allow"].as_u64().unwrap_or(0),
                decisions["block"].as_u64().unwrap_or(0),
                decisions["flag"].as_u64().unwrap_or(0),
            ),
        })
    }

    /// List every tmux session on the daemon host via `GET /tmux/sessions`.
    ///
    /// Why: the `/tmux` command lists internal and external tmux sessions and
    /// flags which are already managed so it can offer to adopt the rest.
    /// What: returns one [`TmuxSessionRow`] per session; the daemon payload may
    /// be plain strings or origin-tagged objects, both of which are accepted. A
    /// session is `managed` when its `origin` field is `trusty_mpm`.
    /// Test: `tmux_session_row_accepts_name`.
    pub async fn tmux_sessions(&self) -> anyhow::Result<Vec<TmuxSessionRow>> {
        let url = format!("{}/tmux/sessions", self.base);
        let body: serde_json::Value = self.http.get(&url).send().await?.json().await?;
        let sessions = body["sessions"].as_array().cloned().unwrap_or_default();
        Ok(sessions
            .iter()
            .filter_map(|s| {
                let name = s
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| s.as_str())?;
                let managed = s.get("origin").and_then(|v| v.as_str()) == Some("trusty_mpm");
                Some(TmuxSessionRow {
                    name: name.to_string(),
                    managed,
                })
            })
            .collect())
    }

    /// Discover Claude Code projects via `GET /projects/discover`.
    ///
    /// Why: the `/projects` command lists projects mined from
    /// `~/.claude/projects/` so an operator can register one without typing a
    /// path.
    /// What: `GET /projects/discover`, returns the `projects` array deserialized
    /// into [`DiscoveredProjectRow`]s.
    /// Test: covered by the executor's projects test.
    pub async fn discover_projects(&self) -> anyhow::Result<Vec<DiscoveredProjectRow>> {
        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            projects: Vec<DiscoveredProjectRow>,
        }
        let url = format!("{}/projects/discover", self.base);
        let body: Body = self.http.get(&url).send().await?.json().await?;
        Ok(body.projects)
    }

    /// Register a project via `POST /projects`.
    ///
    /// Why: the `/projects` keyboard's "Set Active" button registers a
    /// discovered project with the daemon.
    /// What: POSTs `{"path": <path>}`; returns `Ok(())` on a 2xx response.
    /// Test: covered by the executor's projects test.
    pub async fn register_project(&self, path: &str) -> anyhow::Result<()> {
        let url = format!("{}/projects", self.base);
        self.http
            .post(&url)
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Capture a tmux pane snapshot via `GET /tmux/sessions/{name}/snapshot`.
    ///
    /// Why: the `/snapshot` command shows a tmux pane's recent output.
    /// What: returns the snapshot text, or `Ok(None)` when the session is
    /// unknown / tmux is unavailable (the daemon answers 404).
    /// Test: covered by the daemon's tmux tests.
    pub async fn snapshot_tmux_session(&self, name: &str) -> anyhow::Result<Option<String>> {
        let url = format!("{}/tmux/sessions/{name}/snapshot", self.base);
        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let body: serde_json::Value = resp.json().await?;
        Ok(Some(snapshot_text(&body["snapshot"])))
    }

    /// Adopt an external tmux session via `POST /tmux/adopt`.
    ///
    /// Why: brings a session trusty-mpm did not create under oversight.
    /// What: POSTs the session name; returns `Ok(true)` on success, `Ok(false)`
    /// when the session was not found.
    /// Test: covered by the daemon's tmux tests.
    pub async fn adopt_tmux_session(&self, name: &str) -> anyhow::Result<bool> {
        let url = format!("{}/tmux/adopt", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "session": name }))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }

    /// Auto-discover tmux sessions running Claude Code via
    /// `POST /sessions/discover`.
    ///
    /// Why: the `/discover` command (TUI and Telegram) triggers a daemon scan
    /// of every tmux pane and adopts the ones running Claude Code.
    /// What: POSTs to `/sessions/discover`; returns the count of newly-adopted
    /// sessions reported by the daemon.
    /// Test: `discover_sessions_returns_count` in the daemon's `api_tests.rs`.
    pub async fn discover_sessions(&self) -> anyhow::Result<usize> {
        let url = format!("{}/sessions/discover", self.base);
        let body: serde_json::Value = self
            .http
            .post(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body
            .get("discovered")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize)
    }

    /// Analyze a project's Claude Code config via `GET /claude-config`.
    ///
    /// Why: the `/config` command surfaces analyzer recommendations.
    /// What: `GET /claude-config?project=<path>`, returns one
    /// [`ConfigRecommendation`] per recommendation.
    /// Test: covered by the executor's config test.
    pub async fn analyze_config(&self, project: &str) -> anyhow::Result<Vec<ConfigRecommendation>> {
        let url = format!("{}/claude-config", self.base);
        let body: serde_json::Value = self
            .http
            .get(&url)
            .query(&[("project", project)])
            .send()
            .await?
            .json()
            .await?;
        let recs = body["recommendations"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(recs
            .iter()
            .map(|r| ConfigRecommendation {
                id: r
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                message: r
                    .get("message")
                    .and_then(|v| v.as_str())
                    .or_else(|| r.as_str())
                    .unwrap_or("?")
                    .to_string(),
            })
            .collect())
    }

    /// Apply a config recommendation via `POST /claude-config/apply`.
    ///
    /// Why: lets a UI act on a recommendation without hand-editing JSON.
    /// What: POSTs the project path and recommendation id; returns the
    /// checkpoint id on success.
    /// Test: covered by the daemon's claude-config tests.
    pub async fn apply_recommendation(
        &self,
        project: &str,
        recommendation_id: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{}/claude-config/apply", self.base);
        let body: serde_json::Value = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "project": project,
                "recommendation_id": recommendation_id,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body
            .get("checkpoint_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// List a project's config checkpoints via `GET /claude-config/checkpoints`.
    ///
    /// Why: a UI offers a restore picker fed by this list.
    /// What: returns the raw checkpoint JSON array.
    /// Test: covered by the daemon's claude-config tests.
    pub async fn list_checkpoints(&self, project: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let url = format!("{}/claude-config/checkpoints", self.base);
        let body: serde_json::Value = self
            .http
            .get(&url)
            .query(&[("project", project)])
            .send()
            .await?
            .json()
            .await?;
        Ok(body["checkpoints"].as_array().cloned().unwrap_or_default())
    }

    /// Deploy a built-in profile via `POST /claude-config/deploy`.
    ///
    /// Why: lets a UI apply a configuration preset in one call.
    /// What: POSTs the project path and profile name; returns the checkpoint id.
    /// Test: covered by the daemon's claude-config tests.
    pub async fn deploy_profile(
        &self,
        project: &str,
        profile_name: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{}/claude-config/deploy", self.base);
        let body: serde_json::Value = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "project": project,
                "profile_name": profile_name,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body
            .get("checkpoint_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Request a one-time pairing code via `POST /pair/request`.
    ///
    /// Why: `tm pair` asks the local daemon for a code to type into the bot.
    /// What: POSTs an empty body; returns the generated code and its TTL.
    /// Test: covered by the executor's pairing test.
    pub async fn pair_request(&self) -> anyhow::Result<PairRequest> {
        let url = format!("{}/pair/request", self.base);
        let body: PairRequest = self
            .http
            .post(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body)
    }

    /// Confirm a pairing code via `POST /pair/confirm`.
    ///
    /// Why: the bot's `/pair <code>` flow registers its chat with the daemon.
    /// What: POSTs the code and chat id; returns the success / error result.
    /// Test: covered by the executor's pairing test.
    pub async fn pair_confirm(&self, code: &str, chat_id: i64) -> anyhow::Result<PairConfirm> {
        let url = format!("{}/pair/confirm", self.base);
        let body: PairConfirm = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "code": code, "chat_id": chat_id }))
            .send()
            .await?
            .json()
            .await?;
        Ok(body)
    }

    /// Send a chat message to the daemon's LLM assistant via `POST /llm/chat`.
    ///
    /// Why: free-text Telegram messages and the TUI's `/chat` command route to
    /// the daemon's conversational endpoint; the UI owns the rolling history
    /// and threads it through each turn.
    /// What: POSTs `{ message, history }`; returns `Ok(Some(outcome))` with the
    /// reply and updated history on success, `Ok(None)` when the daemon answers
    /// `503` (LLM chat not configured), and `Err` on transport failure.
    /// Test: `llm_chat_response_deserializes` covers the wire shape.
    pub async fn llm_chat(
        &self,
        message: &str,
        history: &[ChatMessage],
    ) -> anyhow::Result<Option<LlmChatOutcome>> {
        let url = format!("{}/llm/chat", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "message": message, "history": history }))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(None);
        }
        let outcome: LlmChatOutcome = resp.error_for_status()?.json().await?;
        Ok(Some(outcome))
    }

    /// Fetch the cross-session coordinator snapshot.
    ///
    /// Why: the TUI/GUI coordinator sidebar refreshes its session list from the
    /// daemon's activity snapshot — every session with its status and a
    /// recent-output excerpt.
    /// What: `GET /api/v1/coordinator/context`, returns the deserialized
    /// [`CoordinatorContext`]; `Err` on a transport or decode failure.
    /// Test: `coordinator_context_deserializes` covers the wire shape.
    pub async fn coordinator_context(&self) -> anyhow::Result<CoordinatorContext> {
        let url = format!("{}/api/v1/coordinator/context", self.base);
        let context = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(context)
    }

    /// Send a message to the cross-session coordinator.
    ///
    /// Why: the coordinator is the operator's one conversational surface over
    /// every session — a `@prefix:` message routes a command at a named
    /// session, a plain message is answered by the LLM with full session
    /// context. The UI owns the rolling chat history and threads it through.
    /// What: POSTs `{ message, history }` to `/api/v1/coordinator/chat`; returns
    /// `Ok(Some(outcome))` on success, `Ok(None)` when the daemon answers `503`
    /// (LLM not configured for a non-prefixed message), and `Err` on transport
    /// failure.
    /// Test: `coordinator_chat_outcome_deserializes` covers the wire shape.
    pub async fn coordinator_chat(
        &self,
        message: &str,
        history: &[ChatMessage],
    ) -> anyhow::Result<Option<CoordinatorChatOutcome>> {
        let url = format!("{}/api/v1/coordinator/chat", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "message": message, "history": history }))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(None);
        }
        let outcome: CoordinatorChatOutcome = resp.error_for_status()?.json().await?;
        Ok(Some(outcome))
    }

    /// Query pairing status via `GET /pair/status`.
    ///
    /// Why: the `/start` command branches on whether the daemon is paired.
    /// What: `GET /pair/status`, returns the paired flag and chat id.
    /// Test: covered by the executor's pairing test.
    pub async fn pair_status(&self) -> anyhow::Result<PairStatus> {
        let url = format!("{}/pair/status", self.base);
        let body: PairStatus = self.http.get(&url).send().await?.json().await?;
        Ok(body)
    }

    /// Run the full system diagnostic via `GET /api/v1/doctor`.
    ///
    /// Why: the `tm doctor` CLI command and the Telegram `/doctor` command both
    /// need the daemon's verdict on whether the trusty-mpm stack is correctly
    /// wired; this is the one transport call behind both.
    /// What: `GET /api/v1/doctor`, passing the caller's `project` path so the
    /// daemon can scope the instruction-pipeline probe. Returns the parsed
    /// [`DoctorReport`]; `Err` on a transport or decode failure.
    /// Test: covered by the executor's doctor test.
    pub async fn doctor(
        &self,
        project: Option<&str>,
    ) -> anyhow::Result<crate::core::doctor::DoctorReport> {
        let url = format!("{}/api/v1/doctor", self.base);
        let mut request = self.http.get(&url);
        if let Some(project) = project {
            request = request.query(&[("project", project)]);
        }
        let report = request.send().await?.error_for_status()?.json().await?;
        Ok(report)
    }

    /// Launch a fresh Claude Code session in `workdir`.
    ///
    /// Why: the TUI's `/connect <dir>` command is the single entry point for
    /// "connect to or launch a session for a project" — when no session exists
    /// for a directory it must start one, mirroring `tm session start`. A
    /// trusty-mpm session is always the `claude` (Claude Code) CLI, never
    /// `claude-mpm`; the trusty-mpm behaviour comes from the custom instructions
    /// (deployed agents + project `CLAUDE.md`) prepared before launch.
    /// What: runs [`crate::core::session_launch::prepare_session`] (deploy
    /// agents + merge `CLAUDE.md`), POSTs `{project, project_path}` to
    /// `/sessions`, then creates a detached tmux session via `tmux new-session`
    /// and starts `claude` in it via `tmux send-keys`. Returns the
    /// daemon-assigned tmux session name. The daemon only registers session
    /// state; the prep and launch (tmux + process) are owned by the client,
    /// exactly as the CLI does it.
    /// Test: `launch_session_errors_when_daemon_unreachable`.
    pub async fn launch_session(&self, workdir: &str) -> anyhow::Result<String> {
        // Prepare the custom instructions Claude Code reads at startup: deploy
        // composed agents to `~/.claude/agents/` and merge the project
        // `CLAUDE.md`. A prep failure is logged but not fatal — the session can
        // still launch with whatever instructions already exist on disk.
        let fw = crate::core::paths::FrameworkPaths::default();
        if let Err(err) =
            crate::core::session_launch::prepare_session(&fw, std::path::Path::new(workdir))
        {
            tracing::warn!(%err, "session pre-launch preparation failed");
        }

        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            name: String,
        }
        let url = format!("{}/sessions", self.base);
        let body: Body = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "project": workdir,
                "project_path": workdir,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // Build the combined `--append-system-prompt` text (claude-mpm PM
        // instructions + trusty tool-priority block), resolved *for this project
        // directory* so override files under `<workdir>/.trusty-mpm/` take effect
        // (issue #381). Write it to a temp file and pass it via
        // `--append-system-prompt-file` so every launched `claude` is a properly
        // configured PM instance while preserving Claude Code's built-in tool use
        // instructions. The temp file persists because `claude` reads it at
        // startup; it lives in `/tmp` and is superseded by the next launch — no
        // explicit cleanup is performed.
        let prompt =
            crate::core::session_launch::build_system_prompt_for(std::path::Path::new(workdir));
        let claude_cmd = {
            let path = std::env::temp_dir().join(format!(
                "trusty-mpm-system-prompt-{}.txt",
                uuid::Uuid::new_v4()
            ));
            match std::fs::write(&path, &prompt) {
                Ok(()) => format!("claude --append-system-prompt-file {}", path.display()),
                Err(err) => {
                    tracing::warn!(%err, "failed to write system prompt file; launching bare claude");
                    "claude".to_string()
                }
            }
        };

        let new_session = std::process::Command::new("tmux")
            .args(["new-session", "-d", "-s", &body.name, "-c", workdir])
            .status();
        match new_session {
            Ok(status) if status.success() => {
                let send = std::process::Command::new("tmux")
                    .args(["send-keys", "-t", &body.name, &claude_cmd, "Enter"])
                    .status();
                if !matches!(send, Ok(s) if s.success()) {
                    return Err(anyhow::anyhow!(
                        "tmux session {} created but failed to start claude",
                        body.name
                    ));
                }
            }
            Ok(_) | Err(_) => {
                return Err(anyhow::anyhow!(
                    "failed to create tmux session {} in {}",
                    body.name,
                    workdir
                ));
            }
        }
        Ok(body.name)
    }

    /// Connect to — or start — a Claude Code session in `workdir` *without*
    /// running the framework-deployment sequence.
    ///
    /// Why: `tm connect` is the lightweight sibling of `launch_session`. Where
    /// `launch_session` first runs
    /// [`crate::core::session_launch::prepare_session`] to deploy
    /// instructions, agents, and skills into the project, `connect` deliberately
    /// skips all of that — it assumes the framework is already deployed (or that
    /// the operator does not want it touched) and only wants the daemon to know
    /// about the session and the tmux host to be running.
    /// What: POSTs `{project, project_path}` to `/api/v1/sessions/connect`, then
    /// runs `tmux new-session -A` (idempotent — creates the session when absent,
    /// no-ops when it already exists). When the session is freshly created it
    /// starts `claude` in it via `tmux send-keys`; an already-running session is
    /// left untouched. The system-prompt file is still built and passed so a
    /// freshly-started `claude` is a configured PM — that is prompt composition,
    /// not artifact deployment. Returns the daemon-assigned tmux session name.
    /// Test: `connect_session_errors_when_daemon_unreachable`.
    pub async fn connect_session(&self, workdir: &str) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            name: String,
        }
        let url = format!("{}/api/v1/sessions/connect", self.base);
        let body: Body = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "project": workdir,
                "project_path": workdir,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // Build the `--append-system-prompt` text so a freshly-started `claude`
        // is a configured PM, resolved *for this project directory* so override
        // files under `<workdir>/.trusty-mpm/` take effect (issue #381). This is
        // prompt composition from bundled assets + project overrides, not
        // deployment of agents/skills/hooks into the project — `connect` only
        // skips the latter (`prepare_session`).
        let prompt =
            crate::core::session_launch::build_system_prompt_for(std::path::Path::new(workdir));
        let claude_cmd = {
            let path = std::env::temp_dir().join(format!(
                "trusty-mpm-system-prompt-{}.txt",
                uuid::Uuid::new_v4()
            ));
            match std::fs::write(&path, &prompt) {
                Ok(()) => format!("claude --append-system-prompt-file {}", path.display()),
                Err(err) => {
                    tracing::warn!(%err, "failed to write system prompt file; launching bare claude");
                    "claude".to_string()
                }
            }
        };

        // `tmux new-session -A` is idempotent: it attaches to the session when
        // it already exists and creates it (detached, `-d`) otherwise. The
        // `has-session` probe distinguishes the two so `claude` is started only
        // for a freshly-created session — an already-running one is left alone.
        let already_running = std::process::Command::new("tmux")
            .args(["has-session", "-t", &body.name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let new_session = std::process::Command::new("tmux")
            .args(["new-session", "-A", "-d", "-s", &body.name, "-c", workdir])
            .status();
        match new_session {
            Ok(status) if status.success() => {
                if !already_running {
                    let send = std::process::Command::new("tmux")
                        .args(["send-keys", "-t", &body.name, &claude_cmd, "Enter"])
                        .status();
                    if !matches!(send, Ok(s) if s.success()) {
                        return Err(anyhow::anyhow!(
                            "tmux session {} created but failed to start claude",
                            body.name
                        ));
                    }
                }
            }
            Ok(_) | Err(_) => {
                return Err(anyhow::anyhow!(
                    "failed to create tmux session {} in {}",
                    body.name,
                    workdir
                ));
            }
        }
        Ok(body.name)
    }
}

/// Render a tmux snapshot JSON value as a flat text block.
///
/// Why: the daemon's snapshot payload may be a plain string or an object with a
/// `content` / `lines` field; a UI needs a single string.
/// What: returns the string form, joining a `lines` array if present.
/// Test: covered indirectly by `snapshot_tmux_session`.
fn snapshot_text(snapshot: &serde_json::Value) -> String {
    if let Some(s) = snapshot.as_str() {
        return s.to_string();
    }
    if let Some(content) = snapshot.get("content").and_then(|v| v.as_str()) {
        return content.to_string();
    }
    if let Some(lines) = snapshot.get("lines").and_then(|v| v.as_array()) {
        return lines
            .iter()
            .filter_map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
    }
    snapshot.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_is_stored() {
        let client = DaemonClient::new("http://127.0.0.1:7880");
        assert_eq!(client.base_url(), "http://127.0.0.1:7880");
    }

    #[test]
    fn set_base_url_repoints_client() {
        // Why: a long-lived UI must follow the daemon to a new ephemeral port
        // after a restart; `set_base_url` is what makes that re-pointing possible.
        let mut client = DaemonClient::new("http://127.0.0.1:7880");
        client.set_base_url("http://127.0.0.1:54321");
        assert_eq!(client.base_url(), "http://127.0.0.1:54321");
    }

    #[tokio::test]
    async fn launch_session_errors_when_daemon_unreachable() {
        // Why: `/connect <dir>` launches via `launch_session`; when the daemon
        // POST fails (port 0 never connects) the error must surface rather than
        // proceeding to spawn tmux against an unregistered session.
        let client = DaemonClient::new("http://127.0.0.1:0");
        let result = client.launch_session("/tmp/no-such-project").await;
        assert!(result.is_err(), "expected launch to fail with no daemon");
    }

    #[tokio::test]
    async fn connect_session_errors_when_daemon_unreachable() {
        // Why: `tm connect` registers via `POST /api/v1/sessions/connect`
        // before touching tmux; when the daemon POST fails the error must
        // surface rather than proceeding to spawn tmux against an
        // unregistered session.
        let client = DaemonClient::new("http://127.0.0.1:0");
        let result = client.connect_session("/tmp/no-such-project").await;
        assert!(result.is_err(), "expected connect to fail with no daemon");
    }

    #[test]
    fn session_row_deserializes_tmux_name() {
        let json = serde_json::json!({
            "id": "abcd1234-5678-90ab-cdef-1234567890ab",
            "workdir": "/tmp/proj",
            "status": "Active",
            "active_delegations": 1,
            "tmux_name": "tmpm-quiet-falcon"
        });
        let row: SessionRow = serde_json::from_value(json).unwrap();
        assert_eq!(row.tmux_name, "tmpm-quiet-falcon");
    }

    #[test]
    fn session_row_defaults_tmux_name_when_absent() {
        let json = serde_json::json!({
            "id": "abcd1234-5678-90ab-cdef-1234567890ab",
            "workdir": "/tmp/proj",
            "status": "Active"
        });
        let row: SessionRow = serde_json::from_value(json).unwrap();
        assert_eq!(row.tmux_name, "");
        assert_eq!(row.last_seen.secs_since_epoch, 0);
    }

    #[test]
    fn events_deserialize_from_record_shape() {
        let json = serde_json::json!({
            "session": "abcd1234-5678-90ab-cdef-1234567890ab",
            "event": "PreToolUse",
            "at": "2024-01-01T00:00:00Z",
            "payload": {}
        });
        let row: EventRow = serde_json::from_value(json).unwrap();
        assert_eq!(row.event, crate::core::hook::HookEvent::PreToolUse);
        assert_eq!(row.at, "2024-01-01T00:00:00Z");
    }

    #[test]
    fn events_default_payload_when_absent() {
        let json = serde_json::json!({
            "session": "abcd1234-5678-90ab-cdef-1234567890ab",
            "event": "Stop",
            "at": "2024-01-01T00:00:00Z"
        });
        let row: EventRow = serde_json::from_value(json).unwrap();
        assert!(row.payload.is_null());
    }

    #[test]
    fn breakers_deserialize_from_api_shape() {
        let json = serde_json::json!({
            "agent": "research",
            "breaker": { "state": "closed", "consecutive_failures": 0 }
        });
        #[derive(serde::Deserialize)]
        struct WireBreaker {
            state: String,
            consecutive_failures: u32,
        }
        #[derive(serde::Deserialize)]
        struct WireRow {
            agent: String,
            breaker: WireBreaker,
        }
        let row: WireRow = serde_json::from_value(json).unwrap();
        assert_eq!(row.agent, "research");
        assert_eq!(row.breaker.state, "closed");
        assert_eq!(row.breaker.consecutive_failures, 0);
    }

    #[test]
    fn tmux_session_row_accepts_name() {
        // The snapshot helper joins a `lines` array; the name parse is exercised
        // here directly on both wire shapes.
        let obj = serde_json::json!({"name": "tmpm-quiet-falcon"});
        assert_eq!(
            obj.get("name").and_then(|v| v.as_str()),
            Some("tmpm-quiet-falcon")
        );
        let plain = serde_json::json!("external-shell");
        assert_eq!(plain.as_str(), Some("external-shell"));
    }

    #[test]
    fn snapshot_text_handles_each_shape() {
        assert_eq!(snapshot_text(&serde_json::json!("plain")), "plain");
        assert_eq!(
            snapshot_text(&serde_json::json!({"content": "from content"})),
            "from content"
        );
        assert_eq!(
            snapshot_text(&serde_json::json!({"lines": ["a", "b"]})),
            "a\nb"
        );
    }

    #[test]
    fn pair_request_deserializes() {
        let json = serde_json::json!({"code": "A4X9KZ", "expires_in_seconds": 300});
        let req: PairRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.code, "A4X9KZ");
        assert_eq!(req.expires_in_seconds, 300);
    }

    #[test]
    fn pair_confirm_deserializes_failure() {
        let json = serde_json::json!({"success": false, "error": "invalid or expired code"});
        let confirm: PairConfirm = serde_json::from_value(json).unwrap();
        assert!(!confirm.success);
        assert_eq!(confirm.error.as_deref(), Some("invalid or expired code"));
        assert_eq!(confirm.chat_id, None);
    }

    #[test]
    fn llm_chat_message_round_trips() {
        // A ChatMessage serializes to the `{role, content}` wire shape the
        // daemon expects and deserializes back unchanged.
        let msg = ChatMessage {
            role: "user".into(),
            content: "hello".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hello");
        let back: ChatMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn chat_message_constructors_set_role() {
        assert_eq!(ChatMessage::user("x").role, "user");
        assert_eq!(ChatMessage::assistant("y").role, "assistant");
        assert_eq!(ChatMessage::user("x").content, "x");
    }

    #[test]
    fn llm_chat_response_deserializes() {
        // The `POST /llm/chat` response carries the reply and updated history.
        let json = serde_json::json!({
            "reply": "hi there",
            "history": [
                { "role": "user", "content": "hello" },
                { "role": "assistant", "content": "hi there" },
            ],
        });
        let outcome: LlmChatOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(outcome.reply, "hi there");
        assert_eq!(outcome.history.len(), 2);
        assert_eq!(outcome.history[1].role, "assistant");
    }

    #[test]
    fn coordinator_context_deserializes() {
        // The `GET /api/v1/coordinator/context` snapshot carries the session
        // summaries; the daemon's `recent_events` field is ignored.
        let json = serde_json::json!({
            "sessions": [{
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "tmpm-aipowerranking",
                "prefix": "aipowerranking",
                "workdir": "/tmp/proj",
                "status": "Active",
                "active_delegations": 3,
                "recent_output": ["building…"],
            }],
            "recent_events": [],
            "generated_at": "2026-05-19T00:00:00Z",
        });
        let context: CoordinatorContext = serde_json::from_value(json).unwrap();
        assert_eq!(context.sessions.len(), 1);
        assert_eq!(context.sessions[0].prefix, "aipowerranking");
        assert_eq!(context.sessions[0].active_delegations, 3);
    }

    #[test]
    fn coordinator_chat_outcome_deserializes() {
        // A routed-command outcome carries the session name and pane output.
        let json = serde_json::json!({
            "reply": "Sent to tmpm-foo: run tests",
            "routed_to_session": "tmpm-foo",
            "command_output": "tests passed",
        });
        let outcome: CoordinatorChatOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(outcome.routed_to_session.as_deref(), Some("tmpm-foo"));
        assert_eq!(outcome.command_output.as_deref(), Some("tests passed"));

        // A plain LLM reply omits the routing fields.
        let json = serde_json::json!({ "reply": "two sessions are active" });
        let outcome: CoordinatorChatOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(outcome.reply, "two sessions are active");
        assert!(outcome.routed_to_session.is_none());
    }

    #[test]
    fn pair_status_deserializes() {
        let json = serde_json::json!({"paired": true, "chat_id": 12345678});
        let status: PairStatus = serde_json::from_value(json).unwrap();
        assert!(status.paired);
        assert_eq!(status.chat_id, Some(12345678));
    }
}
