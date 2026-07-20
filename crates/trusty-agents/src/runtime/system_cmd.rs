//! `tagent system status [--json] [--agent <name>]` — CLI surface for the
//! `system_status` tool (epic #3052).
//!
//! Why: this project's standing "must be testable by CLI/API" directive
//! means the subsystem status check must be reachable without burning an LLM
//! turn — an operator (or a script) should be able to run `tagent system
//! status` directly. Reuses `crate::system_status::gather` verbatim (the
//! same core the LLM tool calls), so the CLI and the tool can never
//! disagree about what "up" means.
//! What: `run_system_subcommand` parses the `system` argv tail (`status` is
//! the only verb today) and prints either the human-readable
//! `format::render_text` or `serde_json::to_string_pretty`, matching the
//! existing global `--json` convention used elsewhere in this CLI (e.g.
//! `eval run --json`).
//! Test: `crates/trusty-agents/tests/system_status_cli.rs` drives the real
//! built binary.

use anyhow::{Context, Result, bail};

/// Handle `tagent system <verb>`.
///
/// Why: split out of `subcommands.rs` (which only locates + dispatches) to
/// keep that file focused on argv-prefix matching.
/// What: `status` is the only verb; `--json` switches to machine-readable
/// output; `--agent <name>` overrides which agent's identity is reported
/// under `tagent` (defaults to `"ctrl"`, the harness's base coordination
/// identity — the same default `resolve_endpoint_agent` falls back to when
/// no persona is active).
/// Test: `system_status_cli.rs::system_status_json_has_expected_top_level_keys`.
pub(super) async fn run_system_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    match sub {
        "status" => run_status(&args[1.min(args.len())..]).await,
        other => {
            let known = &["status"];
            if let Some(s) = crate::cli::did_you_mean(other, known, 2) {
                eprintln!("tagent system: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!("tagent system: unknown subcommand '{other}'. Try: status");
            }
            bail!("unknown system subcommand: {other}");
        }
    }
}

async fn run_status(args: &[String]) -> Result<()> {
    let mut as_json = false;
    let mut agent_name = "ctrl".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                as_json = true;
                i += 1;
            }
            "--agent" => {
                agent_name = args.get(i + 1).context("--agent requires a value")?.clone();
                i += 2;
            }
            "--help" | "-h" => {
                println!("Usage: tagent system status [--json] [--agent <name>]");
                return Ok(());
            }
            other => bail!("unknown argument to `system status`: {other}"),
        }
    }

    let report = crate::system_status::gather(&agent_name).await;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialize system status report")?
        );
    } else {
        print!("{}", crate::system_status::format::render_text(&report));
    }
    Ok(())
}
