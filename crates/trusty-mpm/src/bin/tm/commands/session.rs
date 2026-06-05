//! `session` command handler.
//!
//! Why: session lifecycle operations (start, stop, list, clean, info, events,
//! breakers, pause, resume, run, output, instructions) form a cohesive group
//! that benefits from a dedicated file.
//! What: the `session` dispatcher function and its private helpers
//! `resolve_session_id`, `compose_session_instructions`.
//! Test: `cli_parses_session_*`, `compose_session_instructions_*` in `tests.rs`.

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
/// `Info` resolve a session by id or friendly name; `List` and `Clean` scope
/// to the project directory.
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
                        .args(["send-keys", "-t", &body.name, "claude", "Enter"])
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
                None => println!("session '{id_or_name}' not found"),
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
            let id = match resolve_session_id(client, url, &id_or_name).await? {
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
    }
    Ok(())
}

/// Resolve a session id-or-name to its UUID string via `GET /sessions`.
///
/// Why: `session events` calls `GET /sessions/{id}/events`, which requires a
/// UUID; operators may pass a friendly `tmpm-<adj>-<noun>` name instead, so the
/// name must be resolved against the live session list first.
/// What: fetches `GET /sessions`, matching `id.0` or `tmux_name` against the
/// argument; returns `Some(uuid)` on a hit, `None` when no session matches.
/// Test: covered indirectly by `cli_parses_session_events`.
pub(crate) async fn resolve_session_id(
    client: &reqwest::Client,
    url: &str,
    id_or_name: &str,
) -> anyhow::Result<Option<String>> {
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
    let found = body.sessions.iter().find_map(|s| {
        // `SessionId` is a single-field newtype: serde serializes it as a bare
        // UUID string, so `id` is read directly (not as `{"0": ...}`).
        let uuid = s.get("id").and_then(|v| v.as_str());
        let name = s.get("tmux_name").and_then(|v| v.as_str());
        match uuid {
            Some(u) if u == id_or_name || name == Some(id_or_name) => Some(u.to_string()),
            _ => None,
        }
    });
    Ok(found)
}

/// Run the instruction merge pipeline and stash the override-resolved PM prompt.
///
/// Why: `session start` and `session instructions` both need the effective PM
/// prompt — the text actually delivered to `claude --append-system-prompt-file`.
/// The old code returned `output.merged` (the legacy pipeline: INSTRUCTIONS.md +
/// delegation authority + CLAUDE.md) for display, while stashing `resolve_pm_prompt`
/// separately. That caused `tm session instructions` to print content that differed
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

    // The single source of truth for the live PM prompt is `resolve_pm_prompt`.
    // Writing it to the stash AND returning it as the display string ensures that
    // `tm session instructions` always shows exactly what `claude` received (the
    // #382 fix). Previously this function returned `output.merged` for display,
    // which came from the old pipeline (INSTRUCTIONS.md + delegation + CLAUDE.md)
    // and differed from the stash, causing the visible divergence.
    let resolved_prompt = trusty_mpm::core::instruction_overrides::resolve_pm_prompt(project_dir);
    let stash_dir = project_dir.join(".trusty-mpm");
    std::fs::create_dir_all(&stash_dir)?;
    let stash = stash_dir.join("last-instructions.md");
    std::fs::write(&stash, &resolved_prompt)?;

    Ok((resolved_prompt, output, stash))
}
