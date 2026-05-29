//! ompm — thin CLI client for the open-mpm HTTP API (#151 phase-3).
//!
//! Why: Separates the user-facing ergonomic surface from the orchestrator
//! binary. Users run a long-lived server (`open-mpm --serve`) and interact
//! with it via this tiny binary — same UX as `docker` <-> `dockerd`. The
//! thin CLI is trivial to rewrite in other languages or ship as a standalone
//! download, and a future GUI can hit the same HTTP surface.
//! What: Subcommands for task submission (`task`), status polling
//! (`status`), listing (`tasks`), and health check (`health`). Defaults to
//! narrative output; `--json` prints the full `PmResponse` envelope.
//! Test: Exercised by running against a live `--serve` instance; unit tests
//! cover argv parsing for the slash/@ prefix routing logic.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const DEFAULT_SERVER: &str = "http://localhost:8080";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Clap CLI for the thin `ompm` HTTP client.
///
/// Why: Replaces the hand-rolled positional/flag scanner with a derive-based
/// parser so help text, error messages, and value validation are generated
/// automatically. Slash and `@agent` prefix routing is preserved by
/// special-casing argv[0] before clap runs (clap doesn't natively handle
/// "first arg starts with `/` or `@`").
/// What: Five normal subcommands (`task`, `status`, `tasks`, `health`,
/// `help`). Each subcommand owns its own flag/positional schema.
/// Test: Existing `route_prefix` / `parse_task_args` unit tests are
/// retained — slash routing still goes through `parse_task_args`.
#[derive(Debug, Parser)]
#[command(
    name = "ompm",
    about = "Thin client for the open-mpm HTTP API",
    disable_help_subcommand = false
)]
struct OmpmCli {
    #[command(subcommand)]
    cmd: OmpmCommand,
}

#[derive(Debug, Subcommand)]
enum OmpmCommand {
    /// Submit a task and poll for completion.
    Task {
        /// Task description (positional; supports multi-word).
        #[arg(trailing_var_arg = true)]
        rest: Vec<String>,
    },
    /// Print a single task's current status.
    Status {
        /// Task id returned from `task` submission.
        id: String,
    },
    /// List all tasks tracked by the server.
    Tasks,
    /// Healthcheck the server.
    Health,
    /// Execute a REPL slash command via the `open-mpm` binary (#344).
    ///
    /// Why: Lets `ompm` reach the same control surface (`/service`, `/tm`,
    /// `/help`, etc.) that the `open-mpm` REPL exposes, without spinning up
    /// the interactive TUI. The thin client just spawns the orchestrator
    /// binary with the slash args and forwards stdout/stderr/exit code.
    /// What: Captures the slash command + args (e.g. `ompm slash /service
    /// status`) and execs `open-mpm /service status`.
    Slash {
        /// The slash command and its arguments (e.g. `/service status`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
}

/// Minimal subset of the server-side `PmResponse` we actually need to read.
///
/// Why: avoids coupling the thin client to the full `open-mpm` crate. We
/// only care about `id`, `status`, and the fields we print.
/// What: Deserializes from the server's JSON; unknown fields are tolerated.
#[derive(Debug, Clone, Deserialize)]
struct PmResponseView {
    id: String,
    status: String,
    #[serde(default)]
    narrative: String,
    #[serde(default)]
    errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TaskBody<'a> {
    task: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    out_dir: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a str>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ompm: {e:#}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        anyhow::bail!("missing subcommand");
    }

    let server = std::env::var("OMPM_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string());

    // #151 phase-4: slash and @ prefix routing — the first arg dispatches
    // to well-known workflows or agents. Detected before clap so the
    // unusual argv shape (`/qa ./path`) doesn't confuse the parser.
    if let Some(routed) = route_prefix(&args) {
        return submit_and_print(&server, &args[1..], routed).await;
    }

    // Built-in help passthrough so `ompm help` continues to print the
    // legacy usage banner (it documents slash commands and `@agent` which
    // aren't part of the clap schema).
    if matches!(args[0].as_str(), "help" | "--help" | "-h") {
        print_usage();
        return Ok(());
    }

    // Re-prepend the binary name so clap can parse with its default config
    // (clap expects argv[0] to be the program name).
    let mut clap_argv: Vec<String> = vec!["ompm".to_string()];
    clap_argv.extend(args.iter().cloned());
    let parsed = OmpmCli::try_parse_from(clap_argv).map_err(|e| anyhow::anyhow!("{e}"))?;

    match parsed.cmd {
        OmpmCommand::Task { rest } => {
            submit_and_print(&server, &rest, RoutedRequest::default()).await
        }
        OmpmCommand::Status { id } => cmd_status(&server, &[id]).await,
        OmpmCommand::Tasks => cmd_tasks(&server).await,
        OmpmCommand::Health => cmd_health(&server).await,
        OmpmCommand::Slash { rest } => cmd_slash(&rest).await,
    }
}

/// Exec `open-mpm <slash-args>` and forward exit code (#344).
///
/// Why: `ompm slash /service start` should behave identically to running
/// `open-mpm /service start` directly — the thin client is just a more
/// ergonomic dispatcher. We spawn the orchestrator binary so the REPL's
/// slash dispatcher (the source of truth) runs the command.
/// What: Looks up `open-mpm` on PATH, passes through stdin/stdout/stderr,
/// waits for the child, exits with its status code.
/// Test: Manual: `ompm slash /help` should print the same help as
/// `open-mpm /help`.
async fn cmd_slash(rest: &[String]) -> anyhow::Result<()> {
    if rest.is_empty() || !rest[0].starts_with('/') {
        anyhow::bail!("slash requires a slash command (e.g. `ompm slash /help`)");
    }
    let status = std::process::Command::new("open-mpm")
        .args(rest)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn `open-mpm`: {e}"))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn print_usage() {
    eprintln!(
        "ompm — thin client for the open-mpm HTTP API\n\
\n\
Usage:\n\
  ompm task \"<description>\" [--workflow <name>] [--out-dir <dir>] [--json]\n\
  ompm status <task-id>\n\
  ompm tasks\n\
  ompm health\n\
\n\
Slash commands:\n\
  ompm /research \"<query>\"   — single-agent research-agent\n\
  ompm /implement \"<task>\"   — prescriptive workflow\n\
  ompm /qa <path>             — QA-only workflow\n\
  ompm /plan \"<task>\"        — plan-only workflow\n\
  ompm slash /<cmd> [args]    — passthrough to `open-mpm /<cmd>` (#344)\n\
\n\
Agent prefix:\n\
  ompm @<agent-name> \"<task>\" — dispatch single agent\n\
\n\
Env:\n\
  OMPM_SERVER  base URL (default {DEFAULT_SERVER})\n"
    );
}

// ---------- subcommands ----------

async fn cmd_health(server: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{server}/api/health"))
        .send()
        .await?
        .error_for_status()?;
    let body: Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

async fn cmd_tasks(server: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{server}/api/tasks"))
        .send()
        .await?
        .error_for_status()?;
    let body: Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

async fn cmd_status(server: &str, rest: &[String]) -> anyhow::Result<()> {
    let id = rest
        .first()
        .ok_or_else(|| anyhow::anyhow!("status requires a <task-id> argument"))?;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{server}/api/task/{id}"))
        .send()
        .await?
        .error_for_status()?;
    let body: Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

/// Shared submit flow: POST /api/task, poll /api/task/:id every 2s, print
/// result on terminal status.
///
/// Why: Every task-submission path (`ompm task`, slash commands, @agent)
/// funnels here so the polling + output logic is defined once.
async fn submit_and_print(
    server: &str,
    rest: &[String],
    routed: RoutedRequest,
) -> anyhow::Result<()> {
    let (task_text, flags) = parse_task_args(rest, &routed)?;
    let client = reqwest::Client::new();

    let body = TaskBody {
        task: &task_text,
        workflow: flags.workflow.as_deref(),
        out_dir: flags.out_dir.as_deref(),
        agent: flags.agent.as_deref(),
    };
    let resp = client
        .post(format!("{server}/api/task"))
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let submitted: Value = resp.json().await?;
    if !status.is_success() {
        anyhow::bail!("server rejected submission: {submitted}");
    }
    let id = submitted
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("server response missing id: {submitted}"))?
        .to_string();

    // Poll until terminal.
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let r = client.get(format!("{server}/api/task/{id}")).send().await?;
        if !r.status().is_success() {
            anyhow::bail!("polling failed: status {}", r.status());
        }
        let v: Value = r.json().await?;
        let status_str = v
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("running");
        if status_str == "running" {
            eprint!(".");
            use std::io::Write;
            let _ = std::io::stderr().flush();
            continue;
        }
        eprintln!();
        if flags.json_output {
            println!("{}", serde_json::to_string_pretty(&v)?);
        } else {
            let view: PmResponseView = serde_json::from_value(v)?;
            if !view.errors.is_empty() {
                eprintln!("errors: {}", view.errors.join("; "));
            }
            if !view.narrative.is_empty() {
                println!("{}", view.narrative);
            } else {
                eprintln!("(no narrative; status={})", view.status);
                eprintln!("task id: {}", view.id);
            }
        }
        return Ok(());
    }
}

// ---------- argv / prefix parsing ----------

#[derive(Debug, Clone, Default)]
struct RoutedRequest {
    agent: Option<String>,
    workflow: Option<String>,
    /// Optional prefix prepended to the user's task text. Lets slash commands
    /// like `/qa ./path` produce a task string of "run QA on ./path"
    /// (phase-4 convention) without forcing callers to type the prefix.
    task_prefix: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct TaskFlags {
    workflow: Option<String>,
    out_dir: Option<String>,
    agent: Option<String>,
    json_output: bool,
}

/// Detect a slash or @ prefix in the first argv entry and map it to a
/// `RoutedRequest`. Returns `None` for non-prefixed commands (fall through
/// to the normal `task`/`status`/... dispatcher).
///
/// Why (#151 phase-4): Gives operators a terse vocabulary for common
/// dispatch patterns without teaching them the full flag surface.
/// What: `/research|/implement|/qa|/plan` → workflow or agent routing;
/// `@<agent>` → single-agent dispatch.
/// Test: `route_prefix_handles_known_slash_and_at_prefixes`.
fn route_prefix(args: &[String]) -> Option<RoutedRequest> {
    let first = args.first()?.as_str();
    if let Some(rest) = first.strip_prefix('/') {
        let routed = match rest {
            "research" => RoutedRequest {
                agent: Some("research-agent".into()),
                workflow: None,
                task_prefix: None,
            },
            "implement" => RoutedRequest {
                agent: None,
                workflow: Some("prescriptive".into()),
                task_prefix: None,
            },
            "qa" => RoutedRequest {
                agent: None,
                workflow: Some("qa-only".into()),
                task_prefix: Some("run QA on".into()),
            },
            "plan" => RoutedRequest {
                agent: None,
                workflow: Some("plan-only".into()),
                task_prefix: None,
            },
            _ => return None,
        };
        return Some(routed);
    }
    if let Some(agent) = first.strip_prefix('@')
        && !agent.is_empty()
    {
        return Some(RoutedRequest {
            agent: Some(agent.to_string()),
            workflow: None,
            task_prefix: None,
        });
    }
    None
}

/// Parse the task positional argument and trailing flags.
///
/// Why: `ompm task "foo" --workflow bar --json` needs to collapse the flag
/// tail cleanly whether the caller came from slash-prefix routing or the
/// raw `task` subcommand.
/// What: First non-flag arg is the task text; remaining `--key value` pairs
/// become `TaskFlags`. Merges `RoutedRequest` defaults for agent/workflow
/// but explicit flags win.
/// Test: `parse_task_args_basic`, `parse_task_args_merges_routed`.
fn parse_task_args(rest: &[String], routed: &RoutedRequest) -> anyhow::Result<(String, TaskFlags)> {
    let mut task_text: Option<String> = None;
    let mut flags = TaskFlags {
        workflow: routed.workflow.clone(),
        agent: routed.agent.clone(),
        ..Default::default()
    };
    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        match a.as_str() {
            "--json" => {
                flags.json_output = true;
                i += 1;
            }
            "--workflow" => {
                flags.workflow = Some(
                    rest.get(i + 1)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--workflow requires a value"))?,
                );
                i += 2;
            }
            "--out-dir" => {
                flags.out_dir = Some(
                    rest.get(i + 1)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--out-dir requires a value"))?,
                );
                i += 2;
            }
            "--agent" => {
                flags.agent = Some(
                    rest.get(i + 1)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--agent requires a value"))?,
                );
                i += 2;
            }
            s if s.starts_with("--") => {
                anyhow::bail!("unknown flag: {s}");
            }
            _ => {
                if task_text.is_none() {
                    task_text = Some(a.clone());
                } else {
                    // Concatenate extra positional args (treats remaining as
                    // part of the task string).
                    let existing = task_text.take().unwrap_or_default();
                    task_text = Some(format!("{existing} {a}"));
                }
                i += 1;
            }
        }
    }
    let task = task_text.ok_or_else(|| anyhow::anyhow!("missing task description"))?;
    let task = match &routed.task_prefix {
        Some(prefix) => format!("{prefix} {task}"),
        None => task,
    };
    Ok((task, flags))
}

#[cfg(test)]
#[path = "ompm/tests.rs"]
mod tests;
