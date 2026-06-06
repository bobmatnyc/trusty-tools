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

    // Long-running daemon mode: init file-rotating tracing + bug-capture layer
    // (identical to the former trusty-mpmd binary). Short-lived CLI invocations
    // skip subscriber init entirely — they have no meaningful log volume and
    // there is no global registry yet to conflict with.
    //
    // The non-blocking writer guard `_daemon_log_guard` must live for the
    // duration of `main` so buffered records flush before the process exits.
    // It is declared unconditionally (as an Option) so the borrow checker is
    // satisfied regardless of which cfg branch runs.
    #[cfg(feature = "daemon")]
    let mut _daemon_log_guard: Option<tracing_appender::non_blocking::WorkerGuard> = None;

    if matches!(cli.command, Command::Daemon { .. }) {
        #[cfg(feature = "daemon")]
        {
            // File logging: write daily-rotated logs to ~/.trusty-mpm/logs/ in
            // addition to the existing stderr stream.
            let log_dir = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?
                .join(".trusty-mpm")
                .join("logs");
            std::fs::create_dir_all(&log_dir)?;
            let file_appender = tracing_appender::rolling::daily(&log_dir, "trusty-mpm.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            _daemon_log_guard = Some(guard);

            let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into());
            let file_filter = tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into());

            // Bug-reporting Phase 1 (#478): compose the bug-capture layer so
            // ERROR events are captured to <data_dir>/trusty-mpm/errors.jsonl
            // and an in-memory ring without modifying any call sites.
            // Capture writes ONLY to JSONL + in-memory ring — never stdout —
            // so this is safe for both the HTTP daemon and the MCP stdio path.
            let (capture_layer, _error_store) = trusty_common::error_capture::bug_capture_layer(
                "trusty-mpm",
                trusty_common::error_capture::DEFAULT_CAPTURE_CAPACITY,
                env!("CARGO_PKG_VERSION"),
            );

            use tracing_subscriber::Layer as _;
            use tracing_subscriber::layer::SubscriberExt as _;
            use tracing_subscriber::util::SubscriberInitExt as _;
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        // MCP mode speaks JSON-RPC on stdout — keep tracing on stderr.
                        .with_writer(std::io::stderr)
                        .with_filter(env_filter),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .with_ansi(false)
                        .with_filter(file_filter),
                )
                .with(capture_layer)
                .init();
        }
        #[cfg(not(feature = "daemon"))]
        {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .with_writer(std::io::stderr)
                .init();
        }
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
