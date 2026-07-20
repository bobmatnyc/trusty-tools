//! [`CodeEngine`]: the `trusty_tui::TuiEngine` adapter driving a long-lived
//! `tcode serve --http` daemon (issue #3415, DOC-50 §3.3/§3.4).
//!
//! Why: see `crate::tui_client`'s module docs for the ephemeral-`--stdio`
//! vs. long-lived-`--http` client distinction. This module is the `TuiEngine`
//! impl itself — everything else in `tui_client` (`discovery`, `rpc`, `sse`)
//! exists to support it.
//! What: [`EngineState`] holds every piece of daemon-observed state this
//! client caches (current session id, last-known active workstream, and —
//! ahead of `trusty-tui` Slice 1.5's SYNCHRONOUS `TuiEngine::commands()`/
//! `picker(name)` accessors (#3428, landing into `crates/trusty-tui`
//! alongside this slice) — a `commands`/`picker` cache populated during the
//! async `setup()`/event-handling paths so those future sync accessors never
//! need to perform network I/O inline (see [`EngineState::commands`]/
//! [`EngineState::picker`]'s docs for why `std::sync::Mutex`, not
//! `tokio::sync::Mutex`, guards this state). `CodeEngine` itself is a thin
//! `Arc<EngineState>` wrapper so the background workstream-subscription task
//! spawned by `subscribe_workstream_events` can share the SAME state (and
//! refresh the SAME caches) without a second, divergent copy.
//! Test: `engine_tests::*` for the pure event-mapping/parsing helpers below;
//! the full discover -> setup -> stream -> cancel -> workstream-activation
//! flow against a mock HTTP daemon lives in `tests/tui_client_engine.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use trusty_tui::{CommandDescriptor, PickerItem, ReplEvent, TuiEngine, WorkstreamSummary};

use crate::events::{Event, SessionEventEnvelope};

use super::discovery::discover_daemon_url;
use super::error::EngineError;
use super::rpc::RpcHttpClient;
use super::sse::SseLines;

/// How long a session-event / workstream-event SSE read may sit idle (no
/// bytes, not even a keep-alive comment) before this client treats the
/// connection as dead and reconnects. Axum's default `KeepAlive` sends a
/// comment roughly every 15s (`crate::serve::http::session_events_sse`'s
/// `KeepAlive::default()`), so three missed beats is a generous margin
/// before declaring the daemon unreachable.
const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(45);

/// Fixed backoff between SSE reconnect attempts. Not exponential — MVP
/// scope (DOC-50 §5 Slice 3); a persistently-down daemon retries at a
/// steady, human-visible cadence rather than hot-looping.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);

/// How many times [`EngineState::pump_session_events`] reconnects before
/// giving up and returning control to the caller (`handle_input` must
/// eventually return so the REPL's input loop stays responsive — unlike the
/// workstream-activation subscription, which is meant to run for the whole
/// TUI session).
const SESSION_STREAM_MAX_RECONNECTS: u32 = 5;

/// Every piece of daemon-observed state [`CodeEngine`] caches, shared (via
/// `Arc`) between the foreground `TuiEngine` calls and the background
/// workstream-subscription task.
struct EngineState {
    rpc: RpcHttpClient,
    /// Project root to bind the session to, if any (mirrors `session.create`'s
    /// `project` param — see `crate::session::protocol::create`'s docs).
    project_path: Option<PathBuf>,
    session_id: Mutex<Option<String>>,
    active_workstream: Mutex<Option<WorkstreamSummary>>,
    /// Ahead-of-Slice-1.5 cache for `TuiEngine::commands()` (#3428) — see
    /// module docs. Populated once, in `setup()`; static for this MVP (the
    /// one engine-routed command, `/workstream`, never changes at runtime).
    commands_cache: Mutex<Vec<CommandDescriptor>>,
    /// Ahead-of-Slice-1.5 cache for `TuiEngine::picker(name)` (#3428) — see
    /// module docs. Keyed by picker name (matches the DOC-50-noted
    /// convention that a command name doubles as its picker name, e.g.
    /// `/workstream` <-> `picker("workstream")`). Refreshed in `setup()`,
    /// after a successful `/workstream activate`, and whenever the
    /// background subscription observes a `WorkstreamActivationChanged`
    /// event — see [`EngineState::refresh_workstream_cache`].
    picker_cache: Mutex<HashMap<String, Vec<PickerItem>>>,
    shutting_down: AtomicBool,
}

impl EngineState {
    /// `std::sync::Mutex`, not `tokio::sync::Mutex` — see module docs:
    /// `commands()`/`picker()` are, per #3428, plain synchronous `fn`s (no
    /// `.await` available), so the cache they read MUST be lockable without
    /// an async runtime. Every lock here is held only long enough to clone a
    /// small `Vec`/`Option` — never across an `.await` point.
    fn commands(&self) -> Vec<CommandDescriptor> {
        self.commands_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn picker(&self, name: &str) -> Vec<PickerItem> {
        self.picker_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// Re-fetch `workstream.list` and refresh both `active_workstream` and
    /// the `"workstream"` picker cache entry. Best-effort: an RPC failure
    /// here (daemon transiently unreachable) leaves the previous cache
    /// contents in place rather than clearing them — a stale picker list is
    /// strictly better than an empty one.
    async fn refresh_workstream_cache(&self) -> Option<WorkstreamSummary> {
        let result = self.rpc.call("workstream.list", json!({})).await.ok()?;
        let active_id = result
            .get("active_workstream_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let workstreams: Vec<Value> = result
            .get("workstreams")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let items: Vec<PickerItem> = workstreams
            .iter()
            .filter_map(|w| {
                let id = w.get("id").and_then(Value::as_str)?.to_string();
                let name = w.get("name").and_then(Value::as_str).unwrap_or_default();
                let label = if name.is_empty() {
                    id.clone()
                } else {
                    name.to_string()
                };
                Some(PickerItem { id, label })
            })
            .collect();
        *self.picker_cache.lock().unwrap_or_else(|e| e.into_inner()) = {
            let mut map = HashMap::new();
            map.insert("workstream".to_string(), items);
            map
        };

        let active = active_id.and_then(|id| {
            workstreams
                .iter()
                .find(|w| w.get("id").and_then(Value::as_str) == Some(id.as_str()))
                .map(|w| WorkstreamSummary {
                    id: id.clone(),
                    name: w
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
                .or(Some(WorkstreamSummary {
                    id,
                    name: String::new(),
                }))
        });
        *self
            .active_workstream
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = active.clone();
        active
    }

    /// Route one submitted `/workstream`/`/ws` subcommand (`rest` is
    /// whatever followed the command name, already trimmed; empty means
    /// "no subcommand" -> defaults to `list`).
    async fn handle_workstream_command(
        &self,
        rest: &str,
        tx: &UnboundedSender<ReplEvent>,
    ) -> Result<(), EngineError> {
        if rest.is_empty() || rest == "list" {
            let ws = self.refresh_workstream_cache().await;
            let items = self.picker("workstream");
            let active_id = ws.as_ref().map(|w| w.id.as_str());
            let rows: Vec<String> = items
                .iter()
                .map(|item| {
                    let marker = if Some(item.id.as_str()) == active_id {
                        "*"
                    } else {
                        " "
                    };
                    format!("{marker} {} {}", item.id, item.label)
                })
                .collect();
            let msg = if rows.is_empty() {
                "no workstreams".to_string()
            } else {
                rows.join("\n")
            };
            let _ = tx.send(ReplEvent::StatusMessage(msg));
            return Ok(());
        }

        if let Some(id) = rest.strip_prefix("activate ") {
            let id = id.trim().to_string();
            let result = self
                .rpc
                .call("workstream.activate", json!({ "id": id }))
                .await?;
            let active_id = result
                .get("active_id")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            let ws = self
                .refresh_workstream_cache()
                .await
                .unwrap_or(WorkstreamSummary {
                    id: active_id.clone(),
                    name: String::new(),
                });
            let _ = tx.send(ReplEvent::WorkstreamUpdated(ws));
            let _ = tx.send(ReplEvent::StatusMessage(format!(
                "activated workstream {active_id}"
            )));
            return Ok(());
        }

        let _ = tx.send(ReplEvent::StatusMessage(format!(
            "unknown /workstream subcommand: {rest} (try `list` or `activate <id>`)"
        )));
        Ok(())
    }

    /// Send one chat line as a fresh `task.run` against the current session,
    /// then stream its response back via [`Self::pump_session_events`].
    async fn run_chat_turn(
        &self,
        line: &str,
        tx: &UnboundedSender<ReplEvent>,
    ) -> Result<(), EngineError> {
        let session_id = {
            self.session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
        .ok_or(EngineError::NoSession)?;
        self.rpc
            .call(
                "task.run",
                json!({
                    "task_description": line,
                    "session_id": session_id,
                }),
            )
            .await?;
        self.pump_session_events(&session_id, tx).await
    }

    /// Stream `GET /sessions/{session_id}/events` until a terminal event
    /// (`SessionDone`/`SessionCancelled`) is observed, translating every
    /// event into `ReplEvent`s along the way. Reconnects (bounded by
    /// [`SESSION_STREAM_MAX_RECONNECTS`]) on a `502`/`503` status, a
    /// transport error, an idle timeout, or a clean-but-premature stream
    /// close — surfacing each as `ReplEvent::ConnectionLost` first.
    async fn pump_session_events(
        &self,
        session_id: &str,
        tx: &UnboundedSender<ReplEvent>,
    ) -> Result<(), EngineError> {
        let url = format!("{}/sessions/{session_id}/events", self.rpc.base_url());
        let mut attempts = 0u32;
        'reconnect: loop {
            let resp = match self.rpc.http().get(&url).send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    let status = r.status();
                    if is_retryable_status(status) && attempts < SESSION_STREAM_MAX_RECONNECTS {
                        attempts += 1;
                        let _ = tx.send(ReplEvent::ConnectionLost {
                            reason: format!("daemon returned {status}; reconnecting…"),
                        });
                        tokio::time::sleep(RECONNECT_BACKOFF).await;
                        continue 'reconnect;
                    }
                    return Err(EngineError::Status { url, status });
                }
                Err(source) => {
                    if attempts < SESSION_STREAM_MAX_RECONNECTS {
                        attempts += 1;
                        let _ = tx.send(ReplEvent::ConnectionLost {
                            reason: format!("connection failed: {source}; reconnecting…"),
                        });
                        tokio::time::sleep(RECONNECT_BACKOFF).await;
                        continue 'reconnect;
                    }
                    return Err(EngineError::Transport { url, source });
                }
            };
            attempts = 0;
            let mut lines = SseLines::new(resp);
            loop {
                match tokio::time::timeout(SSE_IDLE_TIMEOUT, lines.next_data()).await {
                    Ok(Ok(Some(payload))) => {
                        let Ok(envelope) = serde_json::from_str::<SessionEventEnvelope>(&payload)
                        else {
                            continue;
                        };
                        if forward_session_event(envelope, tx) {
                            return Ok(());
                        }
                    }
                    Ok(Ok(None)) => {
                        if attempts < SESSION_STREAM_MAX_RECONNECTS {
                            attempts += 1;
                            let _ = tx.send(ReplEvent::ConnectionLost {
                                reason: "daemon closed the event stream; reconnecting…".to_string(),
                            });
                            tokio::time::sleep(RECONNECT_BACKOFF).await;
                            continue 'reconnect;
                        }
                        return Ok(());
                    }
                    Ok(Err(source)) => {
                        if attempts < SESSION_STREAM_MAX_RECONNECTS {
                            attempts += 1;
                            let _ = tx.send(ReplEvent::ConnectionLost {
                                reason: format!("stream error: {source}; reconnecting…"),
                            });
                            tokio::time::sleep(RECONNECT_BACKOFF).await;
                            continue 'reconnect;
                        }
                        return Err(EngineError::Transport { url, source });
                    }
                    Err(_elapsed) => {
                        if attempts < SESSION_STREAM_MAX_RECONNECTS {
                            attempts += 1;
                            let _ = tx.send(ReplEvent::ConnectionLost {
                                reason: "no data from daemon within the idle timeout; \
                                         reconnecting…"
                                    .to_string(),
                            });
                            tokio::time::sleep(RECONNECT_BACKOFF).await;
                            continue 'reconnect;
                        }
                        return Err(EngineError::Status {
                            url,
                            status: reqwest::StatusCode::REQUEST_TIMEOUT,
                        });
                    }
                }
            }
        }
    }
}

/// `true` for HTTP statuses worth retrying (daemon restarting) rather than
/// failing immediately.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::BAD_GATEWAY || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
}

/// Translate one [`SessionEventEnvelope`] into zero or more [`ReplEvent`]s
/// on `tx`. Returns `true` iff this event is terminal for the current chat
/// turn (`SessionDone`/`SessionCancelled`) — the caller stops pumping.
///
/// Why: kept as a free function (not a method) so it's directly unit
/// testable against a hand-built envelope, no HTTP/mock server needed.
/// What: `Message`/`AgentMessage`/`PmThinking` -> `AssistantOutput` chunks
/// (`done: false`); `ToolStarted` -> `ToolInvocation{result: None}`;
/// `ToolFinished`/`ToolError` -> `ToolInvocation{result: Some(..)}`, keyed
/// by the SAME `call_id` so the (future) tool-card renderer can pair them;
/// `SessionDone` -> a final `AssistantOutput{done: true}` (`is_error` iff
/// `status == "failed"`); `SessionCancelled` -> a status message. Every
/// other event kind (progress/telemetry/agent-lifecycle events this MVP
/// doesn't yet render) is silently ignored — forward-compatible with new
/// `Event` variants (no `match` arm needed per new kind, thanks to the
/// catch-all).
/// Test: `engine_tests::forward_message_emits_assistant_output_chunk_not_done`,
/// `engine_tests::forward_tool_started_emits_tool_invocation_with_call_id`,
/// `engine_tests::forward_tool_finished_carries_result_and_shares_call_id`,
/// `engine_tests::forward_session_done_is_terminal`,
/// `engine_tests::forward_session_done_failed_marks_is_error`,
/// `engine_tests::forward_session_cancelled_is_terminal`,
/// `engine_tests::forward_unrelated_event_is_ignored`.
fn forward_session_event(envelope: SessionEventEnvelope, tx: &UnboundedSender<ReplEvent>) -> bool {
    match envelope.event {
        Event::Message { text, .. }
        | Event::AgentMessage { text, .. }
        | Event::PmThinking { text, .. } => {
            let _ = tx.send(ReplEvent::AssistantOutput {
                chunk: text,
                done: false,
                is_error: false,
            });
            false
        }
        Event::ToolStarted {
            tool,
            call_id,
            args_preview,
            ..
        } => {
            let _ = tx.send(ReplEvent::ToolInvocation {
                id: call_id,
                tool_name: tool,
                args: json!(args_preview),
                result: None,
            });
            false
        }
        Event::ToolFinished {
            tool,
            call_id,
            result_preview,
            success,
            ..
        } => {
            let result = if success {
                result_preview
            } else {
                format!("FAILED: {result_preview}")
            };
            let _ = tx.send(ReplEvent::ToolInvocation {
                id: call_id,
                tool_name: tool,
                args: Value::Null,
                result: Some(result),
            });
            false
        }
        Event::ToolError {
            tool,
            call_id,
            error,
            ..
        } => {
            let _ = tx.send(ReplEvent::ToolInvocation {
                id: call_id,
                tool_name: tool,
                args: Value::Null,
                result: Some(format!("ERROR: {error}")),
            });
            false
        }
        Event::SessionDone { status, .. } => {
            let _ = tx.send(ReplEvent::AssistantOutput {
                chunk: String::new(),
                done: true,
                is_error: status == "failed",
            });
            true
        }
        Event::SessionCancelled { .. } => {
            let _ = tx.send(ReplEvent::StatusMessage("cancelled".to_string()));
            true
        }
        _ => false,
    }
}

/// The AC-7.2 wire envelope this client deserialises off
/// `GET /workstreams/{id}/events` — mirrors
/// `crate::workstreams::sse::WorkstreamEventEnvelope`
/// (`trusty_agents_common::transport::EventEnvelope<Event>`) exactly, but
/// with `Deserialize` (the shared type only derives `Serialize` — it's
/// built server-side, never parsed) — see module docs for why a small
/// mirror struct here is preferable to adding `Deserialize` to the shared
/// type for one client.
#[derive(Debug, serde::Deserialize)]
struct WireWorkstreamEnvelope {
    #[allow(dead_code)]
    session_id: String,
    #[allow(dead_code)]
    event_type: String,
    payload: Event,
}

fn parse_workstream_envelope(payload_json: &str) -> Option<WireWorkstreamEnvelope> {
    serde_json::from_str(payload_json).ok()
}

/// Whether `rest` (already trimmed) names the `/workstream`/`/ws`
/// engine-routed command, and if so, what follows it (empty string if bare).
///
/// Why: kept pure (no `&self`) so slash-command recognition is unit
/// testable without constructing an engine. Requires a word boundary after
/// the command name (a bare `strip_prefix` would wrongly match
/// `"/workstreamx"`).
fn workstream_subcommand(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    for prefix in ["/workstream", "/ws"] {
        if trimmed == prefix {
            return Some("");
        }
        if let Some(rest) = trimmed.strip_prefix(prefix)
            && let Some(rest) = rest.strip_prefix(' ')
        {
            return Some(rest.trim());
        }
    }
    None
}

/// Long-lived background loop for `subscribe_workstream_events`: holds one
/// SSE connection to `GET /workstreams/{current_id}/events` at a time,
/// reconnecting (to a possibly-NEW workstream id, per DOC-48 §5.3 point 5)
/// on activation changes, transport errors, or idle timeouts, until
/// `state.shutting_down` is set.
async fn run_workstream_subscription(
    state: Arc<EngineState>,
    mut current_id: String,
    tx: UnboundedSender<ReplEvent>,
) {
    loop {
        if state.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let url = format!("{}/workstreams/{current_id}/events", state.rpc.base_url());
        let resp = match state.rpc.http().get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => {
                let _ = tx.send(ReplEvent::ConnectionLost {
                    reason: format!(
                        "could not reach workstream events endpoint for {current_id}; retrying…"
                    ),
                });
                tokio::time::sleep(RECONNECT_BACKOFF).await;
                continue;
            }
        };

        let mut lines = SseLines::new(resp);
        loop {
            if state.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            match tokio::time::timeout(SSE_IDLE_TIMEOUT, lines.next_data()).await {
                Ok(Ok(Some(payload))) => {
                    let Some(env) = parse_workstream_envelope(&payload) else {
                        continue;
                    };
                    if let Event::WorkstreamActivationChanged {
                        new_active_id: Some(new_id),
                        prior_id,
                    } = env.payload
                    {
                        let _ = tx.send(ReplEvent::WorkstreamActivationChanged {
                            new_active_id: new_id.clone(),
                            prior_id,
                        });
                        state.refresh_workstream_cache().await;
                        if new_id != current_id {
                            current_id = new_id;
                            break; // reconnect to the newly-active workstream's endpoint
                        }
                    }
                }
                Ok(Ok(None)) => {
                    let _ = tx.send(ReplEvent::ConnectionLost {
                        reason: "workstream event stream closed; reconnecting…".to_string(),
                    });
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                    break;
                }
                Ok(Err(_)) | Err(_) => {
                    let _ = tx.send(ReplEvent::ConnectionLost {
                        reason: "workstream event stream error; reconnecting…".to_string(),
                    });
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                    break;
                }
            }
        }
    }
}

fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// The `trusty_tui::TuiEngine` adapter for `tcode tui` — see module docs.
pub struct CodeEngine {
    state: Arc<EngineState>,
}

impl CodeEngine {
    /// Discover a running `tcode serve --http` daemon (per
    /// `crate::tui_client::discovery`'s priority) and build a `CodeEngine`
    /// targeting it. `project_path` is forwarded to `session.create` in
    /// `setup()` (mirrors `session::protocol::create`'s `project` param —
    /// `None` is a fully valid, projectless session).
    pub async fn discover(project_path: Option<PathBuf>) -> Result<Self, EngineError> {
        let http = build_http_client();
        let base_url = discover_daemon_url(&http).await?;
        Ok(Self::with_daemon_url(http, base_url, project_path))
    }

    /// Build a `CodeEngine` targeting an explicit daemon URL, bypassing
    /// discovery — the constructor every test in `tests/tui_client_engine.rs`
    /// uses (against a `wiremock`-mocked daemon).
    pub fn with_daemon_url(
        http: reqwest::Client,
        base_url: impl Into<String>,
        project_path: Option<PathBuf>,
    ) -> Self {
        Self {
            state: Arc::new(EngineState {
                rpc: RpcHttpClient::new(http, base_url.into()),
                project_path,
                session_id: Mutex::new(None),
                active_workstream: Mutex::new(None),
                commands_cache: Mutex::new(Vec::new()),
                picker_cache: Mutex::new(HashMap::new()),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    /// The daemon URL this engine targets (test/debug convenience).
    pub fn daemon_url(&self) -> &str {
        self.state.rpc.base_url()
    }

    /// Engine-routed slash commands this MVP supports (ahead of
    /// `trusty-tui` Slice 1.5's synchronous `TuiEngine::commands()`, #3428 —
    /// see module docs). Returns the cache populated during `setup()`;
    /// **move this into the `impl TuiEngine for CodeEngine` block once this
    /// crate rebases onto the trait revision that declares `commands()`.**
    pub fn commands(&self) -> Vec<CommandDescriptor> {
        self.state.commands()
    }

    /// Picker items for `name` (ahead of `trusty-tui` Slice 1.5's
    /// synchronous `TuiEngine::picker(name)`, #3428 — see module docs).
    /// Returns the cache populated during `setup()`/refreshed on workstream
    /// changes; `[]` for an unknown picker name. **Move this into the
    /// `impl TuiEngine for CodeEngine` block once this crate rebases onto
    /// the trait revision that declares `picker()`.**
    pub fn picker(&self, name: &str) -> Vec<PickerItem> {
        self.state.picker(name)
    }
}

#[async_trait::async_trait]
impl TuiEngine for CodeEngine {
    async fn handle_input(
        &self,
        line: String,
        tx: UnboundedSender<ReplEvent>,
    ) -> anyhow::Result<bool> {
        if let Some(rest) = workstream_subcommand(&line) {
            self.state.handle_workstream_command(rest, &tx).await?;
            return Ok(true);
        }
        self.state.run_chat_turn(line.trim(), &tx).await?;
        Ok(true)
    }

    async fn setup(&self, tx: UnboundedSender<ReplEvent>) -> anyhow::Result<()> {
        let result = self
            .state
            .rpc
            .call(
                "session.create",
                json!({
                    "task": "tcode tui session",
                    "project": self.state.project_path,
                }),
            )
            .await?;
        let session_id = result
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| EngineError::Malformed("session.create: response missing `id`".into()))?
            .to_string();
        *self
            .state
            .session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(session_id.clone());

        // Ahead-of-Slice-1.5 `commands()` cache — see struct docs. The one
        // engine-routed command this MVP supports.
        *self
            .state
            .commands_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = vec![CommandDescriptor {
            name: "workstream".to_string(),
            summary: "List or activate the daemon's workstreams".to_string(),
        }];

        if let Some(ws) = self.state.refresh_workstream_cache().await {
            let _ = tx.send(ReplEvent::WorkstreamUpdated(ws));
        }

        let _ = tx.send(ReplEvent::StatusMessage(format!(
            "connected to tcode daemon at {} (session {session_id})",
            self.state.rpc.base_url(),
        )));
        Ok(())
    }

    async fn cancel_session(&self) -> anyhow::Result<()> {
        let session_id = {
            self.state
                .session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        };
        let Some(session_id) = session_id else {
            return Ok(());
        };
        // Thin-client axiom (DOC-39 §2.1 C-2): the daemon performs the real
        // cancellation via `session.cancel` — this call is not optional
        // client-side render-stop.
        self.state
            .rpc
            .call("session.cancel", json!({ "session_id": session_id }))
            .await?;
        Ok(())
    }

    async fn subscribe_workstream_events(
        &self,
        tx: UnboundedSender<ReplEvent>,
    ) -> anyhow::Result<()> {
        let current_id = {
            self.state
                .active_workstream
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
        .map(|ws| ws.id);
        // No workstream is known yet: DOC-48 §5.3's SSE fan-out is scoped
        // PER workstream id (there is no daemon-wide "tell me about any
        // future activation" feed), so there is genuinely nothing to
        // subscribe to until at least one workstream id is known. See this
        // crate's PR description for this as a reported daemon-API gap
        // rather than a client-side workaround — matches the default no-op
        // `TuiEngine::subscribe_workstream_events` contract for engines with
        // no push transport available right now.
        let Some(current_id) = current_id else {
            return Ok(());
        };
        tokio::spawn(run_workstream_subscription(
            self.state.clone(),
            current_id,
            tx,
        ));
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.state.shutting_down.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn envelope(event: Event) -> SessionEventEnvelope {
        SessionEventEnvelope::new("s-1".to_string(), 1, chrono::Utc::now(), event)
    }

    #[test]
    fn forward_message_emits_assistant_output_chunk_not_done() {
        let (tx, mut rx) = unbounded_channel();
        let terminal = forward_session_event(
            envelope(Event::Message {
                session_id: "s-1".into(),
                text: "hi".into(),
            }),
            &tx,
        );
        assert!(!terminal);
        assert_eq!(
            rx.try_recv().expect("event"),
            ReplEvent::AssistantOutput {
                chunk: "hi".into(),
                done: false,
                is_error: false,
            }
        );
    }

    #[test]
    fn forward_tool_started_emits_tool_invocation_with_call_id() {
        let (tx, mut rx) = unbounded_channel();
        let terminal = forward_session_event(
            envelope(Event::ToolStarted {
                session_id: "s-1".into(),
                agent: "pm".into(),
                agent_id: String::new(),
                tool: "bash".into(),
                call_id: "call-1".into(),
                args_preview: "ls".into(),
            }),
            &tx,
        );
        assert!(!terminal);
        match rx.try_recv().expect("event") {
            ReplEvent::ToolInvocation {
                id,
                tool_name,
                result,
                ..
            } => {
                assert_eq!(id, "call-1");
                assert_eq!(tool_name, "bash");
                assert!(result.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn forward_tool_finished_carries_result_and_shares_call_id() {
        let (tx, mut rx) = unbounded_channel();
        forward_session_event(
            envelope(Event::ToolFinished {
                session_id: "s-1".into(),
                agent: "pm".into(),
                agent_id: String::new(),
                tool: "bash".into(),
                call_id: "call-1".into(),
                success: true,
                result_preview: "done".into(),
            }),
            &tx,
        );
        match rx.try_recv().expect("event") {
            ReplEvent::ToolInvocation { id, result, .. } => {
                assert_eq!(id, "call-1");
                assert_eq!(result.as_deref(), Some("done"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn forward_session_done_is_terminal() {
        let (tx, mut rx) = unbounded_channel();
        let terminal = forward_session_event(
            envelope(Event::SessionDone {
                session_id: "s-1".into(),
                status: "finished".into(),
            }),
            &tx,
        );
        assert!(terminal);
        assert_eq!(
            rx.try_recv().expect("event"),
            ReplEvent::AssistantOutput {
                chunk: String::new(),
                done: true,
                is_error: false,
            }
        );
    }

    #[test]
    fn forward_session_done_failed_marks_is_error() {
        let (tx, mut rx) = unbounded_channel();
        forward_session_event(
            envelope(Event::SessionDone {
                session_id: "s-1".into(),
                status: "failed".into(),
            }),
            &tx,
        );
        assert_eq!(
            rx.try_recv().expect("event"),
            ReplEvent::AssistantOutput {
                chunk: String::new(),
                done: true,
                is_error: true,
            }
        );
    }

    #[test]
    fn forward_session_cancelled_is_terminal() {
        let (tx, mut rx) = unbounded_channel();
        let terminal = forward_session_event(
            envelope(Event::SessionCancelled {
                session_id: "s-1".into(),
            }),
            &tx,
        );
        assert!(terminal);
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn forward_unrelated_event_is_ignored() {
        let (tx, mut rx) = unbounded_channel();
        let terminal = forward_session_event(envelope(Event::Ping), &tx);
        assert!(!terminal);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn workstream_subcommand_parses_list_and_activate() {
        assert_eq!(workstream_subcommand("/workstream list"), Some("list"));
        assert_eq!(
            workstream_subcommand("/ws activate abc-123"),
            Some("activate abc-123")
        );
        assert_eq!(workstream_subcommand("/workstream"), Some(""));
        assert_eq!(workstream_subcommand("/workstreamx foo"), None);
        assert_eq!(workstream_subcommand("hello"), None);
    }

    #[test]
    fn parse_workstream_envelope_round_trips_activation_changed() {
        let json = serde_json::json!({
            "session_id": "",
            "event_type": "workstream_activation_changed",
            "payload": {
                "type": "workstream_activation_changed",
                "new_active_id": "ws-2",
                "prior_id": "ws-1",
            },
        })
        .to_string();
        let env = parse_workstream_envelope(&json).expect("parse");
        match env.payload {
            Event::WorkstreamActivationChanged {
                new_active_id,
                prior_id,
            } => {
                assert_eq!(new_active_id.as_deref(), Some("ws-2"));
                assert_eq!(prior_id.as_deref(), Some("ws-1"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn is_retryable_status_covers_502_and_503_only() {
        assert!(is_retryable_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(!is_retryable_status(reqwest::StatusCode::NOT_FOUND));
    }

    /// The caches must start empty before `setup()` populates them —
    /// `commands()`/`picker()` must degrade to "nothing yet," never panic,
    /// when queried before `setup()` runs.
    #[test]
    fn caches_are_empty_before_setup() {
        let engine =
            CodeEngine::with_daemon_url(reqwest::Client::new(), "http://127.0.0.1:1", None);
        assert!(engine.commands().is_empty());
        assert!(engine.picker("workstream").is_empty());
    }
}
