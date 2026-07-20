//! [`EngineState`]: every piece of daemon-observed state [`super::CodeEngine`]
//! caches, shared (via `Arc`) between the foreground `TuiEngine` calls and
//! the background workstream-subscription task (issue #3415).
//!
//! Why: split out of `engine.rs` (issue #610's 500-SLOC production-file
//! cap) — this struct and its methods are the bulk of `CodeEngine`'s real
//! logic (session lifecycle, the commands/picker caches, the chat-turn SSE
//! pump), so they earn their own file; `engine.rs` keeps the public
//! `CodeEngine` wrapper and the `TuiEngine` impl that delegates into this.
//! What: [`EngineState`] itself, and every method DOC-50's design puts on
//! the engine adapter: `commands`/`picker` (ahead of `trusty-tui` Slice
//! 1.5's synchronous accessors, #3428 — see [`EngineState::commands`]'s
//! docs for why these caches use `std::sync::Mutex`, not
//! `tokio::sync::Mutex`), `refresh_workstream_cache` (re-fetches
//! `workstream.list`), `handle_workstream_command` (`/workstream`/`/ws`
//! subcommand routing), `run_chat_turn` + `pump_session_events` (the
//! `task.run` -> `GET /sessions/{id}/events` streaming path).
//! Test: `engine_tests::*` (in the sibling `engine_tests.rs`, included from
//! `engine.rs`) for the pure helpers this module calls into
//! (`session_events::forward_session_event`); the full
//! setup/stream/cancel/workstream flow against a mock HTTP daemon lives in
//! `tests/tui_client_engine.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use trusty_tui::{CommandDescriptor, PickerItem, ReplEvent, WorkstreamSummary};

use crate::events::SessionEventEnvelope;

use super::engine::{RECONNECT_BACKOFF, SESSION_STREAM_MAX_RECONNECTS, SSE_IDLE_TIMEOUT};
use super::error::EngineError;
use super::rpc::RpcHttpClient;
use super::session_events::{forward_session_event, is_retryable_status};
use super::sse::SseLines;

/// See module docs.
pub(super) struct EngineState {
    pub(super) rpc: RpcHttpClient,
    /// Project root to bind the session to, if any (mirrors `session.create`'s
    /// `project` param — see `crate::session::protocol::create`'s docs).
    pub(super) project_path: Option<PathBuf>,
    pub(super) session_id: Mutex<Option<String>>,
    pub(super) active_workstream: Mutex<Option<WorkstreamSummary>>,
    /// Ahead-of-Slice-1.5 cache for `TuiEngine::commands()` (#3428) — see
    /// module docs. Populated once, in `setup()`; static for this MVP (the
    /// one engine-routed command, `/workstream`, never changes at runtime).
    pub(super) commands_cache: Mutex<Vec<CommandDescriptor>>,
    /// Ahead-of-Slice-1.5 cache for `TuiEngine::picker(name)` (#3428) — see
    /// module docs. Keyed by picker name (matches the DOC-50-noted
    /// convention that a command name doubles as its picker name, e.g.
    /// `/workstream` <-> `picker("workstream")`). Refreshed in `setup()`,
    /// after a successful `/workstream activate`, and whenever the
    /// background subscription observes a `WorkstreamActivationChanged`
    /// event — see [`EngineState::refresh_workstream_cache`].
    pub(super) picker_cache: Mutex<HashMap<String, Vec<PickerItem>>>,
    pub(super) shutting_down: AtomicBool,
}

impl EngineState {
    pub(super) fn new(rpc: RpcHttpClient, project_path: Option<PathBuf>) -> Self {
        Self {
            rpc,
            project_path,
            session_id: Mutex::new(None),
            active_workstream: Mutex::new(None),
            commands_cache: Mutex::new(Vec::new()),
            picker_cache: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// `std::sync::Mutex`, not `tokio::sync::Mutex` — see module docs:
    /// `commands()`/`picker()` are, per #3428, plain synchronous `fn`s (no
    /// `.await` available), so the cache they read MUST be lockable without
    /// an async runtime. Every lock here is held only long enough to clone a
    /// small `Vec`/`Option` — never across an `.await` point.
    pub(super) fn commands(&self) -> Vec<CommandDescriptor> {
        self.commands_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(super) fn picker(&self, name: &str) -> Vec<PickerItem> {
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
    pub(super) async fn refresh_workstream_cache(&self) -> Option<WorkstreamSummary> {
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
    pub(super) async fn handle_workstream_command(
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
    pub(super) async fn run_chat_turn(
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
    pub(super) async fn pump_session_events(
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
