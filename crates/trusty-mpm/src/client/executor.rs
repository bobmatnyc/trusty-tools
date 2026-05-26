//! The single command executor.
//!
//! Why: before this crate, three UIs each translated operator intent into
//! daemon HTTP calls independently. [`CommandExecutor`] is the *one* place that
//! mapping lives — every UI hands it a [`TrustyCommand`] and gets back a
//! [`CommandResult`]. A new endpoint is wired here once.
//! What: [`CommandExecutor`] owns a [`DaemonClient`] and exposes [`execute`]
//! (command → result) plus [`pair_confirm`] for the chat-id-carrying pairing
//! confirm that does not fit the pure `TrustyCommand → CommandResult` shape.
//! Unreachable-daemon errors become a [`CommandResult::Error`], never a panic.
//! Test: `cargo test -p trusty-mpm-client` covers the pure `/help` path and the
//! HTTP paths against an in-process test daemon.

use super::command::{TrustyCommand, help_text};
use super::http_client::{DaemonClient, SessionRow};

/// Maximum captured-output length returned by `/send` (Telegram's message cap).
///
/// Why: Telegram rejects messages longer than ~4096 characters; truncating the
/// captured pane keeps the reply deliverable on every UI.
pub const MAX_OUTPUT_CHARS: usize = 4000;
use super::result::{
    CommandResult, DecisionCounts, DiscoveredProjectSummary, RecommendationSummary, SessionSummary,
    TmuxSessionSummary,
};

/// Translates [`TrustyCommand`]s into daemon HTTP calls.
///
/// Why: the single seam between UI intent and the daemon API; isolating it here
/// means the Telegram bot, the TUI, and the CLI never embed HTTP logic.
/// What: wraps a [`DaemonClient`]; [`execute`] runs one command end-to-end.
/// Test: `execute_help_returns_help`, `execute_sessions_against_test_daemon`.
pub struct CommandExecutor {
    /// The shared daemon HTTP client.
    client: DaemonClient,
}

impl CommandExecutor {
    /// Build an executor targeting `daemon_url`.
    ///
    /// Why: a UI constructs one executor for the daemon it was pointed at.
    /// What: wraps a fresh [`DaemonClient`] for `daemon_url`.
    /// Test: `execute_help_returns_help`.
    pub fn new(daemon_url: impl Into<String>) -> Self {
        Self {
            client: DaemonClient::new(daemon_url),
        }
    }

    /// The underlying daemon client.
    ///
    /// Why: a UI's alert loop and pairing flow need direct client access
    /// alongside command execution.
    /// What: returns a reference to the wrapped [`DaemonClient`].
    /// Test: covered by the pairing tests.
    pub fn client(&self) -> &DaemonClient {
        &self.client
    }

    /// Execute one [`TrustyCommand`] against the daemon.
    ///
    /// Why: the single dispatch point — every UI funnels intent through here.
    /// What: maps each command to daemon calls and returns a structured
    /// [`CommandResult`]; a transport failure becomes [`CommandResult::Error`].
    /// `Pair { code: Some(_) }` cannot complete here (it needs a chat id) and is
    /// reported as a state query — UIs must call [`Self::pair_confirm`] instead.
    /// Test: `execute_help_returns_help`, `execute_sessions_against_test_daemon`,
    /// `execute_kill_returns_killed`.
    pub async fn execute(&self, cmd: TrustyCommand) -> CommandResult {
        match cmd {
            TrustyCommand::Help => CommandResult::Help(help_text().to_string()),
            TrustyCommand::Alerts => CommandResult::AlertSubscriptions(vec![
                "Categories: Permission, Agent".to_string(),
                "Memory alerts: enabled".to_string(),
            ]),
            TrustyCommand::Sessions => self.sessions().await,
            TrustyCommand::Status { session_id } => self.status(&session_id).await,
            TrustyCommand::Approve { session_id } => self.decide(&session_id, true).await,
            TrustyCommand::Deny { session_id } => self.decide(&session_id, false).await,
            TrustyCommand::Overseer => self.overseer().await,
            TrustyCommand::Tmux => self.tmux().await,
            TrustyCommand::Projects => self.projects().await,
            TrustyCommand::Discover => self.discover().await,
            TrustyCommand::Adopt { session } => self.adopt(&session).await,
            TrustyCommand::Config { project } => self.config(&project).await,
            TrustyCommand::Snapshot { session } => self.snapshot(&session).await,
            TrustyCommand::Kill { session_id } => self.kill(&session_id).await,
            TrustyCommand::Send { session, prompt } => self.send(&session, &prompt).await,
            TrustyCommand::Launch { project, .. } => self.launch(&project).await,
            TrustyCommand::Connect { project, .. } => self.connect(&project).await,
            TrustyCommand::Start => self.pair_state().await,
            TrustyCommand::Doctor => self.doctor().await,
            TrustyCommand::CoordinatorChat { message } => self.coordinator_chat(&message).await,
            TrustyCommand::Pair { code: None } => self.pair_state().await,
            TrustyCommand::Pair { code: Some(_) } => {
                // A code-carrying pair requires the caller's chat id, which is
                // not part of the command; UIs route those to `pair_confirm`.
                self.pair_state().await
            }
        }
    }

    /// Confirm a pairing code on behalf of a specific chat.
    ///
    /// Why: `POST /pair/confirm` needs the confirming chat's id, which is not
    /// carried by [`TrustyCommand::Pair`]; the bot adapter supplies it here.
    /// What: calls the daemon's confirm endpoint and maps the result to
    /// [`CommandResult::PairSuccess`] or [`CommandResult::Error`].
    /// Test: `pair_confirm_unknown_code_errors`.
    pub async fn pair_confirm(&self, code: &str, chat_id: i64) -> CommandResult {
        match self.client.pair_confirm(code, chat_id).await {
            Ok(confirm) if confirm.success => CommandResult::PairSuccess {
                chat_info: format!("chat {}", confirm.chat_id.unwrap_or(chat_id)),
            },
            Ok(confirm) => CommandResult::Error(
                confirm
                    .error
                    .unwrap_or_else(|| "invalid or expired code".to_string()),
            ),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// Request a one-time pairing code from the daemon.
    ///
    /// Why: `tm pair` asks the local daemon for a code to display.
    /// What: calls `POST /pair/request` and maps it to [`CommandResult::PairCode`].
    /// Test: `pair_request_returns_code`.
    pub async fn pair_request(&self) -> CommandResult {
        match self.client.pair_request().await {
            Ok(req) => CommandResult::PairCode {
                code: req.code,
                expires_in_seconds: req.expires_in_seconds,
            },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/sessions` — fetch and summarize the managed session list.
    async fn sessions(&self) -> CommandResult {
        match self.client.sessions().await {
            Ok(rows) => CommandResult::Sessions(
                rows.into_iter()
                    .map(|s| SessionSummary {
                        id: s.id.0.to_string(),
                        status: status_label(s.status),
                        workdir: s.workdir,
                    })
                    .collect(),
            ),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/status` — fetch one session's recent events.
    async fn status(&self, session_id: &str) -> CommandResult {
        match self.client.session_events(session_id).await {
            Ok(events) => {
                let names: Vec<String> = events
                    .iter()
                    .rev()
                    .take(5)
                    .rev()
                    .map(|e| e.event.wire_name().to_string())
                    .collect();
                CommandResult::SessionDetail {
                    id: session_id.to_string(),
                    status: "active".to_string(),
                    events: names,
                }
            }
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/approve` and `/deny` — verify the session, record a synthetic decision.
    ///
    /// Why: both share the same flow — confirm the session is known, then post a
    /// synthetic `PostToolUse` hook carrying `{"approved": bool}` so the
    /// decision is audited.
    /// What: lists sessions to confirm `session_id`, posts the hook, and returns
    /// the approve/deny result; an unknown session is an `Error`.
    /// Test: `execute_approve_unknown_session_errors`.
    async fn decide(&self, session_id: &str, approved: bool) -> CommandResult {
        let exists = match self.client.sessions().await {
            Ok(rows) => rows.iter().any(|s| s.id.0.to_string() == session_id),
            Err(e) => return CommandResult::Error(format!("daemon unreachable: {e}")),
        };
        if !exists {
            return CommandResult::Error(format!("session {session_id} not found"));
        }
        // Record the decision as a synthetic PostToolUse hook event.
        let hook_url = format!("{}/hooks", self.client.base_url());
        let _ = reqwest::Client::new()
            .post(&hook_url)
            .json(&serde_json::json!({
                "session_id": session_id,
                "event": "PostToolUse",
                "payload": { "approved": approved },
            }))
            .send()
            .await;
        if approved {
            CommandResult::Approved {
                session_id: session_id.to_string(),
            }
        } else {
            CommandResult::Denied {
                session_id: session_id.to_string(),
            }
        }
    }

    /// `/overseer` — fetch the overseer status.
    async fn overseer(&self) -> CommandResult {
        match self.client.overseer_status().await {
            Ok(snap) => CommandResult::OverseerStatus {
                enabled: snap.enabled,
                handler: snap.handler,
                decisions: DecisionCounts {
                    allow: snap.decisions.0,
                    block: snap.decisions.1,
                    flag: snap.decisions.2,
                },
            },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/tmux` — list every tmux session on the host.
    async fn tmux(&self) -> CommandResult {
        match self.client.tmux_sessions().await {
            Ok(rows) => CommandResult::TmuxSessions(
                rows.into_iter()
                    .map(|r| TmuxSessionSummary {
                        name: r.name,
                        managed: r.managed,
                    })
                    .collect(),
            ),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/projects` — discover projects from the Claude Code configuration.
    ///
    /// Why: lets the operator browse projects Claude Code already knows about
    /// and register one without typing its path.
    /// What: calls `GET /projects/discover` and maps each row to a
    /// [`DiscoveredProjectSummary`].
    /// Test: `execute_projects_against_test_daemon`.
    async fn projects(&self) -> CommandResult {
        match self.client.discover_projects().await {
            Ok(rows) => CommandResult::DiscoveredProjects(
                rows.into_iter()
                    .map(|r| DiscoveredProjectSummary {
                        path: r.path,
                        session_count: r.session_count,
                        last_session: r.last_session,
                    })
                    .collect(),
            ),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/discover` — auto-discover tmux sessions running Claude Code.
    ///
    /// Why: operators run Claude Code in tmux panes the daemon never created;
    /// `/discover` scans every pane and adopts the ones running it so they show
    /// up without a manual `/adopt`.
    /// What: calls `POST /sessions/discover`; returns [`CommandResult::Discovered`]
    /// with the count, or an `Error` when the daemon is unreachable.
    /// Test: `execute_discover_against_test_daemon`.
    async fn discover(&self) -> CommandResult {
        match self.client.discover_sessions().await {
            Ok(count) => CommandResult::Discovered { count },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/adopt` — adopt an external tmux session for oversight.
    ///
    /// Why: brings a session trusty-mpm did not create under management with a
    /// single command.
    /// What: calls `POST /tmux/adopt`; returns [`CommandResult::Adopted`] on
    /// success, an `Error` when the session is unknown or the daemon is down.
    /// Test: `execute_adopt_unknown_session_errors`.
    async fn adopt(&self, session: &str) -> CommandResult {
        match self.client.adopt_tmux_session(session).await {
            Ok(true) => CommandResult::Adopted {
                session: session.to_string(),
            },
            Ok(false) => CommandResult::Error(format!("tmux session {session} not found")),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// Register a discovered project with the daemon.
    ///
    /// Why: the Telegram `/projects` keyboard's "Set Active" button registers a
    /// project by path; that does not fit the pure `TrustyCommand` shape (the
    /// path is keyboard callback data), so the bot adapter calls this directly.
    /// What: calls `POST /projects`; returns [`CommandResult::DiscoveredProjects`]
    /// is not appropriate here, so a confirmation is reported via the registered
    /// path. Returns an `Error` when the daemon is unreachable.
    /// Test: `register_project_succeeds`.
    pub async fn register_project(&self, path: &str) -> CommandResult {
        match self.client.register_project(path).await {
            Ok(()) => CommandResult::ProjectRegistered {
                path: path.to_string(),
            },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/config` — analyze a project's Claude Code config.
    async fn config(&self, project: &str) -> CommandResult {
        match self.client.analyze_config(project).await {
            Ok(recs) => CommandResult::ConfigAnalysis {
                project: project.to_string(),
                recommendations: recs
                    .into_iter()
                    .map(|r| RecommendationSummary {
                        id: r.id,
                        message: r.message,
                    })
                    .collect(),
            },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/snapshot` — capture a tmux pane.
    async fn snapshot(&self, session: &str) -> CommandResult {
        match self.client.snapshot_tmux_session(session).await {
            Ok(Some(output)) => CommandResult::Snapshot {
                session: session.to_string(),
                output,
            },
            Ok(None) => CommandResult::Error(format!("tmux session {session} not found")),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/kill` — kill a session.
    async fn kill(&self, session_id: &str) -> CommandResult {
        match self.client.kill_session(session_id).await {
            Ok(true) => CommandResult::Killed {
                session_id: session_id.to_string(),
            },
            Ok(false) => CommandResult::Error(format!("session {session_id} not found")),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/send` — resolve a session and send a prompt to its tmux pane.
    ///
    /// Why: `/send <session> <prompt>` drives a running Claude Code session
    /// remotely; the session is identified by id, friendly name, or a name
    /// prefix, so the executor resolves it against `GET /sessions` first.
    /// What: fetches the session list, finds the first session whose id or
    /// `tmux_name` matches `session` (exact, then prefix), posts the prompt via
    /// `POST /sessions/{id}/command`, and returns the captured pane output
    /// truncated to [`MAX_OUTPUT_CHARS`] (Telegram's message limit). An unknown
    /// session or unreachable daemon yields a [`CommandResult::Error`].
    /// Test: `execute_send_unknown_session_errors`,
    /// `execute_send_against_test_daemon`.
    async fn send(&self, session: &str, prompt: &str) -> CommandResult {
        if prompt.trim().is_empty() {
            return CommandResult::Error("send: a prompt is required".to_string());
        }
        let rows = match self.client.sessions().await {
            Ok(rows) => rows,
            Err(e) => return CommandResult::Error(format!("daemon unreachable: {e}")),
        };
        let Some(target) = resolve_session(&rows, session) else {
            return CommandResult::Error(format!("session {session} not found"));
        };
        match self.client.send_session_command(&target, prompt).await {
            Ok(Some(output)) => CommandResult::CommandSent {
                session: target,
                output: truncate_output(&output),
            },
            Ok(None) => CommandResult::Error(format!("session {session} not found")),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/launch` — deploy the framework, then start or attach to a session.
    ///
    /// Why: `tm launch` (and a future `/launch` on other UIs) is the full entry
    /// point — it runs `prepare_session` to deploy instructions, agents, and
    /// skills before bringing the tmux-hosted session up.
    /// What: calls [`DaemonClient::launch_session`], which runs the deployment
    /// sequence and then registers + starts the tmux session.
    /// Test: `execute_launch_errors_when_daemon_unreachable`.
    async fn launch(&self, project: &std::path::Path) -> CommandResult {
        let workdir = project.to_string_lossy().to_string();
        match self.client.launch_session(&workdir).await {
            Ok(session) => CommandResult::SessionStarted {
                session,
                workdir,
                deployed: true,
            },
            Err(e) => CommandResult::Error(format!("launch failed: {e}")),
        }
    }

    /// `/connect` — start or attach to a session *without* deploying anything.
    ///
    /// Why: `tm connect` is the lightweight sibling of `/launch`. It skips the
    /// `prepare_session` deployment sequence and only ensures the tmux-hosted
    /// session is running — idempotent: create when absent, attach when present.
    /// What: calls [`DaemonClient::connect_session`], which registers the
    /// session via `POST /api/v1/sessions/connect` and runs `tmux new-session
    /// -A` without any artifact deployment.
    /// Test: `execute_connect_errors_when_daemon_unreachable`.
    async fn connect(&self, project: &std::path::Path) -> CommandResult {
        let workdir = project.to_string_lossy().to_string();
        match self.client.connect_session(&workdir).await {
            Ok(session) => CommandResult::SessionStarted {
                session,
                workdir,
                deployed: false,
            },
            Err(e) => CommandResult::Error(format!("connect failed: {e}")),
        }
    }

    /// `/doctor` — run the full system diagnostic.
    ///
    /// Why: a single command that confirms the whole trusty-mpm stack is wired
    /// correctly; every UI funnels `/doctor` here so the report is identical.
    /// What: resolves the process cwd as the project to scope the instruction
    /// probe, calls `GET /api/v1/doctor`, and maps the result to
    /// [`CommandResult::Doctor`]; an unreachable daemon becomes an `Error`.
    /// Test: `execute_doctor_against_test_daemon`.
    async fn doctor(&self) -> CommandResult {
        // The instruction-pipeline probe is project-scoped; the process cwd is
        // the best "current project" guess a stateless executor can offer.
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string());
        match self.client.doctor(cwd.as_deref()).await {
            Ok(report) => CommandResult::Doctor(report),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `tm coordinator <message>` — send a message to the cross-session
    /// coordinator and return its reply.
    ///
    /// Why: the CLI/Telegram entry point for the coordinator; a stateless
    /// invocation has no rolling history, so each call is a fresh conversation.
    /// What: calls `POST /api/v1/coordinator/chat` with an empty history; maps
    /// a routed-command outcome (or an LLM reply) to [`CommandResult::ChatReply`],
    /// and a `503` (LLM not configured) to a clear [`CommandResult::Error`].
    /// Test: `coordinator_chat_outcome_deserializes` in the client tests.
    async fn coordinator_chat(&self, message: &str) -> CommandResult {
        match self.client.coordinator_chat(message, &[]).await {
            Ok(Some(outcome)) => {
                // A routed command returns the captured pane output; a plain
                // message returns the LLM reply. Surface whichever is present.
                let reply = match outcome.command_output {
                    Some(output) => format!("{}\n{output}", outcome.reply),
                    None => outcome.reply,
                };
                CommandResult::ChatReply { reply }
            }
            Ok(None) => CommandResult::Error(
                "coordinator chat is not configured (no OpenRouter API key)".to_string(),
            ),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/start` and `/pair` (no code) — query the pairing status.
    async fn pair_state(&self) -> CommandResult {
        match self.client.pair_status().await {
            Ok(status) => CommandResult::PairState {
                paired: status.paired,
            },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }
}

/// Resolve a session reference (id, name, or prefix) to a definitive target.
///
/// Why: `/send` accepts a fuzzy session reference; resolving it against the
/// live session list keeps the daemon call unambiguous.
/// What: returns the first session whose id or `tmux_name` equals `query`, or —
/// failing an exact match — whose `tmux_name` starts with `query`. The id and
/// friendly name are both acceptable targets for `POST /sessions/{id}/command`.
/// Test: `resolve_session_exact_and_prefix`.
fn resolve_session(rows: &[SessionRow], query: &str) -> Option<String> {
    if let Some(row) = rows
        .iter()
        .find(|r| r.id.0.to_string() == query || r.tmux_name == query)
    {
        return Some(target_of(row));
    }
    rows.iter()
        .find(|r| !r.tmux_name.is_empty() && r.tmux_name.starts_with(query))
        .map(target_of)
}

/// The path-segment target for a session: its friendly name when set, else its id.
///
/// Why: `POST /sessions/{id}/command` resolves either form; the friendly name
/// is more readable in the reply, so it is preferred when present.
/// What: returns `tmux_name` when non-empty, otherwise the UUID string.
/// Test: covered by `resolve_session_exact_and_prefix`.
fn target_of(row: &SessionRow) -> String {
    if row.tmux_name.is_empty() {
        row.id.0.to_string()
    } else {
        row.tmux_name.clone()
    }
}

/// Truncate captured output to [`MAX_OUTPUT_CHARS`] on a character boundary.
///
/// Why: a session's pane capture can exceed Telegram's message limit; the
/// reply must stay deliverable.
/// What: returns `text` unchanged when short enough, otherwise the first
/// [`MAX_OUTPUT_CHARS`] characters followed by a truncation marker.
/// Test: `truncate_output_caps_long_text`.
fn truncate_output(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n… (output truncated)")
}

/// Render a [`SessionStatus`] as its wire label.
///
/// Why: `SessionSummary.status` is a display string; serializing the typed
/// status keeps the label identical to the daemon's JSON wire form.
/// What: serializes the status via serde and strips the surrounding quotes.
/// Test: covered by `execute_sessions_against_test_daemon`.
fn status_label(status: crate::core::session::SessionStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::IntoFuture;

    /// Spawn the daemon's real HTTP API on a random loopback port.
    ///
    /// Why: lets the executor be tested against the genuine daemon routes
    /// without a live daemon, tmux, or external network.
    /// What: builds `api::router(DaemonState::shared())`, binds an ephemeral
    /// port, serves it on a background task, and returns the state plus base URL.
    /// Test: used by the `execute_*` tests below.
    async fn spawn_test_daemon() -> (std::sync::Arc<crate::daemon::state::DaemonState>, String) {
        use crate::daemon::{api, state::DaemonState};
        // Root the daemon's persisted state at a throwaway temp directory so
        // pairing tests never read (or write) the operator's real pairing
        // record. `keep` leaks the directory so it outlives the server task.
        let root = tempfile::tempdir().unwrap().keep();
        let state = std::sync::Arc::new(DaemonState::with_root(root));
        let router = api::router(std::sync::Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router).into_future());
        (state, format!("http://{addr}"))
    }

    #[tokio::test]
    async fn execute_help_returns_help() {
        // The `/help` path is pure — no HTTP, no daemon.
        let executor = CommandExecutor::new("http://unused");
        match executor.execute(TrustyCommand::Help).await {
            CommandResult::Help(text) => assert!(text.contains("/sessions")),
            other => panic!("expected Help, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_sessions_against_test_daemon() {
        // With one registered session, `/sessions` returns exactly that summary.
        use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
        let (state, url) = spawn_test_daemon().await;
        let mut session = Session::new(SessionId::new(), "/tmp/proj", ControlModel::Tmux, None);
        session.status = SessionStatus::Active;
        state.register_session(session);

        let executor = CommandExecutor::new(url);
        match executor.execute(TrustyCommand::Sessions).await {
            CommandResult::Sessions(list) => {
                assert_eq!(list.len(), 1);
                assert_eq!(list[0].workdir, "/tmp/proj");
            }
            other => panic!("expected Sessions, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_kill_returns_killed() {
        // Registering a session then killing it yields `Killed`.
        use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
        let (state, url) = spawn_test_daemon().await;
        let id = SessionId::new();
        let mut session = Session::new(id, "/tmp/proj", ControlModel::Tmux, None);
        session.status = SessionStatus::Active;
        state.register_session(session);

        let executor = CommandExecutor::new(url);
        match executor
            .execute(TrustyCommand::Kill {
                session_id: id.0.to_string(),
            })
            .await
        {
            CommandResult::Killed { session_id } => assert_eq!(session_id, id.0.to_string()),
            other => panic!("expected Killed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_kill_unknown_session_errors() {
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor
            .execute(TrustyCommand::Kill {
                session_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
        {
            CommandResult::Error(msg) => assert!(msg.contains("not found")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_approve_unknown_session_errors() {
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor
            .execute(TrustyCommand::Approve {
                session_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
        {
            CommandResult::Error(msg) => assert!(msg.contains("not found")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_approve_known_session() {
        use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
        let (state, url) = spawn_test_daemon().await;
        let id = SessionId::new();
        let mut session = Session::new(id, "/tmp/proj", ControlModel::Tmux, None);
        session.status = SessionStatus::Active;
        state.register_session(session);

        let executor = CommandExecutor::new(url);
        match executor
            .execute(TrustyCommand::Approve {
                session_id: id.0.to_string(),
            })
            .await
        {
            CommandResult::Approved { session_id } => assert_eq!(session_id, id.0.to_string()),
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_projects_against_test_daemon() {
        // `/projects` returns a well-formed (possibly empty) discovered list.
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor.execute(TrustyCommand::Projects).await {
            CommandResult::DiscoveredProjects(list) => {
                for p in &list {
                    assert!(!p.path.is_empty());
                }
            }
            other => panic!("expected DiscoveredProjects, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_discover_against_test_daemon() {
        // `/discover` returns a well-formed count (zero when tmux is absent on
        // CI), never an error against a live daemon.
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor.execute(TrustyCommand::Discover).await {
            CommandResult::Discovered { count } => {
                // Count is a usize; the call simply must succeed.
                let _ = count;
            }
            other => panic!("expected Discovered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_adopt_unknown_session_errors() {
        // Adopting a session that does not exist (or with tmux unavailable on
        // CI) reports an error rather than a success.
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor
            .execute(TrustyCommand::Adopt {
                session: "no-such-session-xyz".into(),
            })
            .await
        {
            CommandResult::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn register_project_succeeds() {
        // The `[Set Active]` flow registers a project by path.
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor.register_project("/work/discovered-demo").await {
            CommandResult::ProjectRegistered { path } => {
                assert_eq!(path, "/work/discovered-demo");
            }
            other => panic!("expected ProjectRegistered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_doctor_against_test_daemon() {
        // `/doctor` returns a five-check report against a live daemon.
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor.execute(TrustyCommand::Doctor).await {
            CommandResult::Doctor(report) => {
                assert_eq!(report.checks.len(), 5);
            }
            other => panic!("expected Doctor, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_overseer_returns_status() {
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor.execute(TrustyCommand::Overseer).await {
            CommandResult::OverseerStatus { handler, .. } => assert!(!handler.is_empty()),
            other => panic!("expected OverseerStatus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_status_no_events() {
        use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
        let (state, url) = spawn_test_daemon().await;
        let id = SessionId::new();
        let mut session = Session::new(id, "/tmp/proj", ControlModel::Tmux, None);
        session.status = SessionStatus::Active;
        state.register_session(session);

        let executor = CommandExecutor::new(url);
        match executor
            .execute(TrustyCommand::Status {
                session_id: id.0.to_string(),
            })
            .await
        {
            CommandResult::SessionDetail { events, .. } => assert!(events.is_empty()),
            other => panic!("expected SessionDetail, got {other:?}"),
        }
    }

    #[test]
    fn resolve_session_exact_and_prefix() {
        use crate::core::session::{SessionId, SessionStatus};
        let rows = vec![
            SessionRow {
                id: SessionId(uuid::Uuid::nil()),
                workdir: "/tmp/a".into(),
                status: SessionStatus::Active,
                active_delegations: 0,
                tmux_name: "tmpm-blue-fox".into(),
                last_seen: Default::default(),
            },
            SessionRow {
                id: SessionId(uuid::Uuid::from_u128(1)),
                workdir: "/tmp/b".into(),
                status: SessionStatus::Active,
                active_delegations: 0,
                tmux_name: "frontend".into(),
                last_seen: Default::default(),
            },
        ];
        // Exact friendly-name match.
        assert_eq!(
            resolve_session(&rows, "frontend").as_deref(),
            Some("frontend")
        );
        // Prefix match.
        assert_eq!(
            resolve_session(&rows, "tmpm-blue").as_deref(),
            Some("tmpm-blue-fox")
        );
        // Exact id match resolves to the friendly name.
        assert_eq!(
            resolve_session(&rows, &uuid::Uuid::nil().to_string()).as_deref(),
            Some("tmpm-blue-fox")
        );
        assert!(resolve_session(&rows, "no-such").is_none());
    }

    #[test]
    fn truncate_output_caps_long_text() {
        let short = "hello";
        assert_eq!(truncate_output(short), short);
        let long = "x".repeat(MAX_OUTPUT_CHARS + 100);
        let truncated = truncate_output(&long);
        assert!(truncated.contains("output truncated"));
        assert!(truncated.chars().count() <= MAX_OUTPUT_CHARS + 32);
    }

    #[tokio::test]
    async fn execute_send_unknown_session_errors() {
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor
            .execute(TrustyCommand::Send {
                session: "no-such-session".into(),
                prompt: "hello".into(),
            })
            .await
        {
            CommandResult::Error(msg) => assert!(msg.contains("not found")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_connect_errors_when_daemon_unreachable() {
        // `/connect` registers via `POST /api/v1/sessions/connect`; with no
        // daemon the failure surfaces as a renderable `Error`, never a panic.
        let executor = CommandExecutor::new("http://127.0.0.1:0");
        match executor
            .execute(TrustyCommand::Connect {
                project: "/tmp/no-such-project".into(),
                session_name: None,
            })
            .await
        {
            CommandResult::Error(msg) => assert!(msg.contains("connect failed")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_launch_errors_when_daemon_unreachable() {
        // `/launch` registers via `POST /sessions`; with no daemon the failure
        // surfaces as a renderable `Error`.
        let executor = CommandExecutor::new("http://127.0.0.1:0");
        match executor
            .execute(TrustyCommand::Launch {
                project: "/tmp/no-such-project".into(),
                session_name: None,
            })
            .await
        {
            CommandResult::Error(msg) => assert!(msg.contains("launch failed")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_send_empty_prompt_errors() {
        let executor = CommandExecutor::new("http://unused");
        match executor
            .execute(TrustyCommand::Send {
                session: "frontend".into(),
                prompt: "   ".into(),
            })
            .await
        {
            CommandResult::Error(msg) => assert!(msg.contains("prompt")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pair_request_returns_code() {
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor.pair_request().await {
            CommandResult::PairCode { code, .. } => assert_eq!(code.len(), 6),
            other => panic!("expected PairCode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pair_confirm_unknown_code_errors() {
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        match executor.pair_confirm("ZZZZZZ", 999).await {
            CommandResult::Error(msg) => assert!(msg.contains("invalid")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pair_request_then_confirm_succeeds() {
        // The full handshake: request a code, confirm it, then status is paired.
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        let code = match executor.pair_request().await {
            CommandResult::PairCode { code, .. } => code,
            other => panic!("expected PairCode, got {other:?}"),
        };
        match executor.pair_confirm(&code, 424242).await {
            CommandResult::PairSuccess { chat_info } => assert!(chat_info.contains("424242")),
            other => panic!("expected PairSuccess, got {other:?}"),
        }
        match executor.execute(TrustyCommand::Start).await {
            CommandResult::PairState { paired } => assert!(paired),
            other => panic!("expected PairState, got {other:?}"),
        }
    }
}
