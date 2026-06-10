//! Unit tests for the `cli` module (argument-parsing round-trips).
//!
//! Why: Keeping tests in a sibling file rather than inline in `cli.rs` lets
//! the definition file stay under the 500-line cap while retaining full
//! coverage of every CLI variant.
//!
//! What: Exercises every `Commands` variant and every global flag via
//! `Cli::try_parse_from`, verifying that the clap derive configuration
//! matches the DOC-5 §1.1 surface.
//!
//! Test: `cargo test -p trusty-controller` runs all tests in this file.

use clap::Parser;

use crate::cli::{Cli, Commands, ScopeArg, StackCmd};

/// Parse `tctl version` — must succeed and select `Commands::Version`.
#[test]
fn parse_version() {
    let cli = Cli::try_parse_from(["tctl", "version"]).expect("version parses");
    assert!(matches!(cli.command, Commands::Version));
}

/// Parse `tctl version --json` — global flag propagates into `Cli.json`.
#[test]
fn parse_version_json() {
    let cli = Cli::try_parse_from(["tctl", "version", "--json"]).expect("version --json parses");
    assert!(cli.json);
    assert!(matches!(cli.command, Commands::Version));
}

/// Parse `tctl stack health`.
#[test]
fn parse_stack_health() {
    let cli = Cli::try_parse_from(["tctl", "stack", "health"]).expect("stack health parses");
    assert!(matches!(cli.command, Commands::Stack(StackCmd::Health)));
}

/// Parse `tctl stack doctor`.
#[test]
fn parse_stack_doctor() {
    let cli = Cli::try_parse_from(["tctl", "stack", "doctor"]).expect("stack doctor parses");
    assert!(matches!(
        cli.command,
        Commands::Stack(StackCmd::Doctor { member: None })
    ));
}

/// Parse `tctl stack doctor trusty-search`.
#[test]
fn parse_stack_doctor_member() {
    let cli = Cli::try_parse_from(["tctl", "stack", "doctor", "trusty-search"])
        .expect("stack doctor <m> parses");
    assert!(matches!(
        cli.command,
        Commands::Stack(StackCmd::Doctor { member: Some(_) })
    ));
}

/// Parse `tctl status`.
#[test]
fn parse_status() {
    let cli = Cli::try_parse_from(["tctl", "status"]).expect("status parses");
    assert!(matches!(cli.command, Commands::Status));
}

/// Parse `tctl updates`.
#[test]
fn parse_updates() {
    let cli = Cli::try_parse_from(["tctl", "updates"]).expect("updates parses");
    assert!(matches!(cli.command, Commands::Updates { latest: false }));
}

/// Parse `tctl updates --latest`.
#[test]
fn parse_updates_latest() {
    let cli =
        Cli::try_parse_from(["tctl", "updates", "--latest"]).expect("updates --latest parses");
    assert!(matches!(cli.command, Commands::Updates { latest: true }));
}

/// Parse `tctl upgrade`.
#[test]
fn parse_upgrade() {
    let cli = Cli::try_parse_from(["tctl", "upgrade"]).expect("upgrade parses");
    assert!(matches!(cli.command, Commands::Upgrade { .. }));
}

/// `tctl update` is a visible alias of `upgrade`.
#[test]
fn parse_update_alias() {
    let cli = Cli::try_parse_from(["tctl", "update"]).expect("update alias parses");
    assert!(matches!(cli.command, Commands::Upgrade { .. }));
}

/// Parse `tctl upgrade --check`.
#[test]
fn parse_upgrade_check() {
    let cli = Cli::try_parse_from(["tctl", "upgrade", "--check"]).expect("upgrade --check parses");
    assert!(matches!(cli.command, Commands::Upgrade { check: true, .. }));
}

/// Parse `tctl install`.
#[test]
fn parse_install() {
    let cli = Cli::try_parse_from(["tctl", "install"]).expect("install parses");
    assert!(matches!(cli.command, Commands::Install { .. }));
}

/// Parse `tctl install trusty-search trusty-memory`.
#[test]
fn parse_install_members() {
    let cli = Cli::try_parse_from(["tctl", "install", "trusty-search", "trusty-memory"])
        .expect("install members parses");
    if let Commands::Install { members } = &cli.command {
        assert_eq!(members, &["trusty-search", "trusty-memory"]);
    } else {
        panic!("expected Install");
    }
}

/// Parse `tctl ensure`.
#[test]
fn parse_ensure() {
    let cli = Cli::try_parse_from(["tctl", "ensure"]).expect("ensure parses");
    assert!(matches!(cli.command, Commands::Ensure { wait: false }));
}

/// Parse `tctl ensure --wait`.
#[test]
fn parse_ensure_wait() {
    let cli = Cli::try_parse_from(["tctl", "ensure", "--wait"]).expect("ensure --wait parses");
    assert!(matches!(cli.command, Commands::Ensure { wait: true }));
}

/// Parse `tctl start`.
#[test]
fn parse_start() {
    let cli = Cli::try_parse_from(["tctl", "start"]).expect("start parses");
    assert!(matches!(cli.command, Commands::Start { .. }));
}

/// Parse `tctl stop`.
#[test]
fn parse_stop() {
    let cli = Cli::try_parse_from(["tctl", "stop"]).expect("stop parses");
    assert!(matches!(cli.command, Commands::Stop { .. }));
}

/// Parse `tctl restart`.
#[test]
fn parse_restart() {
    let cli = Cli::try_parse_from(["tctl", "restart"]).expect("restart parses");
    assert!(matches!(cli.command, Commands::Restart { .. }));
}

/// Parse `tctl config`.
#[test]
fn parse_config() {
    let cli = Cli::try_parse_from(["tctl", "config"]).expect("config parses");
    assert!(matches!(cli.command, Commands::Config { .. }));
}

/// Parse `tctl port`.
#[test]
fn parse_port() {
    let cli = Cli::try_parse_from(["tctl", "port"]).expect("port parses");
    assert!(matches!(cli.command, Commands::Port { .. }));
}

/// Parse `tctl port --addr`.
#[test]
fn parse_port_addr() {
    let cli = Cli::try_parse_from(["tctl", "port", "--addr"]).expect("port --addr parses");
    assert!(matches!(cli.command, Commands::Port { addr: true, .. }));
}

/// Parse `tctl doctor --self-check trusty-search`.
#[test]
fn parse_doctor_self_check() {
    let cli = Cli::try_parse_from(["tctl", "doctor", "--self-check", "trusty-search"])
        .expect("doctor --self-check parses");
    assert!(matches!(
        cli.command,
        Commands::Doctor {
            self_check: true,
            member: Some(_)
        }
    ));
}

/// Parse `tctl ui`.
#[test]
fn parse_ui() {
    let cli = Cli::try_parse_from(["tctl", "ui"]).expect("ui parses");
    assert!(matches!(cli.command, Commands::Ui { .. }));
}

/// `tctl port --json-port` succeeds (standalone).
#[test]
fn parse_port_json_port() {
    let cli =
        Cli::try_parse_from(["tctl", "port", "--json-port"]).expect("port --json-port parses");
    assert!(matches!(
        cli.command,
        Commands::Port {
            json_port: true,
            ..
        }
    ));
}

/// `--json-port` and global `--json` may coexist at the parse level.
///
/// Precedence is defined at the runtime level (in `commands::port::run`):
/// `--json-port` takes priority over `--json` when both are present.
/// This test documents that the parse *succeeds* in both orderings and that
/// the fields are set correctly — the precedence enforcement happens in the handler.
#[test]
fn port_json_port_precedence_over_global_json() {
    // `--json` after the subcommand + `--json-port`: both flags are set;
    // parse succeeds and the handler uses --json-port's shape.
    let cli = Cli::try_parse_from(["tctl", "port", "--json", "--json-port"])
        .expect("port --json --json-port parses");
    // Both flags are visible; handler is responsible for precedence.
    assert!(cli.json, "global --json is set");
    assert!(
        matches!(
            cli.command,
            Commands::Port {
                json_port: true,
                ..
            }
        ),
        "--json-port flag is set"
    );

    // `--json` before the subcommand (global position) + `--json-port`:
    // clap does not enforce conflicts for global args across subcommand
    // boundaries, so this also parses successfully.
    let cli = Cli::try_parse_from(["tctl", "--json", "port", "--json-port"])
        .expect("--json port --json-port parses");
    assert!(cli.json, "global --json is set");
    assert!(
        matches!(
            cli.command,
            Commands::Port {
                json_port: true,
                ..
            }
        ),
        "--json-port flag is set"
    );
}

/// Generic passthrough: `tctl trusty-search doctor`.
#[test]
fn parse_passthrough() {
    let cli = Cli::try_parse_from(["tctl", "trusty-search", "doctor"]).expect("passthrough parses");
    if let Commands::Passthrough(args) = &cli.command {
        assert_eq!(args.as_slice(), &["trusty-search", "doctor"]);
    } else {
        panic!("expected Passthrough");
    }
}

/// Bare `tctl` with no subcommand must fail (arg_required_else_help).
#[test]
fn bare_tctl_fails() {
    assert!(Cli::try_parse_from(["tctl"]).is_err());
}

/// Global `--scope` propagates to nested subcommands.
#[test]
fn global_scope_propagates() {
    let cli = Cli::try_parse_from(["tctl", "--scope", "project", "stack", "health"])
        .expect("scope propagates");
    assert_eq!(cli.scope, Some(ScopeArg::Project));
}

/// Global `--timeout` propagates to nested subcommands.
#[test]
fn global_timeout_propagates() {
    let cli =
        Cli::try_parse_from(["tctl", "--timeout", "30", "version"]).expect("timeout propagates");
    assert_eq!(cli.timeout, Some(30));
}

/// `--yes` / `-y` are equivalent.
#[test]
fn yes_short_flag() {
    let long = Cli::try_parse_from(["tctl", "--yes", "restart"]).expect("--yes parses");
    let short = Cli::try_parse_from(["tctl", "-y", "restart"]).expect("-y parses");
    assert!(long.yes);
    assert!(short.yes);
}
