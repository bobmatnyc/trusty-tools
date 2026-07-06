//! `tcode run-task <agent> <task>` — the M1 cut-line "replay via thin CLI"
//! verb (#2060, vision spec §13).
//!
//! Why: this is the subcommand the cut line names explicitly — a human runs
//! a task via the CLI and SEES it happen, driven entirely over the daemon's
//! JSON-RPC surface (create-or-target a session, `task.run`, `session.attach`,
//! stream events) rather than the legacy in-process `AgentLoop` execution
//! `main.rs`'s `--legacy-in-process` flag still offers (see its module
//! docs for why that path was kept, not removed).
//! What: [`run`] spawns one ephemeral daemon for the whole invocation's
//! lifetime — `task.run` (mints the session), `session.attach` (replay,
//! which will be small-to-empty since attach happens immediately after
//! `task.run` returns), then a poll loop over
//! `StdioRpcClient::next_notification` printing each event via
//! `cli_client::render::render_event_line` (human mode) until
//! `session_done`, a final `session.status` call for the terminal snapshot,
//! and `cli_client::render::exit_code_for_status` for the process exit code.
//! `--json` suppresses the live per-event lines and prints only the final
//! `Session` snapshot as pretty JSON, matching the legacy path's `--json`
//! contract (a single JSON document on stdout). `--mode` (#2059) is passed
//! straight through as `task.run`'s own `mode` param — this function does
//! NOT resolve or validate it; the daemon's `crate::mode::resolve_mode`
//! (highest to lowest: `TRUSTY_CODE_MODE` > this param >
//! `.claude/settings.json` > default) is the single source of truth, and
//! the resolved value is printed back (human line and `--json` output both
//! include `Session.mode`).
//! Test: `tests/cli_e2e.rs::run_task_streams_live_events_and_reports_final_status`
//! (the required black-box "replay via thin CLI" case, driven with
//! `TCODE_MOCK_LLM=echo`).

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use trusty_code::cli_client::StdioRpcClient;
use trusty_code::cli_client::render::{exit_code_for_status, render_event_line};
use trusty_code::events::Event;
use trusty_code::session::Session;

use super::tcode_exe;

/// How often the streaming loop wakes up to re-check for a terminal event.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// `tcode run-task <AGENT> <TASK> [--project P] [--json] [--engineer-model M]
/// [--mode daily-driver|parity]`.
///
/// Why/What: see module docs. Returns the process exit code the caller
/// should use (mirrors the legacy path's `ExitCode` contract via
/// `exit_code_for_status`).
pub async fn run(
    project: &Path,
    agent: &str,
    task: &str,
    json_output: bool,
    engineer_model: Option<String>,
    mode: Option<String>,
) -> Result<i32> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;

    let run_params = json!({
        "task_description": task,
        "agent_name": agent,
        "model_override": engineer_model,
        "mode": mode,
    });
    let run_result = client.call("task.run", run_params).await?;
    let session_id = run_result
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("tcode run-task: daemon did not return a session_id"))?
        .to_string();

    let attach_result = client
        .call("session.attach", json!({"session_id": session_id}))
        .await?;
    let replay = attach_result
        .get("events")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut done = false;
    for raw in &replay {
        if let Ok(envelope) =
            serde_json::from_value::<trusty_code::events::SessionEventEnvelope>(raw.clone())
        {
            if !json_output {
                println!("{}", render_event_line(&envelope));
            }
            done = done || matches!(envelope.event, Event::SessionDone { .. });
        }
    }

    while !done {
        match client.next_notification(POLL_INTERVAL).await? {
            Some(envelope) => {
                if !json_output {
                    println!("{}", render_event_line(&envelope));
                }
                done = matches!(envelope.event, Event::SessionDone { .. });
            }
            None => continue,
        }
    }

    let status_result = client
        .call("session.status", json!({"session_id": session_id}))
        .await?;
    client.shutdown(Duration::from_secs(5)).await?;

    let session: Session = serde_json::from_value(status_result.clone())
        .map_err(|e| anyhow::anyhow!("tcode run-task: malformed daemon response: {e}"))?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&status_result)
                .map_err(|e| anyhow::anyhow!("tcode run-task: failed to render JSON: {e}"))?
        );
    } else {
        println!(
            "run {}: session={} status={} mode={}",
            if session.status == trusty_code::session::SessionStatus::Finished {
                "finished"
            } else {
                "did not finish cleanly"
            },
            session.id,
            session.status.as_str(),
            session.mode.map(|m| m.as_str()).unwrap_or("unknown")
        );
    }

    Ok(exit_code_for_status(session.status).code())
}
