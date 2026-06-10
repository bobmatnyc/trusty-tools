//! `tctl` — thin control plane for the claude-mpm stack.
//!
//! Why: Single entry point that parses CLI args and dispatches to per-command
//! handlers in `trusty_controller`. Keeping `main.rs` as a pure shim ensures
//! all testable logic lives in the library crate.
//!
//! What: Initialises tracing (to stderr per the repo rule), parses the
//! `Cli` struct via clap, then dispatches on `Cli.command` to the appropriate
//! handler in `trusty_controller::commands`. Returns the appropriate process
//! exit code.
//!
//! Test: `cargo run -p trusty-controller -- --help` prints the Phase-0 surface.
//! `cargo test -p trusty-controller` exercises all command handlers and the
//! clap arg-parsing round-trips in `cli::tests`.

use clap::Parser;
use trusty_controller::{
    cli::{Cli, Commands, StackCmd},
    commands::{
        config, doctor, ensure, install, lifecycle, passthrough, port, stack, status, ui, updates,
        upgrade, version,
    },
};

fn main() {
    // Initialise tracing to stderr (CLAUDE.md: logs → stderr, stdout = data).
    // Use try_init so it is idempotent when tests initialise a subscriber first.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let cli = Cli::parse();

    dispatch(cli);
}

/// Dispatch parsed CLI args to the appropriate command handler.
///
/// Why: A standalone function (vs inlining into `main`) makes the dispatch
/// table testable in isolation without actually exiting the process.
///
/// What: Matches on `Cli.command` and calls the corresponding `commands::*::run`
/// function, forwarding the global flags (`json`, `yes`, etc.) as needed.
///
/// Test: Construct a `Cli` via `Cli::parse_from(…)` and call `dispatch`; verify
/// it does not panic.
fn dispatch(cli: Cli) {
    let json = cli.json;
    let yes = cli.yes;

    match cli.command {
        Commands::Version => {
            version::run(json);
        }

        Commands::Stack(StackCmd::Health) => {
            stack::run_health(json);
        }

        Commands::Stack(StackCmd::Doctor { member }) => {
            stack::run_doctor(member.as_deref(), json);
        }

        Commands::Status => {
            status::run(json);
        }

        Commands::Updates { latest } => {
            updates::run(latest, json);
        }

        Commands::Upgrade {
            members,
            check,
            latest,
            exclude_self,
        } => {
            upgrade::run(check, latest, exclude_self, yes, &members, json);
        }

        Commands::Install { members } => {
            install::run(&members, yes, json);
        }

        Commands::Ensure { wait } => {
            ensure::run(wait, json);
        }

        Commands::Start { members } => {
            lifecycle::run_start(&members, yes, json);
        }

        Commands::Stop { members } => {
            lifecycle::run_stop(&members, yes, json);
        }

        Commands::Restart { members } => {
            lifecycle::run_restart(&members, yes, json);
        }

        Commands::Config { members } => {
            config::run(&members, json);
        }

        Commands::Port { addr, json_port } => {
            port::run(addr, json_port, json);
        }

        Commands::Doctor { self_check, member } => {
            doctor::run(self_check, member.as_deref(), json);
        }

        Commands::Ui { print } => {
            ui::run(print, json);
        }

        Commands::Passthrough(args) => {
            passthrough::run(&args, json);
        }
    }
}
