//! `tm` / `trusty-mpm` — unified MPM CLI entry point.
//!
//! Why: this file is intentionally thin. All logic is in the submodules below;
//! `main` only parses arguments, sets up tracing for long-running modes, and
//! dispatches to the appropriate handler function.
//! What: module declarations, lazy HELP initializer, `main()` with clap
//! dispatch.
//! Test: `cargo test -p trusty-mpm` runs the full suite in `tests.rs`.

mod cli;
mod commands;
mod formatters;
mod types;

use clap::Parser;
use cli::{Cli, Command};
use commands::{
    daemon::{restart, run_daemon, start, stop_daemon},
    install::install,
    launch::{connect, launch},
    misc::{attach_cmd, coordinator, doctor, hook, optimizer, overseer, status},
    project::project,
    services::services,
    session::session,
    telegram::telegram,
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests_behavior_a.rs"]
mod tests_behavior_a;

#[cfg(test)]
#[path = "tests_behavior_b.rs"]
mod tests_behavior_b;

/// Lazy-loaded help configuration for "did you mean?" suggestions (issue #216).
///
/// Why: the YAML help bundle is checked in as a string literal; loading it
/// lazily avoids any parse work on the (common) fast path where every argument
/// is valid.
/// What: parses `help.yaml` once on first access via `std::sync::LazyLock`.
/// Test: the suggestion path is exercised indirectly by the clap parse tests.
static HELP: std::sync::LazyLock<trusty_common::help::HelpConfig> =
    std::sync::LazyLock::new(|| {
        trusty_common::help::load_help(include_str!("../../../help.yaml"))
            .expect("trusty-mpm help.yaml is bundled and valid")
    });

/// Binary entry point.
///
/// Why: separation of concerns — `main` owns the lifecycle (arg parsing,
/// tracing init, exit codes) while the handlers own the domain logic.
/// What: tries to parse via `clap::Parser::try_parse`, prints a "did you
/// mean?" hint on an unknown-subcommand error, then dispatches.
/// Test: integration tests in `tests.rs` exercise every dispatch branch.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Why: parse via `try_parse` so we can attach the workspace-shared
    // "did you mean?" suggestion (issue #216) before exiting on a clap error.
    let argv: Vec<String> = std::env::args().collect();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            e.print().ok();
            if matches!(
                e.kind(),
                clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
            ) {
                trusty_common::help::print_suggestion_hint(&argv, &HELP);
            }
            std::process::exit(e.exit_code());
        }
    };

    // Long-running modes need tracing on stderr (the daemon's MCP mode speaks
    // JSON-RPC on stdout, so all logs must stay off stdout).
    if matches!(cli.command, Command::Daemon { .. }) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .with_writer(std::io::stderr)
            .init();
    }

    let client = reqwest::Client::new();
    // Resolve the daemon URL once: explicit --url/TRUSTY_MPM_URL wins, then
    // lock file (daemon may bind to an ephemeral port), then default.
    let url = trusty_mpm::core::resolve_daemon_url(Some(&cli.url));
    match cli.command {
        Command::Status => status(&client, &url).await,
        Command::Start => start(&client, &url).await,
        Command::Serve => start(&client, &url).await,
        Command::Stop => stop_daemon().await,
        Command::Restart => restart(&client, &url).await,
        Command::Project { action } => project(&client, &url, action).await,
        Command::Session { action } => session(&client, &url, action).await,
        Command::Events => commands::misc::events(&client, &url).await,
        Command::Doctor => doctor(&url).await,
        Command::Tui {
            url: tui_url,
            interval_ms,
        } => {
            let resolved = trusty_mpm::core::resolve_daemon_url(Some(&tui_url));
            trusty_mpm::tui::run(resolved, interval_ms).await
        }
        Command::Gui => {
            #[cfg(feature = "gui")]
            {
                trusty_mpm_gui::run();
                Ok(())
            }
            #[cfg(not(feature = "gui"))]
            {
                anyhow::bail!(
                    "this build was compiled without GUI support (the `gui` feature is disabled)"
                )
            }
        }
        Command::Telegram { cmd } => telegram(&url, cmd).await,
        Command::Install { force } => install(force),
        Command::Hook => hook(&client, &url).await,
        Command::Daemon {
            addr,
            tailscale,
            mcp,
        } => run_daemon(addr, tailscale, mcp).await,
        Command::Launch { dir } => launch(&client, &url, dir).await,
        Command::Connect { dir } => connect(&client, &url, dir).await,
        Command::Attach { target, json } => attach_cmd(&client, &url, &target, json).await,
        Command::Optimizer { action } => optimizer(&client, &url, action).await,
        Command::Overseer { action } => overseer(&client, &url, action).await,
        Command::Coordinator { message } => coordinator(&url, message).await,
        Command::Services { action } => services(action),
    }
}
