//! `session` command handler.
//!
//! Why: session lifecycle operations (start, stop, list, clean, info, events,
//! breakers, pause, resume, catchup, run, output, instructions) form a cohesive
//! group that benefits from a dedicated file.
//! What: the `session` dispatcher function and its private helpers
//! `emit_managed_alias_notice`, `compose_session_instructions`. The managed
//! verbs (`new`/`ls`/`send`/`answer`/`attach`/`stop`/`resume`/`decommission`)
//! route through the shared chat-core layer (`commands::managed_route`); the
//! id-or-name resolution for both managed and project sessions goes through the
//! one canonical `client::resolve_target`. `catchup` is handled by the
//! DOC-28 cutover bridge in the `catchup` submodule.
//! Test: `cli_parses_session_*`, `cli_parses_session_catchup`,
//! `compose_session_instructions_*` in `tests.rs`.

// CUTOVER BRIDGE submodule — remove post-migration (#1762)
pub(crate) mod catchup;

use serde::Deserialize;

use crate::cli::SessionAction;
use crate::commands::project::resolve_dir;
use crate::formatters::session::{event_summary, print_compression_stats, short_id};
use crate::types::{EventRow, SessionRow};

/// `session` subcommand — define and manage sessions within a project.
///
/// Why: a session is a Claude Code instance; operators start, stop, list,
/// reap, and inspect them per project from the shell.
/// What: `Start` posts `POST /sessions` with the project path; `Stop` and
/// `Resume` are managed-aware (#1218) — they route to the managed runtime-stop/
/// resume endpoints when the id/name resolves to a managed session, falling back
/// to the project-session path otherwise; `Info` resolves a session by id or
/// friendly name; `List` and `Clean` scope to the project directory.
/// Test: `cli_parses_session_start`, `cli_parses_session_stop`,
/// `cli_parses_session_list`, `cli_parses_session_clean`,
/// `cli_parses_session_info`.
pub(crate) async fn session(
    client: &reqwest::Client,
    url: &str,
    action: SessionAction,
) -> anyhow::Result<()> {
    match action {
        SessionAction::Start { dir } => {
            let path = resolve_dir(dir)?;
            // Prepare the custom instructions Claude Code reads at startup:
            // deploy composed agents to `~/.claude/agents/` and merge the
            // project CLAUDE.md. This shared prep is what makes a plain
            // `claude` process behave as a trusty-mpm session.
            let fw = trusty_mpm::core::paths::FrameworkPaths::default();
            match trusty_mpm::core::session_launch::prepare_session(&fw, &path) {
                Ok(report) => {
                    println!(
                        "Agents: {} deployed, {} skipped, {} unchanged",
                        report.deploy.deployed.len(),
                        report.deploy.skipped.len(),
                        report.deploy.unchanged.len(),
                    );
                    if report.instructions.claude_md_created {
                        println!("  Created CLAUDE.md stub in {}", path.display());
                    }
                    println!(
                        "Instructions: {} agents in delegation authority",
                        report.instructions.agent_count
                    );
                    println!(
                        "  Merged instructions written to {}",
                        report.stash.display()
                    );
                    // DOC-28 cutover bridge: print catch-up digest as seed context.
                    // CUTOVER BRIDGE — remove post-migration (#1762)
                    if let Some(ctx) = report.catchup_context {
                        println!("\n---\n\n## Recent Activity (catch-up)\n\n{ctx}");
                    }
                }
                Err(err) => eprintln!("warning: session preparation failed: {err}"),
            }

            #[derive(Deserialize)]
            struct Body {
                #[serde(default)]
                name: String,
            }
            let body: Body = client
                .post(format!("{url}/sessions"))
                .json(&serde_json::json!({
                    "project": path,
                    "project_path": path,
                }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            // The daemon only registers session state now — it no longer
            // spawns the tmux host (that caused session proliferation). The
            // CLI owns the actual launch: create a detached tmux session in
            // the project directory and start `claude` in it.
            let workdir = path.to_string_lossy().to_string();
            let new_session = std::process::Command::new("tmux")
                .args(["new-session", "-d", "-s", &body.name, "-c", &workdir])
                .status();
            match new_session {
                Ok(status) if status.success() => {
                    let send = std::process::Command::new("tmux")
                        .args([
                            "send-keys",
                            "-t",
                            &body.name,
                            &format!(
                                "claude {}",
                                trusty_mpm::core::model_inject::PERMISSION_MODE_FLAG
                            ),
                            "Enter",
                        ])
                        .status();
                    match send {
                        Ok(s) if s.success() => {
                            println!("started session {} (tmux + claude)", body.name);
                        }
                        Ok(_) | Err(_) => {
                            eprintln!(
                                "warning: tmux session {} created but failed to start claude",
                                body.name
                            );
                            println!("started session {}", body.name);
                        }
                    }
                }
                Ok(_) | Err(_) => {
                    eprintln!(
                        "warning: failed to create tmux session {}; run `claude` manually in {}",
                        body.name, workdir
                    );
                    println!("started session {}", body.name);
                }
            }
        }
        SessionAction::Stop { id_or_name } => {
            // #1218: `stop` is managed-aware. If the argument resolves to a
            // MANAGED session (by id or friendly name) via the canonical
            // chat-core resolver, route to the managed runtime-stop endpoint;
            // otherwise fall back to the project-session DELETE path. This keeps
            // one intuitive verb that does the right thing for both families (the
            // #842 driver skill documents `stop`).
            if let Some(managed_id) =
                crate::commands::managed_route::resolve_managed_match(client, url, &id_or_name)
                    .await
            {
                crate::commands::managed::session_stop(client, url, managed_id).await?;
            } else {
                let resp = client
                    .delete(format!("{url}/sessions/{id_or_name}"))
                    .send()
                    .await?;
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    println!("not found");
                } else {
                    resp.error_for_status()?;
                    println!("stopped {id_or_name}");
                }
            }
        }
        SessionAction::List { dir } => {
            let path = resolve_dir(dir)?;
            #[derive(Deserialize)]
            struct Body {
                sessions: Vec<SessionRow>,
            }
            let body: Body = client
                .get(format!("{url}/sessions"))
                .query(&[("project", path.to_string_lossy().as_ref())])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if body.sessions.is_empty() {
                println!("no sessions for {}", path.display());
            }
            for s in &body.sessions {
                let status = s.status.as_str().unwrap_or("unknown");
                println!("{} {} {}", short_id(&s.id), status, s.workdir);
            }
        }
        SessionAction::Tui {
            url: tui_url,
            interval_ms,
        } => {
            // #1392: the coordinator TUI is `tm sessions tui` (formerly the
            // top-level `tm coordinator-tui`). It polls the daemon live, so its
            // `run` is async and runs on the tokio runtime like `tui::run`.
            // `resolve_daemon_url` honours an explicit `--url`, then the lock
            // file, then the default — so we resolve from this subcommand's own
            // `--url`/`TRUSTY_MPM_URL` flag rather than the dispatcher's `url`.
            let resolved = trusty_mpm::core::resolve_daemon_url(tui_url.as_deref());
            trusty_mpm::tui::coordinator::run(resolved, interval_ms).await?;
        }
        SessionAction::Clean { dir } => {
            // `dir` is accepted for symmetry; the daemon reaps globally.
            let _ = resolve_dir(dir)?;
            let body: serde_json::Value = client
                .delete(format!("{url}/sessions/dead"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let removed = body.get("removed").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("reaped {removed} dead session(s)");
        }
        SessionAction::Info { id_or_name } => {
            #[derive(Deserialize)]
            struct Body {
                sessions: Vec<serde_json::Value>,
            }
            let body: Body = client
                .get(format!("{url}/sessions"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let found = body.sessions.iter().find(|s| {
                let id_match = s
                    .get("id")
                    .and_then(|v| v.get("0"))
                    .and_then(|v| v.as_str())
                    == Some(id_or_name.as_str());
                let name_match =
                    s.get("tmux_name").and_then(|v| v.as_str()) == Some(id_or_name.as_str());
                id_match || name_match
            });
            match found {
                Some(s) => println!("{}", serde_json::to_string_pretty(s)?),
                None => {
                    // Fall back: the id/name may belong to a MANAGED session
                    // (SM sessions live in the managed store, not the project-session
                    // store). Fetch the managed list and search there too before
                    // giving up. This fixes the gap where `tm sessions info <uuid>`
                    // printed "not found" for sessions visible in `tm sessions ls`.
                    let managed = info_from_managed_store(client, url, &id_or_name).await;
                    match managed {
                        Some(val) => println!("{}", serde_json::to_string_pretty(&val)?),
                        None => println!("session '{id_or_name}' not found"),
                    }
                }
            }
        }
        SessionAction::Instructions { dir } => {
            // Pure local computation — no daemon round-trip needed.
            let path = resolve_dir(dir)?;
            let fw = trusty_mpm::core::paths::FrameworkPaths::default();
            // `resolved_prompt` is the same text written to the stash and
            // passed to `claude --append-system-prompt-file` — the single
            // source of truth for what Claude received (issue #382).
            let (resolved_prompt, _output, _stash) = compose_session_instructions(&fw, &path)?;
            print!("{resolved_prompt}");
        }
        SessionAction::Events { id_or_name } => {
            let id = match crate::commands::managed_route::resolve_project_session_id(
                client,
                url,
                &id_or_name,
            )
            .await?
            {
                Some(id) => id,
                None => {
                    println!("session '{id_or_name}' not found");
                    return Ok(());
                }
            };
            #[derive(Deserialize)]
            struct Body {
                events: Vec<EventRow>,
            }
            let body: Body = client
                .get(format!("{url}/sessions/{id}/events/poll"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if body.events.is_empty() {
                println!("no events for session {id_or_name}");
            }
            for e in &body.events {
                println!("{} {} {}", e.at, e.event, event_summary(&e.payload));
            }
        }
        SessionAction::Breakers => {
            #[derive(Deserialize)]
            struct Row {
                agent: String,
                breaker: serde_json::Value,
            }
            #[derive(Deserialize)]
            struct Body {
                breakers: Vec<Row>,
            }
            let body: Body = client
                .get(format!("{url}/breakers"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if body.breakers.is_empty() {
                println!("no circuit breakers");
            } else {
                println!("{:<24} {:<12} FAILURES", "AGENT", "STATE");
                for r in &body.breakers {
                    let state = r
                        .breaker
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let failures = r
                        .breaker
                        .get("consecutive_failures")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    println!("{:<24} {:<12} {}", r.agent, state, failures);
                }
            }
        }
        // DOC-28 cutover bridge — dispatches to session/catchup.rs
        // CUTOVER BRIDGE — remove post-migration (#1762)
        SessionAction::Catchup { all_projects, full } => {
            catchup::handle_catchup(all_projects, full).await?;
        }
        SessionAction::Pause { id_or_name, note } => {
            let resp = client
                .post(format!("{url}/sessions/{id_or_name}/pause"))
                .json(&serde_json::json!({ "summary": note }))
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                println!("session '{id_or_name}' not found");
            } else {
                let body: serde_json::Value = resp.error_for_status()?.json().await?;
                let summary = body.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                println!("paused {id_or_name}: {summary}");
            }
        }
        SessionAction::Resume { id_or_name } => {
            // #1218: `resume` is managed-aware (mirrors `Stop`). A managed
            // session id/name routes (via the canonical chat-core resolver) to
            // the managed resume endpoint; anything else falls back to the
            // project-session pause/resume path.
            if let Some(managed_id) =
                crate::commands::managed_route::resolve_managed_match(client, url, &id_or_name)
                    .await
            {
                crate::commands::managed::session_resume(client, url, managed_id).await?;
            } else {
                let resp = client
                    .post(format!("{url}/sessions/{id_or_name}/resume"))
                    .send()
                    .await?;
                match resp.status() {
                    reqwest::StatusCode::NOT_FOUND => {
                        println!("session '{id_or_name}' not found");
                    }
                    reqwest::StatusCode::CONFLICT => {
                        println!("session '{id_or_name}' is not paused");
                    }
                    _ => {
                        resp.error_for_status()?;
                        println!("resumed {id_or_name}");
                    }
                }
            }
        }
        SessionAction::Run {
            id_or_name,
            command,
            summarize,
        } => {
            let mut req = client.post(format!("{url}/sessions/{id_or_name}/command"));
            if summarize {
                req = req.query(&[("compress", "summarise")]);
            }
            let resp = req
                .json(&serde_json::json!({ "command": command }))
                .send()
                .await?;
            match resp.status() {
                reqwest::StatusCode::NOT_FOUND => {
                    println!("session '{id_or_name}' not found");
                }
                reqwest::StatusCode::CONFLICT => {
                    println!("session '{id_or_name}' is stopped");
                }
                _ => {
                    let body: serde_json::Value = resp.error_for_status()?.json().await?;
                    let output = body.get("output").and_then(|v| v.as_str()).unwrap_or("");
                    print!("{output}");
                    print_compression_stats(&body);
                }
            }
        }
        SessionAction::Output {
            id_or_name,
            lines,
            summarize,
        } => {
            let mut query: Vec<(&str, String)> = vec![("lines", lines.to_string())];
            if summarize {
                query.push(("compress", "summarise".to_string()));
            }
            let resp = client
                .get(format!("{url}/sessions/{id_or_name}/output"))
                .query(&query)
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                println!("session '{id_or_name}' not found");
            } else {
                let body: serde_json::Value = resp.error_for_status()?.json().await?;
                let output = body.get("output").and_then(|v| v.as_str()).unwrap_or("");
                print!("{output}");
                print_compression_stats(&body);
            }
        }
        // ── Managed session-manager actions ──────────────────────────────────
        // All `ls` variants are routed through the direct HTTP path so the
        // `?source_id=` filter and the extended table columns (task, created_at)
        // work regardless of the `--json` flag. `--json` keeps the raw
        // daemon-JSON passthrough byte-for-byte (scripts rely on this).
        SessionAction::Ls {
            json,
            source_id,
            current,
        } => {
            let sid: Option<String> = if current {
                derive_source_id_from_cwd()
            } else {
                source_id
            };
            crate::commands::managed::session_ls(client, url, json, sid.as_deref()).await?
        }
        // `activity` stays on the raw path: its CLI output carries confidence,
        // token, cache, and latency detail that `CommandResult::ManagedActivity`
        // does not model, so routing it through chat-core would regress output.
        SessionAction::Activity { id } => {
            crate::commands::managed::session_activity(client, url, id).await?
        }
        SessionAction::PruneIdle { dry_run, json } => {
            crate::commands::prune::prune_idle(client, url, dry_run, json).await?
        }
        // #1508: bulk teardown + by-state prune go through direct HTTP (like
        // `PruneIdle`), not chat-core — they are fleet-wide store operations, not
        // single-session intents.
        SessionAction::DecommissionEphemeral => {
            crate::commands::managed::session_decommission_ephemeral(client, url).await?
        }
        SessionAction::Prune {
            state,
            dry_run,
            include_active,
        } => {
            crate::commands::managed::session_prune(client, url, state, dry_run, include_active)
                .await?
        }
        // The deprecated verbose aliases emit their deprecation notice, then
        // route through chat-core exactly like their canonical verb (#1205).
        action @ (SessionAction::ManagedStop { .. }
        | SessionAction::RuntimeStop { .. }
        | SessionAction::ManagedResume { .. }) => {
            emit_managed_alias_notice(&action);
            // `to_command` maps every one of these aliases; `run` therefore
            // always handles it.
            crate::commands::managed_route::run(client, url, &action).await?;
        }
        // Every remaining managed verb (`new`/`ls`/`send`/`answer`/`attach`/
        // `decommission`) routes through the shared chat-core layer. `run`
        // returns `false` only for non-managed variants, which the arms above
        // already handled — so an unrouted variant here is a wiring bug.
        action => {
            debug_assert!(
                crate::commands::managed_route::to_command(&action).is_some(),
                "unrouted session action reached the managed fallthrough: {action:?}"
            );
            crate::commands::managed_route::run(client, url, &action).await?;
        }
    }
    Ok(())
}

/// Emit the deprecation notice for a renamed managed lifecycle alias (#1205).
///
/// Why: `managed-stop`/`runtime-stop`/`managed-resume` were renamed to the
/// cleaner `stop`/`resume` family; the old spellings still parse but every
/// invocation must nudge the operator toward the canonical verb. Centralizing the
/// old→new mapping here keeps the routing arm a thin match.
/// What: writes `warning: '<old>' is deprecated; use '<new>'` to stderr for the
/// three deprecated aliases; a no-op for any other action.
/// Test: the message text is asserted by `deprecation_notice_format`; the alias
/// parse paths by `cli_parses_session_managed_stop`/`_runtime_stop`/`_managed_resume`.
fn emit_managed_alias_notice(action: &SessionAction) {
    let pair = match action {
        SessionAction::ManagedStop { .. } => Some(("managed-stop", "stop")),
        SessionAction::RuntimeStop { .. } => Some(("runtime-stop", "stop")),
        SessionAction::ManagedResume { .. } => Some(("managed-resume", "resume")),
        _ => None,
    };
    if let Some((old, new)) = pair {
        crate::commands::managed::deprecation_notice(old, new);
    }
}

/// Derive the `owner/repo` source_id from a git directory for `--current`.
///
/// Why: extracted from `derive_source_id_from_cwd` so the git-remote resolution
/// is unit-testable with a path argument rather than the process cwd (which would
/// require `std::env::set_current_dir`, a process-global, non-thread-safe call).
/// What: runs `git -C <dir> config --get remote.origin.url` and parses the result
/// via `parse_github_path`. Returns `Some("owner/repo")` on success; `None` when
/// not in a git repo or the remote URL is not a GitHub URL.
/// Test: `derive_source_id_from_cwd_returns_none_without_git` (unit).
fn derive_source_id_from_path(dir: &std::path::Path) -> Option<String> {
    let url = trusty_mpm::daemon::managed_routes::inproject::get_origin_url(dir)?;
    let gh = trusty_common::github_path::parse_github_path(&url)?;
    Some(format!("{}/{}", gh.owner, gh.repo))
}

/// Derive the `owner/repo` source_id from the process cwd for `--current`.
///
/// Why: thin wrapper over `derive_source_id_from_path` for the CLI call site;
/// separating cwd resolution from git-remote parsing keeps the impl testable.
/// What: reads `std::env::current_dir()` and delegates to `derive_source_id_from_path`.
/// Returns `Some("owner/repo")` on success; `None` on any failure (both cases silently
/// ignored — the caller treats None as "no filter").
/// Test: `derive_source_id_from_cwd_returns_none_without_git` (unit via path variant).
fn derive_source_id_from_cwd() -> Option<String> {
    derive_source_id_from_path(&std::env::current_dir().ok()?)
}

/// Return true when `session` matches `id_or_name` by id or tmux name.
///
/// Why: extracted from `info_from_managed_store` so the match predicate is
/// unit-testable without requiring an HTTP call.
/// What: compares `session["id"]` and `session["name"]` against `id_or_name`;
/// returns true on either match.
/// Test: `info_managed_fallback_matches_by_id_and_name`.
fn matches_session(session: &serde_json::Value, id_or_name: &str) -> bool {
    let id_match = session.get("id").and_then(|v| v.as_str()) == Some(id_or_name);
    let name_match = session.get("name").and_then(|v| v.as_str()) == Some(id_or_name);
    id_match || name_match
}

/// Fetch the managed session list and find a session matching `id_or_name`.
///
/// Why: `tm sessions info` queries the project-session store first; managed
/// sessions live in a separate store and return 404 there. This fallback
/// prevents the confusing "not found" message for sessions that ARE visible in
/// `tm sessions ls`. Non-404 HTTP errors are logged as warnings so daemon 5xx
/// responses are not silently swallowed as "not found".
/// What: GETs `/api/v1/sessions/managed`, searches by id-exact then name-exact
/// via `matches_session`, returns the first match as a `serde_json::Value`.
/// Returns `None` when the daemon is unreachable, returns a non-success status,
/// or no session matches.
/// Test: `info_managed_fallback_matches_by_id_and_name` (unit on match predicate);
/// HTTP path covered by the integration test.
async fn info_from_managed_store(
    client: &reqwest::Client,
    url: &str,
    id_or_name: &str,
) -> Option<serde_json::Value> {
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed"))
        .send()
        .await
        .ok()?;
    let status = resp.status();
    if !status.is_success() {
        if status != reqwest::StatusCode::NOT_FOUND {
            tracing::warn!(
                "managed store lookup failed with HTTP {status} — \
                 'session not found' may mask a daemon error"
            );
        }
        return None;
    }
    let raw = resp.text().await.ok()?;
    let resp: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let sessions = resp.get("sessions")?.as_array()?;
    sessions
        .iter()
        .find(|s| matches_session(s, id_or_name))
        .cloned()
}

/// Run the instruction merge pipeline and stash the override-resolved PM prompt.
///
/// Why: `session start` and `session instructions` both need the effective PM
/// prompt — the text actually delivered to `claude --append-system-prompt-file`.
/// The old code returned `output.merged` (the legacy pipeline: INSTRUCTIONS.md +
/// delegation authority + CLAUDE.md) for display, while stashing `resolve_pm_prompt`
/// separately. That caused `tm sessions instructions` to print content that differed
/// from what Claude received, which is exactly the divergence issue #382 describes.
/// The single source of truth for "what claude receives" is `resolve_pm_prompt`;
/// the display and the stash must both come from it.
/// What: builds a [`PipelineInput`] and runs [`build_instructions`] to ensure
/// `CLAUDE.md` is seeded (the side-effect we still need); resolves the PM prompt
/// via [`crate::core::instruction_overrides::resolve_pm_prompt`]; writes it to
/// `<project>/.trusty-mpm/last-instructions.md`; returns the resolved prompt text,
/// the `PipelineOutput` metadata flags, and the stash path.
/// Test: `compose_session_instructions_display_matches_stash`,
/// `compose_session_instructions_display_matches_live_prompt`.
pub(crate) fn compose_session_instructions(
    fw: &trusty_mpm::core::paths::FrameworkPaths,
    project_dir: &std::path::Path,
) -> anyhow::Result<(
    String,
    trusty_mpm::core::instruction_pipeline::PipelineOutput,
    std::path::PathBuf,
)> {
    use trusty_mpm::core::instruction_pipeline::{PipelineInput, build_instructions};

    // Run the legacy pipeline for its side-effects: seed CLAUDE.md if absent
    // and populate the metadata flags (agent_count, claude_md_created, …).
    let input = PipelineInput {
        framework_instructions_path: fw.framework_instructions_path(),
        agents_dir: fw.claude_agents_dir(),
        claude_md_path: project_dir.join("CLAUDE.md"),
    };
    let output = build_instructions(&input)?;

    // The single source of truth for the live PM prompt is
    // `build_system_prompt_for`, NOT the bare `resolve_pm_prompt`. The launcher
    // applies HR-4 output-style version-fallback injection on top of the resolved
    // prompt (issue #1409), so `tm sessions instructions` must show — and the
    // stash must hold — that SAME injected text. Writing the pre-injection
    // `resolve_pm_prompt` here made the display/stash diverge from the real launch
    // prompt whenever `claude` was absent/old (injection fires), the same #382
    // divergence this function was written to prevent. Routing through
    // `build_system_prompt_for` keeps display, stash, and launch identical
    // regardless of Claude Code version.
    let resolved_prompt = trusty_mpm::core::session_launch::build_system_prompt_for(project_dir);
    let stash_dir = project_dir.join(".trusty-mpm");
    std::fs::create_dir_all(&stash_dir)?;
    let stash = stash_dir.join("last-instructions.md");
    std::fs::write(&stash, &resolved_prompt)?;

    Ok((resolved_prompt, output, stash))
}

// Unit tests live in session_tests.rs (test-file budget: 1500 SLOC).
#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
