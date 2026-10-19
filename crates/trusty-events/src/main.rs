//! `trusty-events serve`: the event bus daemon (#9075, ADR-0065).
//!
//! Why: ADR-0065 runs the bus as its own launchd-supervised process. This is
//! that process's entry point; the library holds the behaviour.
//! What: parses `serve [--socket P] [--log-dir P] [--retain-days N]
//! [--ring-capacity N]`, defaulting the paths to
//! `control_bus::events_socket_path()` and
//! `<data dir>/trusty-events/event_log/`, logs to stderr (`RUST_LOG`,
//! default `info`), and serves until SIGTERM or SIGINT. Exit 0 after a clean
//! shutdown; exit 1, with the error chain on stderr, when the socket is held
//! by another instance or cannot be bound, the log cannot be opened, or the
//! accept loop gives up; exit 2 on a usage error.
//! Test: `tests/daemon_exit.rs`, `tests/daemon_wire.rs`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Args, Parser, Subcommand};
use trusty_events::daemon::{DEFAULT_LOG_RETAIN_DAYS, DEFAULT_RING_CAPACITY, default_log_dir};
use trusty_events::{DaemonConfig, run};

#[derive(Parser)]
#[command(
    name = "trusty-events",
    version,
    about = "The trusty-* event bus daemon"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the bus socket until SIGTERM or SIGINT.
    Serve(ServeArgs),
}

#[derive(Args)]
struct ServeArgs {
    /// Socket to bind. Default: `<data dir>/trusty-events/trusty-events.sock`.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Day-file directory. Default: `<data dir>/trusty-events/event_log/`.
    #[arg(long)]
    log_dir: Option<PathBuf>,
    /// Calendar days of day files kept.
    #[arg(long, default_value_t = DEFAULT_LOG_RETAIN_DAYS)]
    retain_days: u32,
    /// Events held in memory for live readers.
    #[arg(long, default_value_t = DEFAULT_RING_CAPACITY)]
    ring_capacity: usize,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("trusty-events: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Command::Serve(args) = cli.command;
    let result = runtime.block_on(serve(args));
    // #9075: a log writer stuck in a blocking file call must not hold the exit
    // open; the drain bound has already decided the outcome.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trusty-events: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the configuration and run the daemon.
async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let socket = match args.socket {
        Some(socket) => socket,
        None => trusty_common::control_bus::events_socket_path()
            .context("resolve the trusty-events socket path")?,
    };
    let log_dir = match args.log_dir {
        Some(dir) => dir,
        None => default_log_dir().context("resolve the trusty-events log directory")?,
    };
    let mut config = DaemonConfig::new(socket, log_dir);
    config.retain_days = args.retain_days;
    config.ring_capacity = args.ring_capacity;
    run(config, trusty_common::shutdown_signal())
        .await
        .context("trusty-events stopped")
}
