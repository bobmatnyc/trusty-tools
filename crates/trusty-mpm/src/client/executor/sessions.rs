//! Project-session command methods + the pure session-resolution helpers.
//!
//! Why: the project-session family (`/sessions`, `/status`, `/tmux`, `/send`,
//! `/launch`, …) is the original executor surface; splitting it out of the
//! dispatch facade keeps `mod.rs` to the dispatch table alone and every file
//! under the 500-SLOC cap. The pure helpers (`resolve_session`, `truncate_output`,
//! `status_label`) live here because they are project-session shaped.
//! What: inherent `CommandExecutor` methods for the project-session commands,
//! plus the canonical-resolver-backed [`resolve_session`] and the output/status
//! formatting helpers the methods (and tests) use.
//! Test: covered by the `execute_*` tests in `tests.rs`.

use super::CommandExecutor;
use crate::client::http_client::SessionRow;
use crate::client::resolver::resolve_target;
use crate::client::result::{
    CommandResult, DecisionCounts, DiscoveredProjectSummary, RecommendationSummary, SessionSummary,
    TmuxSessionSummary,
};

/// Maximum captured-output length returned by `/send` (Telegram's message cap).
///
/// Why: Telegram rejects messages longer than ~4096 characters; truncating the
/// captured pane keeps the reply deliverable on every UI.
pub(crate) const MAX_OUTPUT_CHARS: usize = 4000;

impl CommandExecutor {
    /// `/sessions` — fetch and summarize the managed session list.
    pub(super) async fn sessions(&self) -> CommandResult {
        match self.client().sessions().await {
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
    pub(super) async fn status(&self, session_id: &str) -> CommandResult {
        match self.client().session_events(session_id).await {
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

    /// `/approve` and `/deny` — answer a session's pending decision.
    ///
    /// Why: this is the corrected decision-answer path. A managed session's
    /// decision is answered through the REAL `POST .../{id}/answer` endpoint
    /// (`answer_managed_session`) instead of the former synthetic `PostToolUse`
    /// hook, so the harness actually unblocks. A project session (not in the
    /// managed list) keeps its lightweight existence-check-and-acknowledge
    /// behavior — there is no managed decision to answer.
    ///
    /// Round-trip discipline (1A review follow-up): the managed answer needs the
    /// session's `proposed_default`, so a managed approve/deny is resolved
    /// against the managed list and answered in ONE managed round-trip — the
    /// project `GET /sessions` list is fetched ONLY when the managed list does
    /// not contain the target (a genuine project session), never speculatively.
    /// The managed list must be consulted first because managed precedence is a
    /// correctness rule (a real harness decision outranks a project-session
    /// acknowledgement); the families share the id/name space, so there is no
    /// cheaper local discriminator that would let us skip it.
    /// What: resolves `session_id` against the managed list first; on a match it
    /// posts the derived answer (the session's `proposed_default`, else `"yes"`,
    /// for approve; `"no"` for deny) to the answer endpoint and returns —
    /// without the project fetch. Otherwise it confirms the project session
    /// exists and returns Approved/Denied. An unknown session is an `Error`.
    /// Test: `execute_approve_known_session`, `execute_approve_unknown_session_errors`,
    /// `execute_approve_managed_skips_project_fetch`,
    /// `execute_approve_errors_when_managed_list_unreachable`.
    pub(super) async fn decide(&self, session_id: &str, approved: bool) -> CommandResult {
        // Prefer the managed answer endpoint: if this id/name is a managed
        // session, the operator's approve/deny is a real decision answer.
        //
        // A daemon transport error must NOT be swallowed: previously an
        // `if let Ok(..)` silently fell through to the project-session path,
        // turning an unreachable daemon into a misleading "not found". Match
        // the result explicitly — surface the transport error, and only fall
        // through to the project path when the managed list was fetched
        // successfully but did not contain the target.
        let managed = match self.client().list_managed_sessions().await {
            Ok(managed) => managed,
            Err(e) => return CommandResult::Error(format!("daemon unreachable: {e}")),
        };
        if let Some(found) = resolve_target(&managed, session_id) {
            // Managed match: answer in a single managed round-trip. The project
            // `GET /sessions` fetch below is intentionally NOT reached here.
            let answer = decide_answer(approved, found.proposed_default.as_deref());
            return match self
                .client()
                .answer_managed_session(&found.id, &answer)
                .await
            {
                Ok(_) if approved => CommandResult::Approved {
                    session_id: found.id.clone(),
                },
                Ok(_) => CommandResult::Denied {
                    session_id: found.id.clone(),
                },
                Err(e) => CommandResult::Error(format!("answer failed: {e}")),
            };
        }
        // Managed miss → this is (at most) a project session. Only NOW do we
        // pay for the project list to confirm it exists, then acknowledge. (No
        // synthetic hook — the managed path above is the only real decision
        // channel.)
        let exists = match self.client().sessions().await {
            Ok(rows) => rows.iter().any(|s| s.id.0.to_string() == session_id),
            Err(e) => return CommandResult::Error(format!("daemon unreachable: {e}")),
        };
        if !exists {
            return CommandResult::Error(format!("session {session_id} not found"));
        }
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
    pub(super) async fn overseer(&self) -> CommandResult {
        match self.client().overseer_status().await {
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
    pub(super) async fn tmux(&self) -> CommandResult {
        match self.client().tmux_sessions().await {
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
    pub(super) async fn projects(&self) -> CommandResult {
        match self.client().discover_projects().await {
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
    pub(super) async fn discover(&self) -> CommandResult {
        match self.client().discover_sessions().await {
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
    pub(super) async fn adopt(&self, session: &str) -> CommandResult {
        match self.client().adopt_tmux_session(session).await {
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
    /// What: calls `POST /projects`; returns a confirmation via the registered
    /// path. Returns an `Error` when the daemon is unreachable.
    /// Test: `register_project_succeeds`.
    pub async fn register_project(&self, path: &str) -> CommandResult {
        match self.client().register_project(path).await {
            Ok(()) => CommandResult::ProjectRegistered {
                path: path.to_string(),
            },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/config` — analyze a project's Claude Code config.
    pub(super) async fn config(&self, project: &str) -> CommandResult {
        match self.client().analyze_config(project).await {
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
    pub(super) async fn snapshot(&self, session: &str) -> CommandResult {
        match self.client().snapshot_tmux_session(session).await {
            Ok(Some(output)) => CommandResult::Snapshot {
                session: session.to_string(),
                output,
            },
            Ok(None) => CommandResult::Error(format!("tmux session {session} not found")),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/kill` — kill a session.
    pub(super) async fn kill(&self, session_id: &str) -> CommandResult {
        match self.client().kill_session(session_id).await {
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
    /// What: fetches the session list, resolves `session` via the canonical
    /// [`resolve_session`] (id-exact, name-exact, then prefix), posts the prompt
    /// via `POST /sessions/{id}/command`, and returns the captured pane output
    /// truncated to [`MAX_OUTPUT_CHARS`]. An unknown session or unreachable
    /// daemon yields a [`CommandResult::Error`].
    /// Test: `execute_send_unknown_session_errors`.
    pub(super) async fn send(&self, session: &str, prompt: &str) -> CommandResult {
        if prompt.trim().is_empty() {
            return CommandResult::Error("send: a prompt is required".to_string());
        }
        let rows = match self.client().sessions().await {
            Ok(rows) => rows,
            Err(e) => return CommandResult::Error(format!("daemon unreachable: {e}")),
        };
        let Some(target) = resolve_session(&rows, session) else {
            return CommandResult::Error(format!("session {session} not found"));
        };
        match self.client().send_session_command(&target, prompt).await {
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
    /// Why: `tm launch` is the full entry point — it runs `prepare_session`
    /// to deploy instructions, agents, and skills before bringing the
    /// tmux-hosted session up.
    /// What: calls [`DaemonClient::launch_session`], which runs the deployment
    /// sequence and then registers + starts the tmux session.
    /// Test: `execute_launch_errors_when_daemon_unreachable`.
    pub(super) async fn launch(&self, project: &std::path::Path) -> CommandResult {
        let workdir = project.to_string_lossy().to_string();
        match self.client().launch_session(&workdir).await {
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
    /// Why: `tm connect` skips the `prepare_session` deployment sequence and
    /// only ensures the tmux-hosted session is running — idempotent: create
    /// when absent, attach when present.
    /// What: calls [`DaemonClient::connect_session`].
    /// Test: `execute_connect_errors_when_daemon_unreachable`.
    pub(super) async fn connect(&self, project: &std::path::Path) -> CommandResult {
        let workdir = project.to_string_lossy().to_string();
        match self.client().connect_session(&workdir).await {
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
    pub(super) async fn doctor(&self) -> CommandResult {
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string());
        match self.client().doctor(cwd.as_deref()).await {
            Ok(report) => CommandResult::Doctor(report),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `tm coordinator <message>` — send a message to the cross-session
    /// coordinator and return its reply.
    ///
    /// Why: the CLI/Telegram entry point for the coordinator; a stateless
    /// invocation has no rolling history, so each call is a fresh conversation.
    /// What: calls `POST /api/v1/sessions/chat` with an empty history.
    /// Test: `coordinator_chat_outcome_deserializes` in the client tests.
    pub(super) async fn coordinator_chat(&self, message: &str) -> CommandResult {
        match self.client().coordinator_chat(message, &[]).await {
            Ok(Some(outcome)) => {
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
}

/// Resolve a session reference (id, name, or prefix) to a definitive target.
///
/// Why: `/send` accepts a fuzzy session reference; resolving it against the live
/// session list keeps the daemon call unambiguous. This is now a thin wrapper
/// over the canonical [`resolve_target`] resolver so the project-session and
/// managed paths share one precedence definition.
/// What: delegates to [`resolve_target`] (id-exact, name-exact, unambiguous
/// prefix) and maps the matched row to its path-segment target via [`target_of`].
/// Test: `resolve_session_exact_and_prefix`.
pub(crate) fn resolve_session(rows: &[SessionRow], query: &str) -> Option<String> {
    resolve_target(rows, query).map(target_of)
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

/// Derive the answer text a managed approve/deny posts to `.../answer`.
///
/// Why: `/approve` accepts the harness's proposed default when it has one (so the
/// operator confirms exactly what the session asked for), falling back to a plain
/// `"yes"`; `/deny` is always `"no"`. Extracting this keeps the decision-answer
/// derivation pure and unit-testable without a live daemon round-trip.
/// What: returns `proposed_default` (when `approved` and present), else `"yes"`
/// for an approve, else `"no"` for a deny.
/// Test: `decide_answer_prefers_proposed_default`.
pub(crate) fn decide_answer(approved: bool, proposed_default: Option<&str>) -> String {
    if !approved {
        return "no".to_string();
    }
    proposed_default
        .map(str::to_string)
        .unwrap_or_else(|| "yes".to_string())
}

/// Truncate captured output to [`MAX_OUTPUT_CHARS`] on a character boundary.
///
/// Why: a session's pane capture can exceed Telegram's message limit; the
/// reply must stay deliverable.
/// What: returns `text` unchanged when short enough, otherwise the first
/// [`MAX_OUTPUT_CHARS`] characters followed by a truncation marker.
/// Test: `truncate_output_caps_long_text`.
pub(crate) fn truncate_output(text: &str) -> String {
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
