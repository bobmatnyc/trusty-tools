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

mod managed;
mod session_connect;
#[cfg(test)]
mod tests;
mod types;

pub use types::{
    BreakerRow, ChatMessage, ConfigRecommendation, CoordinatorChatOutcome, CoordinatorContext,
    CoordinatorSession, DiscoveredProjectRow, EventRow, HealthSnapshot, LastSeen, LlmChatOutcome,
    ManagedActivityResponse, ManagedAnswerRequest, ManagedAnswerResponse, ManagedAttachCmdResponse,
    ManagedListResponse, ManagedSendInputRequest, ManagedSendInputResponse, ManagedSessionSummary,
    ManagedSpawnRequest, ManagedSpawnResponse, OverseerSnapshot, PairConfirm, PairRequest,
    PairStatus, SessionRow, TmuxSessionRow,
};

use serde::Deserialize;

/// HTTP client for one trusty-mpm daemon.
///
/// Why: a thin wrapper so any UI can be pointed at any daemon address.
/// What: holds the base URL and a shared `reqwest::Client`.
/// Test: `base_url_is_stored`.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    /// Base URL of the daemon, e.g. `http://127.0.0.1:7880`.
    pub(in crate::client::http_client) base: String,
    /// Shared connection-pooling HTTP client.
    pub(in crate::client::http_client) http: reqwest::Client,
}

impl DaemonClient {
    /// Build a client targeting `base` (e.g. `http://127.0.0.1:7880`).
    ///
    /// Why: a UI is pointed at a daemon address resolved from a flag or the
    /// service lock file.
    /// What: stores the base URL and a fresh pooled `reqwest::Client`.
    /// Test: `base_url_is_stored`.
    pub fn new(base: impl Into<String>) -> Self {
        Self::with_client(reqwest::Client::new(), base)
    }

    /// Build a client targeting `base`, REUSING an existing `reqwest::Client`.
    ///
    /// Why: callers that already configured a `reqwest::Client` (custom TLS,
    /// timeouts, proxy, connection pool) must not have that configuration
    /// silently dropped by [`Self::new`] minting a fresh default client. Reusing
    /// the caller's client is cheap — `reqwest::Client` is an `Arc` internally,
    /// so cloning shares the same pool and settings.
    /// What: stores `base` and adopts the passed `client` verbatim as the HTTP
    /// transport.
    /// Test: `with_client_reuses_passed_client` asserts the base is stored and
    /// the client is adopted.
    pub fn with_client(client: reqwest::Client, base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            http: client,
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

    /// Fetch the daemon's catalog-staleness flag from `GET /health` (HR-3).
    ///
    /// Why: the coordinator TUI shows an "updates available" indicator when the
    /// deployed harness content has drifted from the synced catalog (DOC-17
    /// §HR-3). It already polls health on its timer; reading the additive
    /// `catalog_stale` field reuses that one probe rather than adding a request.
    /// What: GETs `/health`, parses the JSON body, and returns `catalog_stale`.
    /// Any transport/parse failure (or an older daemon returning the legacy
    /// string body) yields `false` so the indicator degrades to "no updates"
    /// rather than erroring — staleness never blocks the TUI.
    /// Test: `coord_poll_daemon` integration is exercised against a live daemon;
    /// the parse contract (incl. the missing-field default) is covered by
    /// `catalog_stale_health_body_wire_shape`.
    pub async fn catalog_stale(&self) -> bool {
        let url = format!("{}/health", self.base);
        let Ok(resp) = self.http.get(&url).send().await else {
            return false;
        };
        if !resp.status().is_success() {
            return false;
        }
        #[derive(Deserialize)]
        struct HealthBody {
            #[serde(default)]
            catalog_stale: bool,
        }
        resp.json::<HealthBody>()
            .await
            .map(|b| b.catalog_stale)
            .unwrap_or(false)
    }

    /// Fetch the daemon's full `GET /health` snapshot.
    ///
    /// Why: the `health` verb needs more than the liveness boolean
    /// [`Self::is_healthy`] returns — it surfaces the daemon's liveness word and
    /// the catalog-freshness flags (HR-3) in one probe. Reading them as a typed
    /// struct keeps the verb off raw JSON indexing.
    /// What: GETs `/health`, returns the parsed [`HealthSnapshot`]. `Err` on any
    /// transport or decode failure so the caller renders the daemon as
    /// unreachable rather than panicking. Missing fields default (an older daemon
    /// returning only `status` still parses).
    /// Test: `health_snapshot_deserializes` covers the wire shape; the executor's
    /// `execute_health_*` tests exercise the live and dead-daemon paths.
    pub async fn health_snapshot(&self) -> anyhow::Result<HealthSnapshot> {
        let url = format!("{}/health", self.base);
        let snapshot = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(snapshot)
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
    /// What: `GET /api/v1/sessions/context`, returns the deserialized
    /// [`CoordinatorContext`]; `Err` on a transport or decode failure.
    /// Test: `coordinator_context_deserializes` covers the wire shape.
    pub async fn coordinator_context(&self) -> anyhow::Result<CoordinatorContext> {
        let url = format!("{}/api/v1/sessions/context", self.base);
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
    /// context, and when `actions` is `true` the session-manager may invoke
    /// managed-session verbs INLINE (#1283) so natural language can DRIVE the
    /// fleet, not merely describe it. The UI owns the rolling chat history and
    /// threads it through.
    /// What: POSTs `{ message, history, actions }` to `/api/v1/sessions/chat`;
    /// the `actions` flag opts into the action-capable SM branch. Returns
    /// `Ok(Some(outcome))` on success, `Ok(None)` when the daemon answers `503`
    /// (LLM not configured for a non-prefixed message), and `Err` on transport
    /// failure. On the action path the outcome's `actions_taken` lists any verbs
    /// that ran.
    /// Test: `coordinator_chat_outcome_deserializes`,
    /// `coordinator_chat_serializes_actions_flag` cover the wire shape.
    pub async fn coordinator_chat(
        &self,
        message: &str,
        history: &[ChatMessage],
        actions: bool,
    ) -> anyhow::Result<Option<CoordinatorChatOutcome>> {
        let url = format!("{}/api/v1/sessions/chat", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&coordinator_chat_body(message, history, actions))
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
}

/// Build the `POST /api/v1/sessions/chat` request body.
///
/// Why: extracting the body shape from [`DaemonClient::coordinator_chat`] makes
/// the `actions` opt-in serializable and unit-testable without a live HTTP
/// round-trip — the wire contract is what the daemon's
/// `CoordinatorChatRequest` reads, so it must be pinned by a test.
/// What: returns a JSON object with `message`, `history`, and the `actions`
/// boolean (the flag that routes the action-capable SM branch when `true`).
/// Test: `coordinator_chat_serializes_actions_flag`.
pub(crate) fn coordinator_chat_body(
    message: &str,
    history: &[ChatMessage],
    actions: bool,
) -> serde_json::Value {
    serde_json::json!({
        "message": message,
        "history": history,
        "actions": actions,
    })
}

/// Render a tmux snapshot JSON value as a flat text block.
///
/// Why: the daemon's snapshot payload may be a plain string or an object with a
/// `content` / `lines` field; a UI needs a single string.
/// What: returns the string form, joining a `lines` array if present.
/// Test: covered indirectly by `snapshot_tmux_session`.
pub(crate) fn snapshot_text(snapshot: &serde_json::Value) -> String {
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
