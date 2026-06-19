//! Route the `tm sessions` managed verbs through the shared chat-core layer.
//!
//! Why: the managed session-manager verbs (`new`/`ls`/`send`/`answer`/`attach`/
//! `stop`/`resume`/`decommission`) historically built `reqwest` requests by hand
//! in `managed.rs` and resolved id-or-name with their OWN resolvers. Phase 1B
//! (refs #1283) converges them onto the one shared command layer that landed in
//! `src/client/`: each clap variant is mapped to a [`TrustyCommand`], executed by
//! the single [`CommandExecutor`], and the returned [`CommandResult`] is rendered
//! for the terminal here. The CLI thus shares the exact dispatch, daemon client,
//! and the canonical [`resolve_target`] resolver with every other adapter (TUI,
//! Telegram, Web), so a new endpoint is wired once.
//! What: [`to_command`] maps a managed [`SessionAction`] to a [`TrustyCommand`];
//! [`render_cli`] renders a [`CommandResult`] as plain, scriptable terminal text
//! (NOT the Telegram HTML formatter); [`run`] glues them — build the executor
//! from the CLI's `(client, url)` pair, execute, print. The managed-aware
//! `stop`/`resume` decision (managed vs project session) is made by
//! [`resolve_managed_match`] using the canonical resolver.
//! Test: `to_command_maps_*` and `render_cli_*` in `tests.rs`; the HTTP round-trip
//! is covered by the executor tests and `tests/session_manager_mvp.rs`.

use trusty_mpm::client::{CommandExecutor, CommandResult, TrustyCommand, resolve_target};

use crate::cli::SessionAction;

/// Build a [`CommandExecutor`] from the CLI's `(client, url)` pair.
///
/// Why: the CLI owns a configured `reqwest::Client`; routing through chat-core
/// must reuse it (timeouts/pool) rather than minting a default one.
/// What: returns `CommandExecutor::with_client(client.clone(), url)` — cloning a
/// `reqwest::Client` is cheap (`Arc` internally), so the pool/settings persist.
/// Test: exercised transitively by every routed managed handler's coverage.
fn executor(client: &reqwest::Client, url: &str) -> CommandExecutor {
    CommandExecutor::with_client(client.clone(), url.to_string())
}

/// Map a MANAGED [`SessionAction`] variant to its [`TrustyCommand`] intent.
///
/// Why: the clap surface and the chat-core command surface are deliberately
/// separate (one is the operator's typed CLI, the other the UI-agnostic intent);
/// this is the single, unit-testable seam that converts one into the other so the
/// CLI never embeds dispatch logic.
/// What: returns `Some(TrustyCommand)` for the managed verbs this module routes
/// (`new`/`ls`/`send`/`answer`/`attach`/`stop`/`resume`/`decommission` and the
/// deprecated `managed-stop`/`runtime-stop`/`managed-resume` aliases) and `None`
/// for every project-session or non-managed variant the caller handles elsewhere.
/// The deprecated aliases map to the SAME command as their canonical verb; the
/// caller emits the deprecation notice before dispatch.
/// Test: `to_command_maps_new`, `to_command_maps_lifecycle_aliases`,
/// `to_command_ignores_project_verbs`.
pub(crate) fn to_command(action: &SessionAction) -> Option<TrustyCommand> {
    Some(match action {
        SessionAction::New {
            repo,
            git_ref,
            task,
            name_hint,
            runtime,
        } => TrustyCommand::ManagedNew {
            repo_url: repo.clone(),
            git_ref: git_ref.clone(),
            task: task.clone(),
            name_hint: name_hint.clone(),
            // Send the canonical wire spelling (`claude-code`/`tcode`); the CLI
            // already validated the value via the `RuntimeKind` value-enum.
            runtime: Some(runtime.as_str().to_string()),
        },
        SessionAction::Ls { .. } => TrustyCommand::ManagedList,
        SessionAction::Send { id, text } => TrustyCommand::ManagedSend {
            target: id.clone(),
            text: text.clone(),
        },
        SessionAction::Answer { id, answer } => TrustyCommand::ManagedAnswer {
            target: id.clone(),
            answer: answer.clone(),
        },
        SessionAction::Attach { id } => TrustyCommand::ManagedAttachCmd { target: id.clone() },
        // `managed-stop` and `runtime-stop` are deprecated aliases of `stop`.
        SessionAction::ManagedStop { id } | SessionAction::RuntimeStop { id } => {
            TrustyCommand::ManagedRuntimeStop { target: id.clone() }
        }
        SessionAction::ManagedResume { id } => TrustyCommand::ManagedResume { target: id.clone() },
        SessionAction::Decommission { id } => {
            TrustyCommand::ManagedDecommission { target: id.clone() }
        }
        // Every other variant is project-session / TUI / prune; not routed here.
        _ => return None,
    })
}

/// Render a [`CommandResult`] as plain, scriptable terminal text.
///
/// Why: chat-core hands back a structured result; the CLI must render it in its
/// own idiom — newline-delimited plain text for the shell, NOT the Telegram HTML
/// formatter. Output is kept close to the pre-chat-core CLI lines so existing
/// scripts keep working.
/// What: returns the multi-line string the CLI prints for each managed result
/// variant (and the shared `Error` variant). Variants the managed CLI never
/// produces render a single diagnostic line rather than panicking, so the
/// function is total.
/// Test: `render_cli_spawned`, `render_cli_sessions`, `render_cli_lifecycle`,
/// `render_cli_error`.
pub(crate) fn render_cli(result: &CommandResult) -> String {
    match result {
        CommandResult::ManagedSpawned {
            id,
            name,
            state,
            runtime,
            attach_cmd,
        } => format!("spawned {name} ({id}) [{state}] runtime={runtime}\n  attach: {attach_cmd}"),
        CommandResult::ManagedSessions(list) => {
            if list.is_empty() {
                return "no managed sessions".to_string();
            }
            list.iter()
                .map(|s| {
                    let pending = s
                        .pending_decision
                        .as_deref()
                        .map(|d| format!(" pending=\"{d}\""))
                        .unwrap_or_default();
                    format!("{} {} {}{}", s.id, s.name, s.state, pending)
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        CommandResult::ManagedSent { .. } => "sent".to_string(),
        CommandResult::ManagedAnswered { .. } => "answered".to_string(),
        CommandResult::ManagedAttachCmd { attach_cmd, .. } => attach_cmd.clone(),
        CommandResult::ManagedLifecycle {
            id,
            name,
            state,
            action,
        } => match action.as_str() {
            "stopped" => {
                format!("runtime stopped {id} (workspace intact; use 'resume' to restart)")
            }
            "resumed" => format!("resumed {name} ({id}) [{state}]"),
            "decommissioned" => {
                format!("decommissioned {id} (workspace removed; tombstone record kept)")
            }
            other => format!("{other} {id} ({name}) [{state}]"),
        },
        CommandResult::Error(msg) => msg.clone(),
        // The managed CLI verbs never yield these; render a stable diagnostic
        // rather than panicking so `render_cli` stays total.
        other => format!("unexpected result: {other:?}"),
    }
}

/// Resolve a fuzzy id/name against the managed session list (managed-vs-project
/// routing decision for `stop`/`resume`).
///
/// Why (#1218): the canonical `tm sessions stop`/`resume` verbs are managed-aware
/// — a managed id/name routes to the managed runtime; anything else falls back to
/// the project-session path. The matching now goes through the ONE canonical
/// [`resolve_target`] resolver (Phase 1B collapsed the bespoke
/// `classify_managed_target`/`resolve_managed_id`), so the precedence (id-exact →
/// name-exact → unambiguous prefix) is identical to every other surface.
/// What: fetches the managed list via the shared [`CommandExecutor`]'s client and
/// returns the matched session's canonical id, or `None` when the daemon is
/// unreachable / nothing matches (the caller then takes the project path, exactly
/// as the old `resolve_managed_id` did).
/// Test: the resolver precedence is covered by `client::resolver` tests; the
/// managed-vs-project fallback by the integration suite.
pub(crate) async fn resolve_managed_match(
    client: &reqwest::Client,
    url: &str,
    id_or_name: &str,
) -> Option<String> {
    let sessions = executor(client, url)
        .client()
        .list_managed_sessions()
        .await
        .ok()?;
    resolve_target(&sessions, id_or_name).map(|s| s.id.clone())
}

/// Resolve a PROJECT-session id/name to its canonical UUID via the shared
/// resolver.
///
/// Why: `tm sessions events` needs a UUID for `GET /sessions/{id}/events`, but
/// operators may pass a friendly `tmpm-*` name. Phase 1B collapsed the bespoke
/// `resolve_session_id` here so the project-session lookup uses the SAME
/// [`resolve_target`] precedence (id-exact → name-exact → unambiguous prefix) as
/// every managed path.
/// What: fetches the typed project-session list via the shared client and returns
/// the matched row's UUID string, or `None` when the daemon is unreachable /
/// nothing matches.
/// Test: precedence by `client::resolver` tests; wiring by `cli_parses_session_events`.
pub(crate) async fn resolve_project_session_id(
    client: &reqwest::Client,
    url: &str,
    id_or_name: &str,
) -> anyhow::Result<Option<String>> {
    let rows = executor(client, url).client().sessions().await?;
    Ok(resolve_target(&rows, id_or_name).map(|r| r.id.0.to_string()))
}

/// Execute a managed [`SessionAction`] through chat-core and print the result.
///
/// Why: the single entry point the `session` dispatcher calls for every managed
/// verb — it keeps the dispatcher a thin match and concentrates the
/// map → execute → render flow here.
/// What: maps `action` via [`to_command`], runs it on a [`CommandExecutor`] built
/// from the CLI's client, and prints [`render_cli`] to stdout. Returns `false`
/// when `action` is not a managed verb this module routes (so the caller can
/// handle it on the project path); `true` once it has handled and printed.
/// Test: parsing/mapping by `to_command_maps_*`; rendering by `render_cli_*`; the
/// HTTP path by the executor tests and `tests/session_manager_mvp.rs`.
pub(crate) async fn run(
    client: &reqwest::Client,
    url: &str,
    action: &SessionAction,
) -> anyhow::Result<bool> {
    let Some(cmd) = to_command(action) else {
        return Ok(false);
    };
    let result = executor(client, url).execute(cmd).await;
    println!("{}", render_cli(&result));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::client::ManagedSessionView;

    #[test]
    fn to_command_maps_new() {
        let action = SessionAction::New {
            repo: "https://example/r.git".to_string(),
            git_ref: "main".to_string(),
            task: "do it".to_string(),
            name_hint: Some("api".to_string()),
            runtime: trusty_mpm::runtime::RuntimeKind::ClaudeCode,
        };
        match to_command(&action) {
            Some(TrustyCommand::ManagedNew {
                repo_url,
                git_ref,
                task,
                name_hint,
                runtime,
            }) => {
                assert_eq!(repo_url, "https://example/r.git");
                assert_eq!(git_ref, "main");
                assert_eq!(task, "do it");
                assert_eq!(name_hint.as_deref(), Some("api"));
                assert_eq!(runtime.as_deref(), Some("claude-code"));
            }
            other => panic!("expected ManagedNew, got {other:?}"),
        }
    }

    #[test]
    fn to_command_maps_send_answer_attach() {
        assert!(matches!(
            to_command(&SessionAction::Send {
                id: "x".into(),
                text: "hi".into()
            }),
            Some(TrustyCommand::ManagedSend { .. })
        ));
        assert!(matches!(
            to_command(&SessionAction::Answer {
                id: "x".into(),
                answer: "yes".into()
            }),
            Some(TrustyCommand::ManagedAnswer { .. })
        ));
        assert!(matches!(
            to_command(&SessionAction::Attach { id: "x".into() }),
            Some(TrustyCommand::ManagedAttachCmd { .. })
        ));
    }

    #[test]
    fn to_command_maps_lifecycle_aliases() {
        // Both deprecated aliases map to the SAME canonical stop command.
        assert!(matches!(
            to_command(&SessionAction::ManagedStop { id: "x".into() }),
            Some(TrustyCommand::ManagedRuntimeStop { .. })
        ));
        assert!(matches!(
            to_command(&SessionAction::RuntimeStop { id: "x".into() }),
            Some(TrustyCommand::ManagedRuntimeStop { .. })
        ));
        assert!(matches!(
            to_command(&SessionAction::ManagedResume { id: "x".into() }),
            Some(TrustyCommand::ManagedResume { .. })
        ));
        assert!(matches!(
            to_command(&SessionAction::Decommission { id: "x".into() }),
            Some(TrustyCommand::ManagedDecommission { .. })
        ));
        assert!(matches!(
            to_command(&SessionAction::Ls { json: false }),
            Some(TrustyCommand::ManagedList)
        ));
    }

    #[test]
    fn to_command_ignores_project_verbs() {
        // Project-session and prune verbs are NOT routed through chat-core here.
        assert!(
            to_command(&SessionAction::Stop {
                id_or_name: "x".into()
            })
            .is_none()
        );
        assert!(
            to_command(&SessionAction::Resume {
                id_or_name: "x".into()
            })
            .is_none()
        );
        assert!(to_command(&SessionAction::Breakers).is_none());
        assert!(
            to_command(&SessionAction::PruneIdle {
                dry_run: false,
                json: false
            })
            .is_none()
        );
    }

    fn view(id: &str, name: &str, state: &str, pending: Option<&str>) -> ManagedSessionView {
        ManagedSessionView {
            id: id.to_string(),
            name: name.to_string(),
            state: state.to_string(),
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: pending.map(str::to_string),
            proposed_default: None,
        }
    }

    #[test]
    fn render_cli_spawned() {
        let r = CommandResult::ManagedSpawned {
            id: "uuid-1".into(),
            name: "tmpm-red-owl".into(),
            state: "Provisioning".into(),
            runtime: "claude-code".into(),
            attach_cmd: "tmux attach -t tmpm-red-owl".into(),
        };
        let out = render_cli(&r);
        assert_eq!(
            out,
            "spawned tmpm-red-owl (uuid-1) [Provisioning] runtime=claude-code\n  attach: tmux attach -t tmpm-red-owl"
        );
    }

    #[test]
    fn render_cli_sessions() {
        // Empty list and populated list each match the legacy `session_ls` lines.
        assert_eq!(
            render_cli(&CommandResult::ManagedSessions(vec![])),
            "no managed sessions"
        );
        let r = CommandResult::ManagedSessions(vec![
            view("m-1", "alpha", "Running", None),
            view("m-2", "bravo", "Stopped", Some("approve deploy?")),
        ]);
        assert_eq!(
            render_cli(&r),
            "m-1 alpha Running\nm-2 bravo Stopped pending=\"approve deploy?\""
        );
    }

    #[test]
    fn render_cli_send_answer_attach() {
        assert_eq!(
            render_cli(&CommandResult::ManagedSent {
                id: "m-1".into(),
                tmux_name: "tmpm-red-owl".into()
            }),
            "sent"
        );
        assert_eq!(
            render_cli(&CommandResult::ManagedAnswered {
                id: "m-1".into(),
                answer: "yes".into(),
                tmux_name: "tmpm-red-owl".into()
            }),
            "answered"
        );
        assert_eq!(
            render_cli(&CommandResult::ManagedAttachCmd {
                id: "m-1".into(),
                attach_cmd: "tmux attach -t tmpm-red-owl".into()
            }),
            "tmux attach -t tmpm-red-owl"
        );
    }

    #[test]
    fn render_cli_lifecycle() {
        let stop = CommandResult::ManagedLifecycle {
            id: "m-1".into(),
            name: "alpha".into(),
            state: "Stopped".into(),
            action: "stopped".into(),
        };
        assert_eq!(
            render_cli(&stop),
            "runtime stopped m-1 (workspace intact; use 'resume' to restart)"
        );
        let resume = CommandResult::ManagedLifecycle {
            id: "m-1".into(),
            name: "alpha".into(),
            state: "Running".into(),
            action: "resumed".into(),
        };
        assert_eq!(render_cli(&resume), "resumed alpha (m-1) [Running]");
        let decom = CommandResult::ManagedLifecycle {
            id: "m-1".into(),
            name: "alpha".into(),
            state: "Decommissioned".into(),
            action: "decommissioned".into(),
        };
        assert_eq!(
            render_cli(&decom),
            "decommissioned m-1 (workspace removed; tombstone record kept)"
        );
    }

    #[test]
    fn render_cli_error() {
        assert_eq!(
            render_cli(&CommandResult::Error("managed session x not found".into())),
            "managed session x not found"
        );
    }
}
