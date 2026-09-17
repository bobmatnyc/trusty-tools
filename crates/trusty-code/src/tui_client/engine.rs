//! [`CodeEngine`]: the `trusty_code_tui::TuiEngine` adapter driving a long-lived
//! `tcode serve` daemon over its Unix socket (issue #3415, DOC-50 §3.3/§3.4;
//! retransported in #6637).
//!
//! Why: see `crate::tui_client`'s module docs for the ephemeral-`--stdio`
//! vs. long-lived-daemon client distinction. This module is the `TuiEngine`
//! impl itself — `uds_rpc` exists to support it. Split across sibling files
//! (issue #610's 500-SLOC
//! production-file cap): `engine_state.rs` holds [`super::engine_state::EngineState`]
//! (the actual session/cache/streaming logic), `session_events.rs` holds
//! the pure `Event` -> `ReplEvent` mapping, `workstream_subscription.rs`
//! holds the background workstream-activation SSE loop. This file keeps
//! only the public `CodeEngine` wrapper and the `TuiEngine` impl that
//! delegates into `EngineState`.
//! What: [`CodeEngine`] is a thin `Arc<EngineState>` wrapper so the
//! background workstream-subscription task spawned by
//! `subscribe_workstream_events` can share the SAME state (and refresh the
//! SAME caches) without a second, divergent copy. `trusty-code-tui` Slice 1.5
//! (#3428, merged) added the SYNCHRONOUS `TuiEngine::commands()`/
//! `picker(name)` accessors this `impl TuiEngine` block implements directly
//! (not as inherent methods — see the `impl TuiEngine for CodeEngine`
//! block's `commands`/`picker` for why serving from `EngineState`'s
//! `std::sync::Mutex`-guarded caches, populated during `setup()`, is the
//! only way to satisfy a synchronous trait method with no I/O on the
//! calling path).
//! Test: `engine_tests::*` (in the sibling `engine_tests.rs`, included
//! below) for the pure event-mapping/parsing helpers this module's siblings
//! define; the full setup -> stream -> cancel -> workstream-activation flow
//! against a real daemon socket lives in `tests/tui_client_engine.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use trusty_code_tui::{
    CommandDescriptor, CommandRouting, PermissionAnswer, PickerRequest, ReplEvent, TuiEngine,
};

use crate::permissions::{PERMISSION_RESPOND_METHOD, PermissionDecision};

use super::engine_state::EngineState;
use super::error::EngineError;
use super::uds_rpc::UdsRpcClient;
use super::workstream_subscription::run_workstream_subscription;

/// How long [`CodeEngine::connect`] waits for the socket to accept a
/// connection before reporting no daemon. Short and fixed: this runs once, at
/// REPL startup, against a local inode.
const DAEMON_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Fixed backoff between stream reconnect attempts. Not exponential — MVP
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

/// One line naming the agent shape and the working root of the session
/// `session.create` just returned (#8184).
///
/// Why: the two facts that decide what a turn can actually do — whether the
/// agent edits files itself or delegates, and WHERE its file tools are rooted — were
/// invisible from the TUI, so a projectless session (which works in a
/// throwaway scratch directory, never the launch directory) looked exactly
/// like a bound one. Kept pure (no `&self`) so it is unit-testable without a
/// daemon.
/// What: reads `no_delegate` and `binding.root` off the returned `Session`.
/// A missing/false `no_delegate` reads as the delegating PM, matching
/// `Session::no_delegate`'s own `#[serde(default)]`.
/// Test: `engine_tests::session_shape_summary_names_the_solo_agent_and_root`,
/// `engine_tests::session_shape_summary_names_the_projectless_scratch_root`,
/// `engine_tests::session_shape_summary_names_the_delegating_pm`.
fn session_shape_summary(session: &Value) -> String {
    let agent = if session
        .get("no_delegate")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "solo agent (no delegation)"
    } else {
        "delegating PM"
    };
    match session
        .get("binding")
        .and_then(|b| b.get("root"))
        .and_then(Value::as_str)
    {
        // #8184: "file tools rooted at", not "editing in" — `tools::fs` scopes
        // its paths to this root, but `bash` only sets the child's working
        // directory, so this states where the agent works, never a sandbox.
        Some(root) => format!("{agent}, file tools rooted at {root}"),
        None => format!("{agent}, projectless — file tools rooted at a scratch workspace"),
    }
}

/// The `trusty_code_tui::TuiEngine` adapter for `tcode tui` — see module docs.
pub struct CodeEngine {
    state: Arc<EngineState>,
}

impl CodeEngine {
    /// Build a `CodeEngine` dialling the daemon's well-known socket
    /// ([`crate::serve::uds::socket_path`]).
    ///
    /// Why (#6637): this replaces `discover`, which raced a `TCODE_DAEMON_URL`
    /// against an `http_addr` file and liveness-pinged whichever it found. The
    /// socket path is derived from the data directory, so there is one answer
    /// and nothing to choose between. `project_path` is forwarded to
    /// `session.create` in `setup()` (mirrors `session::protocol::create`'s
    /// `project` param — `None` is a fully valid, projectless session).
    ///
    /// # Errors
    ///
    /// [`EngineError::NoDaemon`] when nothing is answering on the socket. This
    /// client does not start one — `tcode tui`'s auto-spawn owns that decision.
    pub async fn connect(project_path: Option<PathBuf>) -> Result<Self, EngineError> {
        let socket = crate::serve::uds::socket_path()
            .map_err(|e| EngineError::Malformed(format!("resolve the daemon socket: {e:#}")))?;
        if !trusty_common::uds::socket_is_serving(&socket, DAEMON_PROBE_TIMEOUT).await {
            return Err(EngineError::NoDaemon { socket });
        }
        Ok(Self::with_socket(socket, project_path))
    }

    /// Build a `CodeEngine` dialling an explicit socket — the constructor
    /// `tcode tui`'s auto-spawn path and every test in
    /// `tests/tui_client_engine.rs` use.
    ///
    /// (#8184) The session this engine creates runs the SOLO agent — see
    /// [`CodeEngine::with_socket_delegating`] for the PM opt-in.
    pub fn with_socket(socket: impl Into<PathBuf>, project_path: Option<PathBuf>) -> Self {
        Self::build(socket, project_path, false)
    }

    /// [`CodeEngine::with_socket`] for a session that runs the DELEGATING PM
    /// — `tcode tui --delegate` (#8184).
    ///
    /// Why: the interactive default is the solo agent, so PM/delegation mode
    /// needs one named surface to be asked for. A second constructor rather
    /// than a third parameter: every other call site wants the default.
    /// What: sends `session.create`'s `delegate: true` from `setup`.
    /// Test: `tui_delegate_opt_in_creates_a_delegating_session` (in
    /// `tests/tui_client_engine.rs`).
    pub fn with_socket_delegating(
        socket: impl Into<PathBuf>,
        project_path: Option<PathBuf>,
    ) -> Self {
        Self::build(socket, project_path, true)
    }

    /// The shared body of both constructors above.
    fn build(socket: impl Into<PathBuf>, project_path: Option<PathBuf>, delegate: bool) -> Self {
        Self {
            state: Arc::new(EngineState::new(
                UdsRpcClient::new(socket),
                project_path,
                delegate,
            )),
        }
    }

    /// The socket this engine dials (test/debug convenience).
    pub fn daemon_socket(&self) -> &std::path::Path {
        self.state.rpc.socket()
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
                    // #8184: an interactive session runs the solo agent —
                    // `false` here IS `session.create`'s own default, sent
                    // explicitly so this client's shape never depends on a
                    // daemon-side default drifting. `--delegate` flips it.
                    "delegate": self.state.delegate,
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

        // #8184: name the agent shape and working root — see
        // `session_shape_summary`.
        let _ = tx.send(ReplEvent::StatusMessage(format!(
            "connected to tcode daemon at {} (session {session_id}; {})",
            self.state.rpc.socket().display(),
            session_shape_summary(&result),
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

    /// Answer one suspended permission request over
    /// `session.permission.respond` (#3422).
    ///
    /// Why: an `ask` rule parks the tool call inside the daemon's gate
    /// (`crate::permissions::gate`), and only the daemon's broker can release
    /// it — same thin-client reasoning as `cancel_session` above (DOC-39
    /// §2.1 C-2, ADR-0063). The TUI decided nothing; it named a button.
    /// What: translates the shared crate's [`PermissionAnswer`] into this
    /// daemon's own [`PermissionDecision`] and sends its wire word, so the
    /// vocabulary has exactly one definition
    /// (`crate::permissions::protocol`). With no session yet (`setup` has
    /// not run) there is no request to answer, so this is a no-op rather
    /// than an error.
    ///
    /// # Errors
    ///
    /// Propagates the RPC failure, including the daemon's `-32602` for a
    /// `request_id` nobody is waiting on — an answer that did not land must
    /// not read as one that did.
    /// Test: `respond_permission_releases_a_suspended_call`,
    /// `respond_permission_without_a_session_is_a_noop` (both in
    /// `tests/tui_client_engine.rs`).
    async fn respond_permission(
        &self,
        request_id: String,
        answer: PermissionAnswer,
    ) -> anyhow::Result<()> {
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
        let (decision, pattern) = match answer {
            PermissionAnswer::AllowOnce => (PermissionDecision::AllowOnce, None),
            PermissionAnswer::AllowForSession { pattern } => (
                PermissionDecision::AllowForSession {
                    pattern: pattern.clone(),
                },
                pattern,
            ),
            PermissionAnswer::Deny => (PermissionDecision::Deny, None),
        };
        self.state
            .rpc
            .call(
                PERMISSION_RESPOND_METHOD,
                json!({
                    "session_id": session_id,
                    "request_id": request_id,
                    "decision": decision.as_str(),
                    "pattern": pattern,
                }),
            )
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
