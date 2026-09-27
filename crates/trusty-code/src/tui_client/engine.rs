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
//! holds the background workstream-activation SSE loop, `splash.rs` holds
//! the startup splash's pure text assembly (#8164). This file keeps
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
    CancelReply, CommandDescriptor, CommandRouting, PermissionAnswer, PickerRequest, ReplEvent,
    TuiEngine,
};

use crate::permissions::{PERMISSION_RESPOND_METHOD, PermissionDecision};

use super::engine_state::EngineState;
use super::error::EngineError;
use super::splash::{SplashFacts, connect_line, splash_lines};
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

/// The words for whether this session runs the solo agent or the delegating
/// PM (#8184), shared by [`session_shape_summary`] and the connect line so
/// the two can never disagree.
///
/// A missing or `false` `no_delegate` reads as the delegating PM, matching
/// `Session::no_delegate`'s own `#[serde(default)]`.
/// Test: `engine_tests::session_shape_summary_names_the_solo_agent_and_root`,
/// `engine_tests::session_shape_summary_names_the_delegating_pm`.
fn agent_label(session: &Value) -> &'static str {
    if session
        .get("no_delegate")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "solo agent (no delegation)"
    } else {
        "delegating PM"
    }
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
    let agent = agent_label(session);
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

    /// Ask the daemon to cancel this session's run and report WHICH of the two
    /// cancel outcomes came back (#8207).
    ///
    /// Why: the daemon answers a confirmed stop and an unconfirmed one with two
    /// different things — a session snapshot and `-32010 cancel_unconfirmed` —
    /// but [`TuiEngine::cancel_session`] returns `anyhow::Result<()>`, which
    /// flattens the second into an opaque error string. That made the domain code
    /// unreadable from the only client that reaches it, which is what the code was
    /// minted to prevent. This is the typed half; the trait method below adapts it
    /// to the signature `trusty-code-tui` owns, and the TUI's own
    /// "still cancelling…" render is a separate change in that crate.
    /// What: `-32010` becomes [`CancelOutcome::StillCancelling`] carrying the
    /// daemon's own sentence. Every other RPC or transport failure stays an error,
    /// and no session at all is [`CancelOutcome::NoSession`] — there was nothing
    /// to stop.
    ///
    /// # Errors
    ///
    /// [`EngineError::Rpc`] for any daemon refusal other than `-32010`, and
    /// [`EngineError::Transport`] for a socket failure.
    /// Test: `engine_tests::cancel_unconfirmed_is_a_still_cancelling_outcome`,
    /// `engine_tests::any_other_refusal_stays_an_error`.
    pub async fn cancel_session_outcome(&self) -> Result<CancelOutcome, EngineError> {
        let session_id = {
            self.state
                .session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        };
        let Some(session_id) = session_id else {
            return Ok(CancelOutcome::NoSession);
        };
        // Thin-client axiom (DOC-39 §2.1 C-2): the daemon performs the real
        // cancellation via `session.cancel` — this call is not optional
        // client-side render-stop.
        match self
            .state
            .rpc
            .call("session.cancel", json!({ "session_id": session_id }))
            .await
        {
            Ok(_) => Ok(CancelOutcome::Stopped),
            Err(err) => Ok(classify_cancel_error(err)?),
        }
    }
}

/// What a `session.cancel` achieved (#8207).
///
/// Why: "the run stopped" and "the run has not stopped yet" are both successful
/// answers to the question the cancel asked, and the TUI has to render them
/// differently — one ends the turn, the other keeps a cancelling state on screen.
/// Collapsing the second into an error is what made `-32010` unreadable.
/// Test: `engine_tests::cancel_unconfirmed_is_a_still_cancelling_outcome`.
#[derive(Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The daemon confirmed the run has terminated.
    Stopped,
    /// The cancel was accepted; the run had not stopped within the daemon's
    /// grace. `detail` is the daemon's own sentence, safe to show verbatim.
    StillCancelling {
        /// The daemon's account of what it is still waiting on.
        detail: String,
    },
    /// `setup` has not minted a session yet, so there was nothing to cancel.
    NoSession,
}

/// The JSON-RPC error code `RpcError::cancel_unconfirmed` carries (#8207) — the
/// daemon-side constant lives in `crate::jsonrpc::error`, which does not export
/// it as a named code.
const CODE_CANCEL_UNCONFIRMED: i32 = -32010;

/// Split a failed `session.cancel` into "still cancelling" and a real failure
/// (#8207).
///
/// Why: exactly one code is not a failure, and keeping the test for it in one
/// named function is what lets a unit test assert the split without a daemon.
/// Test: `engine_tests::cancel_unconfirmed_is_a_still_cancelling_outcome`,
/// `engine_tests::any_other_refusal_stays_an_error`.
fn classify_cancel_error(err: EngineError) -> Result<CancelOutcome, EngineError> {
    match err {
        EngineError::Rpc { code, message, .. } if code == CODE_CANCEL_UNCONFIRMED => {
            Ok(CancelOutcome::StillCancelling { detail: message })
        }
        other => Err(other),
    }
}

/// Project a `session.cancel` result onto the TUI's [`CancelReply`] (#8207).
///
/// Why: this is where the wire vocabulary has to stop. [`CancelReply`]'s
/// payloads are rendered to the user verbatim, and [`EngineError::Rpc`]'s own
/// `Display` embeds the JSON-RPC code — the `daemon returned an error (-32003)`
/// shape #8207 was filed about. A refusal is therefore reduced to the daemon's
/// `message` HERE, where the code is still a separate field, rather than
/// anywhere downstream where only a flattened string survives. Split out of
/// [`TuiEngine::cancel_session_reply`] so all three arms are testable without a
/// daemon socket, the same reason [`classify_cancel_error`] is its own function.
/// What: [`CancelOutcome::NoSession`] reports `Stopped` — `setup` never minted a
/// session, so nothing is running and input should reopen. A transport failure
/// carries no code and is reported as written.
/// Test: `engine_tests::cancel_reply_reports_a_confirmed_stop`,
/// `engine_tests::cancel_reply_keeps_an_unconfirmed_cancel_distinct`,
/// `engine_tests::cancel_reply_strips_the_rpc_code_from_a_refusal`.
fn cancel_reply_from(outcome: Result<CancelOutcome, EngineError>) -> CancelReply {
    match outcome {
        Ok(CancelOutcome::Stopped | CancelOutcome::NoSession) => CancelReply::Stopped,
        Ok(CancelOutcome::StillCancelling { detail }) => CancelReply::StillCancelling { detail },
        Err(EngineError::Rpc { message, .. }) => CancelReply::Failed { error: message },
        Err(other) => CancelReply::Failed {
            error: other.to_string(),
        },
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

        // #8184: the daemon only prompts while someone is watching this
        // session; claim that BEFORE any run can issue a gated tool call.
        super::prompter_claim::hold_prompter_claim(self.state.clone(), session_id.clone()).await?;

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

        let shape = session_shape_summary(&result);

        // #8164: the launch facts, as a splash that REPLACES the banner's
        // generic identity row. `health` is best-effort — a daemon that
        // cannot answer it still ran `session.create`, so a splash missing
        // the daemon's build beats no splash at all.
        let daemon = self.state.rpc.call("health", json!({})).await.ok();
        let workstream = self
            .state
            .active_workstream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|ws| format!("{} ({})", ws.name, ws.id));
        let project = self
            .state
            .project_path
            .as_ref()
            .map(|p| p.display().to_string());
        let _ = tx.send(ReplEvent::SplashUpdated(splash_lines(&SplashFacts {
            client_version: crate::build_info::LONG_VERSION,
            client_build: crate::build_info::GIT_HASH,
            daemon_version: daemon.as_ref().and_then(|d| d["version"].as_str()),
            daemon_build: daemon.as_ref().and_then(|d| d["build"].as_str()),
            socket: self.state.rpc.socket(),
            project: project.as_deref(),
            workstream: workstream.as_deref(),
            shape: &shape,
        })));

        // #8164: one scrollback line naming the home directory the session
        // actually got (the DAEMON's binding, not this client's request), the
        // workstream, and the agent shape — and no session id (owner rule).
        let _ = tx.send(ReplEvent::StatusMessage(connect_line(
            self.state.rpc.socket(),
            result
                .get("binding")
                .and_then(|b| b.get("root"))
                .and_then(Value::as_str),
            workstream.as_deref(),
            agent_label(&result),
        )));
        Ok(())
    }

    /// #8207: the trait's `Result<()>` cannot carry the two-way outcome, so this
    /// adapts [`CodeEngine::cancel_session_outcome`] to it. An unconfirmed cancel
    /// stays an error — a cancel that did not land must not read as one that did —
    /// but with the daemon's plain sentence rather than an opaque `-32010` dump.
    /// The TUI reaches [`Self::cancel_session_reply`] instead, which keeps the
    /// distinction; this one-state form remains for a caller that only needs to
    /// know whether the request was delivered.
    async fn cancel_session(&self) -> anyhow::Result<()> {
        match self.cancel_session_outcome().await? {
            CancelOutcome::Stopped | CancelOutcome::NoSession => Ok(()),
            CancelOutcome::StillCancelling { detail } => {
                Err(anyhow::anyhow!("still cancelling: {detail}"))
            }
        }
    }

    /// #8207: project [`CodeEngine::cancel_session_outcome`] onto the TUI's own
    /// [`CancelReply`], which is the shape the TUI can render as three distinct
    /// states instead of "cancelled or not".
    ///
    /// Why: this is the crate boundary where the wire vocabulary has to stop.
    /// [`CancelReply`]'s payloads are shown to the user verbatim, and
    /// [`EngineError::Rpc`]'s own `Display` embeds the JSON-RPC code — which is
    /// exactly the `daemon returned an error (-32003)` shape #8207 was filed
    /// about. So a refusal is reduced to the daemon's `message` here, where the
    /// code is still a separate field, rather than anywhere downstream where
    /// only a flattened string survives.
    /// What: the projection itself is [`cancel_reply_from`], so every arm is
    /// unit-testable without a daemon socket.
    /// Test: `engine_tests::cancel_reply_reports_a_confirmed_stop`,
    /// `engine_tests::cancel_reply_keeps_an_unconfirmed_cancel_distinct`,
    /// `engine_tests::cancel_reply_strips_the_rpc_code_from_a_refusal`.
    async fn cancel_session_reply(&self) -> CancelReply {
        cancel_reply_from(self.cancel_session_outcome().await)
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
