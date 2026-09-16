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
/// [--mode daily-driver|parity] [--timeout-seconds N] [--no-delegate]`.
///
/// Why/What: see module docs. Returns the process exit code the caller
/// should use (mirrors the legacy path's `ExitCode` contract via
/// `exit_code_for_status`). `timeout_seconds` (#2207) is passed straight
/// through as `task.run`'s own `deadline_secs` param — this function does NOT
/// resolve env/default tiers itself; `task::protocol::task_run` ->
/// `task::executor` resolve it the same way the legacy path's
/// `crate::provider::resolve_deadline_secs` does. `no_delegate` (#8031) is
/// likewise passed straight through as `task.run`'s own `no_delegate` param;
/// the daemon is what swaps `delegate_to_agent` for the named agent's own
/// tools. `pm_model_flag` (#8030) and `max_turns_flag` (#8128) are the ONE
/// exception to "passed straight through": their env tiers
/// (`TCODE_PM_MODEL`, `TCODE_MAX_TURNS`) are resolved here, in the client
/// process the user's environment belongs to, and sent as explicit
/// `pm_model`/`max_turns` params.
/// Test: `cli::run_task::tests::run_params_carry_no_delegate`,
/// `cli::run_task::tests::run_params_carry_pm_model_and_max_turns`.
// #8031: the 8th argument crosses clippy's arity gate. Every argument is one
// `Command::RunTask` clap field passed straight through, so a bundling struct
// here would only restate the clap variant that already is that bundle.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    project: &Path,
    agent: &str,
    task: &str,
    json_output: bool,
    engineer_model: Option<String>,
    mode: Option<String>,
    timeout_seconds: Option<u64>,
    no_delegate: bool,
    pm_model_flag: Option<String>,
    max_turns_flag: Option<u32>,
) -> Result<i32> {
    // #8030/#8128: the env tiers are resolved HERE, in the client, not in the
    // daemon — the daemon is a long-lived process whose environment is not
    // this invocation's, so `TCODE_PM_MODEL`/`TCODE_MAX_TURNS` must be read
    // where the user set them and sent over the wire as explicit params.
    let pm_model = trusty_code::provider::resolve_pm_model_override(pm_model_flag);
    let max_turns =
        trusty_code::provider::resolve_max_turns(max_turns_flag).map_err(anyhow::Error::msg)?;

    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;

    let run_params = build_run_params(
        agent,
        task,
        engineer_model,
        mode,
        timeout_seconds,
        no_delegate,
        pm_model,
        max_turns,
    );
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

/// Build the `task.run` request body this subcommand sends.
///
/// Why: extracted from [`run`] so the wire shape is assertable without
/// spawning a daemon — [`run`] itself is only reachable through a real
/// subprocess, which is why `no_delegate` could otherwise silently stop
/// reaching the daemon with no test noticing (#8031).
/// What: one JSON object; every optional value is serialised as `null` when
/// absent, which `TaskRunRequestParams`'s `#[serde(default)]` fields accept.
/// Test: `cli::run_task::tests::run_params_carry_no_delegate`.
#[allow(clippy::too_many_arguments)]
fn build_run_params(
    agent: &str,
    task: &str,
    engineer_model: Option<String>,
    mode: Option<String>,
    timeout_seconds: Option<u64>,
    no_delegate: bool,
    pm_model: Option<String>,
    max_turns: Option<u32>,
) -> serde_json::Value {
    json!({
        "task_description": task,
        "agent_name": agent,
        "model_override": engineer_model,
        "mode": mode,
        "deadline_secs": timeout_seconds,
        // #8031: the daemon drops `delegate_to_agent` for this run.
        "no_delegate": no_delegate,
        // #8030/#8128: already through their flag-then-env tiers in `run`.
        "pm_model": pm_model,
        "max_turns": max_turns,
    })
}

#[cfg(test)]
mod tests {
    use super::build_run_params;

    /// `--no-delegate` reaches the `task.run` body, and its absence sends
    /// `false` rather than omitting the key (#8031).
    #[test]
    fn run_params_carry_no_delegate() {
        let on = build_run_params("engineer", "do it", None, None, None, true, None, None);
        assert_eq!(on["no_delegate"], serde_json::json!(true));
        assert_eq!(on["agent_name"], serde_json::json!("engineer"));

        let off = build_run_params("engineer", "do it", None, None, None, false, None, None);
        assert_eq!(off["no_delegate"], serde_json::json!(false));
    }

    /// `--pm-model` and `--max-turns` reach the `task.run` body, and their
    /// absence sends an explicit `null` the daemon's `#[serde(default)]`
    /// accepts (#8030, #8128).
    ///
    /// Why: `run` is only reachable through a real subprocess, so without
    /// this the two params could silently stop reaching the daemon — the
    /// exact gap `run_params_carry_no_delegate` was written to close.
    /// What: asserts both keys present-and-typed when set, and `null` when
    /// absent.
    /// Test: this test.
    #[test]
    fn run_params_carry_pm_model_and_max_turns() {
        let set = build_run_params(
            "pm",
            "do it",
            None,
            None,
            None,
            false,
            Some("opus".to_string()),
            Some(24),
        );
        assert_eq!(set["pm_model"], serde_json::json!("opus"));
        assert_eq!(set["max_turns"], serde_json::json!(24));

        let unset = build_run_params("pm", "do it", None, None, None, false, None, None);
        assert_eq!(unset["pm_model"], serde_json::Value::Null);
        assert_eq!(unset["max_turns"], serde_json::Value::Null);
    }
}
