//! Route the `tm session` managed verbs through the shared chat-core layer.
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

use trusty_mpm::client::{
    CommandExecutor, CommandResult, ManagedSessionSummary, TrustyCommand, resolve_target,
};

use crate::cli::SessionAction;

/// Build a [`CommandExecutor`] from the CLI's `(client, url)` pair.
///
/// Why: the CLI owns a configured `reqwest::Client`; routing through chat-core
/// must reuse it (timeouts/pool) rather than minting a default one.
/// What: returns `CommandExecutor::with_client(client.clone(), url)` — cloning a
/// `reqwest::Client` is cheap (`Arc` internally), so the pool/settings persist.
/// `pub(crate)` since #5913: `commands::managed`'s decommission handler builds
/// its executor here rather than minting a second one.
/// Test: exercised transitively by every routed managed handler's coverage.
pub(crate) fn executor(client: &reqwest::Client, url: &str) -> CommandExecutor {
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
            no_inject,
            deliverable,
        } => TrustyCommand::ManagedNew {
            repo_url: repo.clone(),
            git_ref: git_ref.clone(),
            task: task.clone(),
            name_hint: name_hint.clone(),
            // Send the canonical wire spelling (`claude-code`/`tcode`); the CLI
            // already validated the value via the `RuntimeKind` value-enum.
            runtime: Some(runtime.as_str().to_string()),
            // Turnkey by default: omit the field (daemon default = inject) and
            // only send `Some(false)` when `--no-inject` opts into metadata-only
            // (#1903/#1299). Keeping the default absent means `session start`
            // (which builds a New action) posts the same minimal wire shape it
            // always has.
            inject_task: if *no_inject { Some(false) } else { None },
            // `--deliverable <id>` (DOC-35 §10.6, #2379): omitted entirely when
            // absent, same additive wire pattern as `inject_task` above.
            deliverable_id: deliverable.clone(),
            // #2450: the explicit `new`/`start` verb means "launch a NEW
            // session" — force it so the daemon's in-project reconnect
            // pre-flight (#1707) never adopts an existing live session for the
            // same project (and injects this task into it). `session start`
            // builds the same New action and is deliberately kept identical
            // (#1916), so it forces new too; programmatic surfaces (MCP,
            // SM-STDIO, chat, TUI) keep the reconnect by setting
            // `force_new: false` at their own construction sites.
            force_new: true,
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

/// The one decommission result line the CLI prints, for every verdict (#5899).
///
/// Why: two CLI paths report a decommission — the routed chat-core verb
/// ([`render_cli`]) and the raw-HTTP handler the bulk prune sweep calls
/// ([`super::managed::session_decommission`]) — and they disagreed. The routed one
/// hardcoded "workspace removed" for every decommission, so it announced deletion
/// of worktrees still on disk (#5899, a reintroduction of #1787). One function
/// both call means the honest wording cannot drift back apart.
/// What: maps the daemon's verdict to a message. `Some(true)` reports removal,
/// `Some(false)` says the workspace is still on disk, and `None` — an older daemon
/// that sends no verdict — reports the tombstone and says the workspace outcome is
/// unknown. There is no fourth state, and no case claims removal without
/// `Some(true)`: the CLI never probes the filesystem itself, since the daemon's
/// verdict is the authority (it is the only party that knows whether
/// `remove_dir_all` actually ran).
/// Test: `decommission_message_honours_every_verdict`,
/// `decommission_cli_message_never_claims_removal_when_workspace_remains`.
pub(crate) fn decommission_message(id: &str, workspace_removed: Option<bool>) -> String {
    match workspace_removed {
        Some(true) => format!("decommissioned {id} — workspace removed; tombstone record kept"),
        Some(false) => format!(
            "decommissioned {id} — tombstone record kept; workspace NOT removed \
             (still on disk)"
        ),
        None => format!(
            "decommissioned {id} — tombstone record kept; workspace outcome not \
             reported by the daemon"
        ),
    }
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
            workspace_removed,
        } => match action.as_str() {
            "stopped" => {
                format!("runtime stopped {id} (workspace intact; use 'resume' to restart)")
            }
            "resumed" => format!("resumed {name} ({id}) [{state}]"),
            // #5899: report the daemon's verdict, never a hardcoded "removed".
            "decommissioned" => decommission_message(id, *workspace_removed),
            other => format!("{other} {id} ({name}) [{state}]"),
        },
        CommandResult::Error(msg) => msg.clone(),
        // The managed CLI verbs never yield these; render a stable diagnostic
        // rather than panicking so `render_cli` stays total.
        other => format!("unexpected result: {other:?}"),
    }
}

/// Resolve a fuzzy id/name against the managed session list, returning the
/// FULL matched summary (issue #3600).
///
/// Why: [`resolve_managed_match`] discarded everything but the matched
/// session's `id`, but a pane-identity cross-check (`commands::pane_identity`)
/// needs the record's own captured `pane_id` too. Both callers now share this
/// one fetch-and-resolve, rather than each re-implementing the "list, then
/// `resolve_target`" round trip.
/// What: fetches the managed list via the shared [`CommandExecutor`]'s client
/// and returns a clone of the matched [`ManagedSessionSummary`], or `None`
/// when the daemon is unreachable / nothing matches.
/// Test: the resolver precedence is covered by `client::resolver` tests;
/// `resolve_managed_match` (below) is a thin projection of this and shares
/// its coverage.
pub(crate) async fn resolve_managed_summary(
    client: &reqwest::Client,
    url: &str,
    id_or_name: &str,
) -> Option<ManagedSessionSummary> {
    let sessions = executor(client, url)
        .client()
        .list_managed_sessions()
        .await
        .ok()?;
    resolve_target(&sessions, id_or_name).cloned()
}

/// Resolve a fuzzy id/name against the managed session list (managed-vs-project
/// routing decision for `stop`/`resume`).
///
/// Why (#1218): the canonical `tm session stop`/`resume` verbs are managed-aware
/// — a managed id/name routes to the managed runtime; anything else falls back to
/// the project-session path. The matching now goes through the ONE canonical
/// [`resolve_target`] resolver (Phase 1B collapsed the bespoke
/// `classify_managed_target`/`resolve_managed_id`), so the precedence (id-exact →
/// name-exact → unambiguous prefix) is identical to every other surface.
/// What: thin projection of [`resolve_managed_summary`] onto just the matched
/// session's canonical id, or `None` when the daemon is unreachable / nothing
/// matches (the caller then takes the project path, exactly as the old
/// `resolve_managed_id` did).
/// Test: the resolver precedence is covered by `client::resolver` tests; the
/// managed-vs-project fallback by the integration suite.
pub(crate) async fn resolve_managed_match(
    client: &reqwest::Client,
    url: &str,
    id_or_name: &str,
) -> Option<String> {
    resolve_managed_summary(client, url, id_or_name)
        .await
        .map(|s| s.id)
}

/// Resolve a PROJECT-session id/name to its canonical UUID via the shared
/// resolver.
///
/// Why: `tm session events` needs a UUID for `GET /sessions/{id}/events`, but
/// operators may pass a friendly `tm-*`/`tmpm-*` name. Phase 1B collapsed the bespoke
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
/// Why (#2457): the single entry point the `session` dispatcher calls for
/// every managed verb — it keeps the dispatcher a thin match and concentrates
/// the map → execute → render flow here. Before this fix a failed spawn (or
/// any other managed verb) — an unreachable daemon, a rejected spawn, a 404
/// deliverable-not-found — came back from [`CommandExecutor::execute`] as
/// `CommandResult::Error`, which this function printed and then returned
/// `Ok(true)` regardless: the process exited 0 even though the operation
/// failed, so scripts/CI checking the exit code silently treated the failure
/// as success.
/// What: maps `action` via [`to_command`], runs it on a [`CommandExecutor`]
/// built from the CLI's client. A [`CommandResult::Error`] is now propagated
/// as `Err` (via the same `anyhow` path every other CLI failure uses) instead
/// of merely being printed, so `main`'s default error handler reports it and
/// exits non-zero; every other result is rendered via [`render_cli`] and
/// printed to stdout as before. Returns `Ok(false)` when `action` is not a
/// managed verb this module routes (so the caller can handle it on the
/// project path); `Ok(true)` once it has handled and printed a successful
/// result.
/// Test: parsing/mapping by `to_command_maps_*`; rendering by `render_cli_*`;
/// `run_propagates_error_result_as_err` covers the exit-code fix; the HTTP
/// path by the executor tests and `tests/session_manager_mvp.rs`.
pub(crate) async fn run(
    client: &reqwest::Client,
    url: &str,
    action: &SessionAction,
) -> anyhow::Result<bool> {
    let Some(cmd) = to_command(action) else {
        return Ok(false);
    };
    let result = executor(client, url).execute(cmd).await;
    if let CommandResult::Error(msg) = &result {
        anyhow::bail!("{msg}");
    }
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
            no_inject: false,
            deliverable: None,
        };
        match to_command(&action) {
            Some(TrustyCommand::ManagedNew {
                repo_url,
                git_ref,
                task,
                name_hint,
                runtime,
                inject_task,
                deliverable_id,
                force_new,
            }) => {
                assert_eq!(repo_url, "https://example/r.git");
                assert_eq!(git_ref, "main");
                assert_eq!(task, "do it");
                assert_eq!(name_hint.as_deref(), Some("api"));
                assert_eq!(runtime.as_deref(), Some("claude-code"));
                // Default (no `--no-inject`) → field omitted so the daemon's
                // turnkey default (inject) applies.
                assert_eq!(inject_task, None);
                // Default (no `--deliverable`) → no link (#2379).
                assert_eq!(deliverable_id, None);
                // #2450: the explicit `new` verb forces a fresh session (never
                // adopts an existing live one for the same project).
                assert!(force_new, "session new must force a fresh session");
            }
            other => panic!("expected ManagedNew, got {other:?}"),
        }
    }

    #[test]
    fn to_command_new_no_inject_sets_metadata_only() {
        // `--no-inject` maps to `inject_task: Some(false)` (metadata-only, #1903).
        let action = SessionAction::New {
            repo: "https://example/r.git".to_string(),
            git_ref: "main".to_string(),
            task: "do it".to_string(),
            name_hint: None,
            runtime: trusty_mpm::runtime::RuntimeKind::ClaudeCode,
            no_inject: true,
            deliverable: None,
        };
        match to_command(&action) {
            Some(TrustyCommand::ManagedNew { inject_task, .. }) => {
                assert_eq!(inject_task, Some(false));
            }
            other => panic!("expected ManagedNew, got {other:?}"),
        }
    }

    #[test]
    fn to_command_new_deliverable_maps_to_deliverable_id() {
        // `--deliverable <id>` maps to `deliverable_id: Some(<id>)` (#2379).
        let action = SessionAction::New {
            repo: "https://example/r.git".to_string(),
            git_ref: "main".to_string(),
            task: "do it".to_string(),
            name_hint: None,
            runtime: trusty_mpm::runtime::RuntimeKind::ClaudeCode,
            no_inject: false,
            deliverable: Some("11111111-1111-1111-1111-111111111111".to_string()),
        };
        match to_command(&action) {
            Some(TrustyCommand::ManagedNew { deliverable_id, .. }) => {
                assert_eq!(
                    deliverable_id,
                    Some("11111111-1111-1111-1111-111111111111".to_string())
                );
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
            to_command(&SessionAction::Ls {
                terms: vec![],
                json: false,
                source_id: None,
                current: false,
                all: false,
                no_prune: false,
            }),
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
            slot: 0,
            deleted: false,
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
            workspace_removed: None,
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
            workspace_removed: None,
        };
        assert_eq!(render_cli(&resume), "resumed alpha (m-1) [Running]");
    }

    /// Build a decommission result carrying `verdict` as the daemon's verdict.
    fn decommissioned(verdict: Option<bool>) -> CommandResult {
        CommandResult::ManagedLifecycle {
            id: "m-1".into(),
            name: "alpha".into(),
            state: "Decommissioned".into(),
            action: "decommissioned".into(),
            workspace_removed: verdict,
        }
    }

    /// #5899: the rendered decommission line must follow the daemon's verdict, and
    /// only `Some(true)` may claim removal. The `Some(false)` arm is the whole bug
    /// — the CLI printed "workspace removed" for a worktree still on disk — so it
    /// gets an explicit assertion, as does the no-verdict arm.
    #[test]
    fn decommission_message_honours_every_verdict() {
        assert_eq!(
            render_cli(&decommissioned(Some(true))),
            "decommissioned m-1 — workspace removed; tombstone record kept"
        );

        let not_removed = render_cli(&decommissioned(Some(false)));
        assert_eq!(
            not_removed,
            "decommissioned m-1 — tombstone record kept; workspace NOT removed (still on disk)"
        );
        assert!(
            !not_removed.contains("workspace removed"),
            "a `workspace_removed: false` verdict must never read as removal: {not_removed}"
        );

        let unknown = render_cli(&decommissioned(None));
        assert_eq!(
            unknown,
            "decommissioned m-1 — tombstone record kept; workspace outcome not reported by the daemon"
        );
        assert!(
            !unknown.contains("workspace removed"),
            "an absent verdict must not be rendered as removal: {unknown}"
        );
    }

    /// #5899 regression: `tm session decommission <id>` must not print the removal
    /// message while the workspace is still on disk.
    ///
    /// This is the reported scenario reproduced whole — a session whose workspace tm
    /// does not own (the adopt / local-path shape), decommissioned through
    /// `TrustyCommand::ManagedDecommission` on the real daemon route, rendered by
    /// [`render_cli`]. The daemon side was already correct and already covered; what
    /// broke was the client dropping the verdict, so the assertion is on the CLI's
    /// own output, checked against the filesystem rather than against a fixture.
    /// Nothing here touches tmux (the isolated test daemon uses a no-op driver) or
    /// git, so it is deterministic.
    ///
    /// Against the pre-fix commit it fails: the workspace survives and the CLI
    /// announces "workspace removed" anyway. The opposite direction — the removal
    /// message appearing when the workspace IS gone — is covered end-to-end by
    /// `executor_decommission_reports_daemon_workspace_verdict`, which asserts the
    /// verdict against the filesystem in both directions, and exhaustively over the
    /// three verdicts by `decommission_message_honours_every_verdict`.
    #[tokio::test]
    async fn decommission_cli_message_never_claims_removal_when_workspace_remains() {
        use std::future::IntoFuture as _;

        use trusty_mpm::client::{CommandExecutor, TrustyCommand};
        use trusty_mpm::daemon::{api, state::DaemonState};
        use trusty_mpm::runtime::RuntimeKind;
        use trusty_mpm::session_manager::ManagedSessionId;

        let root = tempfile::tempdir().unwrap().keep();
        let state =
            std::sync::Arc::new(DaemonState::with_root_isolated_managed(root.clone()).await);
        let id = ManagedSessionId::new();
        let ws = root.join(format!("{id}-unowned-ws"));
        std::fs::create_dir_all(&ws).expect("create seeded workspace");
        state
            .session_manager()
            .await
            .create_with_id(
                id,
                "regression: #5899 decommission workspace verdict".to_string(),
                Some(ws.clone()),
                None,
                Some(ws.clone()),
                None,
                None,
                RuntimeKind::default(),
                false,
                // Unowned: the daemon must refuse to delete this workspace.
                false,
            )
            .await
            .expect("seed session");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, api::router(state)).into_future());

        let result = CommandExecutor::new(format!("http://{addr}"))
            .execute(TrustyCommand::ManagedDecommission {
                target: id.to_string(),
            })
            .await;
        let rendered = render_cli(&result);

        assert!(
            ws.exists(),
            "fixture invariant: decommission must NOT delete an unowned workspace \
             at {}",
            ws.display()
        );
        assert!(
            !rendered.contains("workspace removed"),
            "the CLI claimed removal while the workspace is still on disk at {}: \
             {rendered}",
            ws.display()
        );
        assert!(
            rendered.contains("NOT removed"),
            "the operator must be told the workspace survived: {rendered}"
        );
    }

    #[test]
    fn render_cli_error() {
        assert_eq!(
            render_cli(&CommandResult::Error("managed session x not found".into())),
            "managed session x not found"
        );
    }

    /// #2457: `run` must propagate a `CommandResult::Error` as `Err`, not
    /// print it and return `Ok(true)` — that was the exact bug (a failed
    /// `sessions new`, or any other managed verb routed here, exited 0).
    /// Port 1 on loopback is not listening, so this deterministically drives
    /// the executor's "daemon unreachable" error path without needing a real
    /// daemon.
    #[tokio::test]
    async fn run_propagates_error_result_as_err() {
        let client = reqwest::Client::new();
        let action = SessionAction::Send {
            id: "nonexistent".into(),
            text: "hi".into(),
        };
        let err = run(&client, "http://127.0.0.1:1", &action)
            .await
            .expect_err("a CommandResult::Error must propagate as Err, not be swallowed");
        assert!(
            err.to_string().contains("daemon unreachable"),
            "unexpected error: {err}"
        );
    }
}
