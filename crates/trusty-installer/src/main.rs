//! `trusty-installer` — thin control plane for the claude-mpm stack.
//!
//! Why: Single entry point that parses CLI args and dispatches to per-command
//! handlers in `trusty_installer`. Keeping `main.rs` as a pure shim ensures
//! all testable logic lives in the library crate.
//!
//! What: Initialises tracing (to stderr per the repo rule), parses the
//! `Cli` struct via clap, then dispatches on `Cli.command` to the appropriate
//! handler in `trusty_installer::commands`. Returns the appropriate process
//! exit code. This binary is installed as both `trusty-installer` (primary)
//! and `tctl` (transitional alias — ADR-0013 / SPEC-INSTALLER-01 Phase 1).
//!
//! Test: `cargo run -p trusty-installer -- --help` prints the Phase-0 surface.
//! `cargo test -p trusty-installer` exercises all command handlers and the
//! clap arg-parsing round-trips in `cli::tests`.

use clap::Parser;
use trusty_installer::{
    cli::{Cli, Commands, ConfigSubcommand, StackCmd},
    commands::{
        config, doctor, ensure, install, lifecycle, passthrough, port, run_up, runtime,
        self_update, sign, stack, status, ui, updates, upgrade, version,
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

    // TODO(#920 / Phase 1): forward `cli.scope` to every handler so commands
    // can honour the DOC-3 §3 scope contract.  In Phase 0 the field is parsed
    // and validated by clap (keeping the CLI surface stable) but the resolved
    // scope is not yet threaded into the stub handlers — that wiring lands when
    // the manifest-driven probe loop is implemented.
    //
    // Hint for Phase 1: use `scope::resolve_scope(cli.scope, &std::env::current_dir()?)` here
    // and pass the result as a parameter to each handler's `run(…)` signature.
    let _ = cli.scope; // intentionally unused in Phase 0; see TODO above

    match cli.command {
        Commands::Up {
            with_mpm,
            no_mpm,
            analyze_core,
            wait,
            skip_claude_upgrade,
        } => {
            // `trusty-installer up` is the one command with a meaningful process exit
            // code (DOC-12 §5: 0 healthy / 2 degraded / 1 core hard-failure), so it
            // exits directly with that code rather than falling through to 0.
            let code = run_up(
                with_mpm,
                no_mpm,
                analyze_core,
                wait,
                skip_claude_upgrade,
                yes,
                json,
            );
            std::process::exit(code);
        }

        Commands::Version => {
            version::run(json);
        }

        Commands::Stack(StackCmd::Health) => {
            std::process::exit(stack::run_health(json));
        }

        Commands::Stack(StackCmd::Doctor { member }) => {
            std::process::exit(stack::run_doctor(member.as_deref(), json));
        }

        Commands::Status => {
            std::process::exit(status::run(json));
        }

        Commands::Updates { latest } => {
            std::process::exit(updates::run(latest, json));
        }

        Commands::Upgrade {
            members,
            check,
            latest,
            exclude_self,
        } => {
            std::process::exit(upgrade::run(
                check,
                latest,
                exclude_self,
                yes,
                &members,
                json,
            ));
        }

        Commands::Install {
            members,
            no_service,
        } => {
            std::process::exit(install::run(&members, yes, json, no_service));
        }

        Commands::Ensure { wait } => {
            std::process::exit(ensure::run(wait, json));
        }

        Commands::Start { members } => {
            std::process::exit(lifecycle::run_start(&members, yes, json));
        }

        Commands::Stop { members } => {
            std::process::exit(lifecycle::run_stop(&members, yes, json));
        }

        Commands::Restart { members } => {
            std::process::exit(lifecycle::run_restart(&members, yes, json));
        }

        Commands::Config(args) => match args.action {
            Some(ConfigSubcommand::Keys(cmd)) => {
                if let Err(e) = runtime::block_on(cmd.run()) {
                    eprintln!("{e:#}");
                    std::process::exit(1);
                }
            }
            None => {
                std::process::exit(config::run(&args.members, json));
            }
        },

        Commands::Port {
            member,
            addr,
            json_port,
        } => {
            std::process::exit(port::run(member.as_deref(), addr, json_port, json));
        }

        Commands::Doctor { self_check, member } => {
            std::process::exit(doctor::run(self_check, member.as_deref(), json));
        }

        Commands::Ui { print } => {
            std::process::exit(ui::run(print, json));
        }

        Commands::SelfUpdate => {
            std::process::exit(self_update::run(json));
        }

        Commands::Sign { target, dir } => {
            std::process::exit(sign::run(target.as_set_name(), dir, json));
        }

        Commands::Passthrough(args) => {
            passthrough::run(&args, json);
        }
    }
}
