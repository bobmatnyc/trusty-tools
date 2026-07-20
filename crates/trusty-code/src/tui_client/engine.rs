//! [`CodeEngine`]: the `trusty_tui::TuiEngine` adapter driving a long-lived
//! `tcode serve --http` daemon (issue #3415, DOC-50 §3.3/§3.4).
//!
//! Why: see `crate::tui_client`'s module docs for the ephemeral-`--stdio`
//! vs. long-lived-`--http` client distinction. This module is the `TuiEngine`
//! impl itself — everything else in `tui_client` (`discovery`, `rpc`, `sse`)
//! exists to support it. Split across sibling files (issue #610's 500-SLOC
//! production-file cap): `engine_state.rs` holds [`super::engine_state::EngineState`]
//! (the actual session/cache/streaming logic), `session_events.rs` holds
//! the pure `Event` -> `ReplEvent` mapping, `workstream_subscription.rs`
//! holds the background workstream-activation SSE loop. This file keeps
//! only the public `CodeEngine` wrapper and the `TuiEngine` impl that
//! delegates into `EngineState`.
//! What: [`CodeEngine`] is a thin `Arc<EngineState>` wrapper so the
//! background workstream-subscription task spawned by
//! `subscribe_workstream_events` can share the SAME state (and refresh the
//! SAME caches) without a second, divergent copy. `trusty-tui` Slice 1.5
//! (#3428, merged) added the SYNCHRONOUS `TuiEngine::commands()`/
//! `picker(name)` accessors this `impl TuiEngine` block implements directly
//! (not as inherent methods — see the `impl TuiEngine for CodeEngine`
//! block's `commands`/`picker` for why serving from `EngineState`'s
//! `std::sync::Mutex`-guarded caches, populated during `setup()`, is the
//! only way to satisfy a synchronous trait method with no I/O on the
//! calling path).
//! Test: `engine_tests::*` (in the sibling `engine_tests.rs`, included
//! below) for the pure event-mapping/parsing helpers this module's siblings
//! define; the full discover -> setup -> stream -> cancel ->
//! workstream-activation flow against a mock HTTP daemon lives in
//! `tests/tui_client_engine.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use trusty_tui::{CommandDescriptor, CommandRouting, PickerRequest, ReplEvent, TuiEngine};

use super::discovery::discover_daemon_url;
use super::engine_state::EngineState;
use super::error::EngineError;
use super::rpc::RpcHttpClient;
use super::workstream_subscription::run_workstream_subscription;

/// How long a session-event / workstream-event SSE read may sit idle (no
/// bytes, not even a keep-alive comment) before this client treats the
/// connection as dead and reconnects. Axum's default `KeepAlive` sends a
/// comment roughly every 15s (`crate::serve::http::session_events_sse`'s
/// `KeepAlive::default()`), so three missed beats is a generous margin
/// before declaring the daemon unreachable. Shared with `engine_state.rs`
/// and `workstream_subscription.rs`.
pub(super) const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(45);

/// Fixed backoff between SSE reconnect attempts. Not exponential — MVP
/// scope (DOC-50 §5 Slice 3); a persistently-down daemon retries at a
/// steady, human-visible cadence rather than hot-looping. Shared with
/// `engine_state.rs` and `workstream_subscription.rs`.
pub(super) const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);

/// How many times `EngineState::pump_session_events` reconnects before
/// giving up and returning control to the caller (`handle_input` must
/// eventually return so the REPL's input loop stays responsive — unlike the
/// workstream-activation subscription, which is meant to run for the whole
/// TUI session). Shared with `engine_state.rs`.
pub(super) const SESSION_STREAM_MAX_RECONNECTS: u32 = 5;

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
            state: Arc::new(EngineState::new(
                RpcHttpClient::new(http, base_url.into()),
                project_path,
            )),
        }
    }

    /// The daemon URL this engine targets (test/debug convenience).
    pub fn daemon_url(&self) -> &str {
        self.state.rpc.base_url()
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

        // `TuiEngine::commands()` cache — see `EngineState`'s struct docs.
        // The one engine-routed command this MVP supports.
        *self
            .state
            .commands_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = vec![CommandDescriptor {
            name: "workstream".to_string(),
            summary: "List or activate the daemon's workstreams".to_string(),
            routing: CommandRouting::Engine,
            args_hint: Some("[list | activate <id>]".to_string()),
        }];

        if let Some(ws) = self.state.refresh_workstream_cache().await {
            let _ = tx.send(ReplEvent::WorkstreamUpdated(ws));
            // Populate the status line's Workstream segment on first render
            // (DOC-50 §5.3, Slice 6) — without this, the TUI would show a
            // blank statusline until the background SSE subscription (which
            // only starts after `setup` returns, see `run.rs::run`) happens
            // to observe an activation change.
            let _ = tx.send(ReplEvent::StatuslineUpdate(
                self.state.statusline_segments(),
            ));
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

    /// Engine-routed slash commands this MVP supports — served from the
    /// cache `setup()` populates (`EngineState::commands_cache`). This is a
    /// SYNCHRONOUS trait method (#3428) — the cache exists precisely so
    /// this never needs to perform network I/O on the calling path; see
    /// `EngineState::commands`'s docs for the `std::sync::Mutex` rationale.
    fn commands(&self) -> Vec<CommandDescriptor> {
        self.state.commands()
    }

    /// The `"workstream"` picker — served from the cache
    /// `EngineState::refresh_workstream_cache` populates (in `setup()`,
    /// after `/workstream activate`, and on every observed
    /// `WorkstreamActivationChanged`). `None` for any other picker name
    /// (this engine has exactly one) or before the cache has been
    /// populated at least once. `dispatch_command` is `"/workstream
    /// activate"` — matching `workstream_subcommand`'s parsing exactly, so
    /// the shared event loop's `"{dispatch_command} {selected.id}"`
    /// resubmission (`"/workstream activate <id>"`) round-trips through
    /// `handle_input` correctly.
    fn picker(&self, name: &str) -> Option<PickerRequest> {
        if name != "workstream" {
            return None;
        }
        let items = self.state.picker_items("workstream")?;
        Some(PickerRequest {
            title: "Workstreams".to_string(),
            items,
            dispatch_command: "/workstream activate".to_string(),
        })
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod engine_tests;
