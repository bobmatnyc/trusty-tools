//! Unit tests for the `cli` module (argument-parsing round-trips).
//!
//! Why: Keeping tests in a sibling file rather than inline in `cli.rs` lets
//! the definition file stay under the 500-line cap while retaining full
//! coverage of every CLI variant.
//!
//! What: Exercises every `Commands` variant and every global flag via
//! `Cli::try_parse_from`, verifying that the clap derive configuration
//! matches the DOC-5 §1.1 surface. Tests use the primary binary name
//! (`trusty-installer`) and also verify the `tctl` alias is accepted.
//!
//! Test: `cargo test -p trusty-installer` runs all tests in this file.

use clap::Parser;

use crate::cli::{AnalyzeCoreArg, Cli, Commands, ScopeArg, StackCmd};

/// Parse `trusty-installer up` — selects `Commands::Up` with all flags defaulted off.
#[test]
fn parse_up() {
    let cli = Cli::try_parse_from(["trusty-installer", "up"]).expect("up parses");
    assert!(matches!(
        cli.command,
        Commands::Up {
            with_mpm: false,
            no_mpm: false,
            analyze_core: None,
            wait: false,
            skip_claude_upgrade: false,
        }
    ));
}

/// The `tctl` alias name is accepted at the parse level (same binary, same args).
///
/// Why: ADR-0013 / SPEC-INSTALLER-01 keep `tctl` as a transitional alias.
/// The binary is published under BOTH names; clap's `name = "trusty-installer"`
/// means the help text shows the primary name, but any argv[0] is accepted by
/// `try_parse_from` because clap uses the provided slice — not the actual
/// process name — for parsing. This test documents that alias invocations parse
/// identically.
/// What: Parses `["tctl", "version"]`; asserts `Commands::Version`.
/// Test: This is the test.
#[test]
fn tctl_alias_parses_identically() {
    let cli = Cli::try_parse_from(["tctl", "version"]).expect("tctl alias parses");
    assert!(matches!(cli.command, Commands::Version));
    let cli2 = Cli::try_parse_from(["trusty-installer", "version"]).expect("primary name parses");
    assert!(matches!(cli2.command, Commands::Version));
}

/// Parse `trusty-installer up --with-mpm`.
#[test]
fn parse_up_with_mpm() {
    let cli = Cli::try_parse_from(["trusty-installer", "up", "--with-mpm"])
        .expect("up --with-mpm parses");
    assert!(matches!(cli.command, Commands::Up { with_mpm: true, .. }));
}

/// `--with-mpm` and `--no-mpm` are mutually exclusive (clap conflicts_with).
#[test]
fn parse_up_with_and_no_mpm_conflict() {
    assert!(Cli::try_parse_from(["trusty-installer", "up", "--with-mpm", "--no-mpm"]).is_err());
}

/// Parse `trusty-installer up --analyze-core blocking|background|skip`.
#[test]
fn parse_up_analyze_core() {
    let cli = Cli::try_parse_from(["trusty-installer", "up", "--analyze-core", "blocking"])
        .expect("up --analyze-core blocking parses");
    assert!(matches!(
        cli.command,
        Commands::Up {
            analyze_core: Some(AnalyzeCoreArg::Blocking),
            ..
        }
    ));
    let cli = Cli::try_parse_from(["trusty-installer", "up", "--analyze-core", "skip"])
        .expect("skip parses");
    assert!(matches!(
        cli.command,
        Commands::Up {
            analyze_core: Some(AnalyzeCoreArg::Skip),
            ..
        }
    ));
}

/// Parse `trusty-installer up --wait --skip-claude-upgrade --no-mpm`.
#[test]
fn parse_up_combined_flags() {
    let cli = Cli::try_parse_from([
        "trusty-installer",
        "up",
        "--wait",
        "--skip-claude-upgrade",
        "--no-mpm",
    ])
    .expect("combined up flags parse");
    assert!(matches!(
        cli.command,
        Commands::Up {
            wait: true,
            skip_claude_upgrade: true,
            no_mpm: true,
            ..
        }
    ));
}

/// Parse `trusty-installer version` — must succeed and select `Commands::Version`.
#[test]
fn parse_version() {
    let cli = Cli::try_parse_from(["trusty-installer", "version"]).expect("version parses");
    assert!(matches!(cli.command, Commands::Version));
}

/// Parse `trusty-installer version --json` — global flag propagates into `Cli.json`.
#[test]
fn parse_version_json() {
    let cli = Cli::try_parse_from(["trusty-installer", "version", "--json"])
        .expect("version --json parses");
    assert!(cli.json);
    assert!(matches!(cli.command, Commands::Version));
}

/// Parse `trusty-installer stack health`.
#[test]
fn parse_stack_health() {
    let cli =
        Cli::try_parse_from(["trusty-installer", "stack", "health"]).expect("stack health parses");
    assert!(matches!(cli.command, Commands::Stack(StackCmd::Health)));
}

/// Parse `trusty-installer stack doctor`.
#[test]
fn parse_stack_doctor() {
    let cli =
        Cli::try_parse_from(["trusty-installer", "stack", "doctor"]).expect("stack doctor parses");
    assert!(matches!(
        cli.command,
        Commands::Stack(StackCmd::Doctor { member: None })
    ));
}

/// Parse `trusty-installer stack doctor trusty-search`.
#[test]
fn parse_stack_doctor_member() {
    let cli = Cli::try_parse_from(["trusty-installer", "stack", "doctor", "trusty-search"])
        .expect("stack doctor <m> parses");
    assert!(matches!(
        cli.command,
        Commands::Stack(StackCmd::Doctor { member: Some(_) })
    ));
}

/// Parse `trusty-installer status`.
#[test]
fn parse_status() {
    let cli = Cli::try_parse_from(["trusty-installer", "status"]).expect("status parses");
    assert!(matches!(cli.command, Commands::Status));
}

/// Parse `trusty-installer updates`.
#[test]
fn parse_updates() {
    let cli = Cli::try_parse_from(["trusty-installer", "updates"]).expect("updates parses");
    assert!(matches!(cli.command, Commands::Updates { latest: false }));
}

/// Parse `trusty-installer updates --latest`.
#[test]
fn parse_updates_latest() {
    let cli = Cli::try_parse_from(["trusty-installer", "updates", "--latest"])
        .expect("updates --latest parses");
    assert!(matches!(cli.command, Commands::Updates { latest: true }));
}

/// Parse `trusty-installer upgrade`.
#[test]
fn parse_upgrade() {
    let cli = Cli::try_parse_from(["trusty-installer", "upgrade"]).expect("upgrade parses");
    assert!(matches!(cli.command, Commands::Upgrade { .. }));
}

/// `trusty-installer update` is a visible alias of `upgrade`.
#[test]
fn parse_update_alias() {
    let cli = Cli::try_parse_from(["trusty-installer", "update"]).expect("update alias parses");
    assert!(matches!(cli.command, Commands::Upgrade { .. }));
}

/// Parse `trusty-installer upgrade --check`.
#[test]
fn parse_upgrade_check() {
    let cli = Cli::try_parse_from(["trusty-installer", "upgrade", "--check"])
        .expect("upgrade --check parses");
    assert!(matches!(cli.command, Commands::Upgrade { check: true, .. }));
}

/// Parse `trusty-installer install`.
#[test]
fn parse_install() {
    let cli = Cli::try_parse_from(["trusty-installer", "install"]).expect("install parses");
    assert!(matches!(cli.command, Commands::Install { .. }));
}

/// Parse `trusty-installer install trusty-search trusty-memory`.
#[test]
fn parse_install_members() {
    let cli = Cli::try_parse_from([
        "trusty-installer",
        "install",
        "trusty-search",
        "trusty-memory",
    ])
    .expect("install members parses");
    if let Commands::Install { members } = &cli.command {
        assert_eq!(members, &["trusty-search", "trusty-memory"]);
    } else {
        panic!("expected Install");
    }
}

/// Parse `trusty-installer ensure`.
#[test]
fn parse_ensure() {
    let cli = Cli::try_parse_from(["trusty-installer", "ensure"]).expect("ensure parses");
    assert!(matches!(cli.command, Commands::Ensure { wait: false }));
}

/// Parse `trusty-installer ensure --wait`.
#[test]
fn parse_ensure_wait() {
    let cli = Cli::try_parse_from(["trusty-installer", "ensure", "--wait"])
        .expect("ensure --wait parses");
    assert!(matches!(cli.command, Commands::Ensure { wait: true }));
}

/// Parse `trusty-installer start`.
#[test]
fn parse_start() {
    let cli = Cli::try_parse_from(["trusty-installer", "start"]).expect("start parses");
    assert!(matches!(cli.command, Commands::Start { .. }));
}

/// Parse `trusty-installer stop`.
#[test]
fn parse_stop() {
    let cli = Cli::try_parse_from(["trusty-installer", "stop"]).expect("stop parses");
    assert!(matches!(cli.command, Commands::Stop { .. }));
}

/// Parse `trusty-installer restart`.
#[test]
fn parse_restart() {
    let cli = Cli::try_parse_from(["trusty-installer", "restart"]).expect("restart parses");
    assert!(matches!(cli.command, Commands::Restart { .. }));
}

/// Parse `trusty-installer config`.
#[test]
fn parse_config() {
    let cli = Cli::try_parse_from(["trusty-installer", "config"]).expect("config parses");
    assert!(matches!(cli.command, Commands::Config { .. }));
}

/// Parse `trusty-installer port`.
#[test]
fn parse_port() {
    let cli = Cli::try_parse_from(["trusty-installer", "port"]).expect("port parses");
    assert!(matches!(cli.command, Commands::Port { .. }));
}

/// Parse `trusty-installer port --addr`.
#[test]
fn parse_port_addr() {
    let cli =
        Cli::try_parse_from(["trusty-installer", "port", "--addr"]).expect("port --addr parses");
    assert!(matches!(cli.command, Commands::Port { addr: true, .. }));
}

/// Parse `trusty-installer port <member>` — the optional positional member is captured.
#[test]
fn parse_port_member() {
    let cli = Cli::try_parse_from(["trusty-installer", "port", "trusty-memory"])
        .expect("port member parses");
    match cli.command {
        Commands::Port { member, .. } => assert_eq!(member.as_deref(), Some("trusty-memory")),
        other => panic!("expected Port, got {other:?}"),
    }
    // Bare `trusty-installer port` leaves the member unset (handler defaults to search).
    let bare = Cli::try_parse_from(["trusty-installer", "port"]).expect("bare port parses");
    assert!(matches!(bare.command, Commands::Port { member: None, .. }));
}

/// Parse `trusty-installer doctor --self-check trusty-search`.
#[test]
fn parse_doctor_self_check() {
    let cli = Cli::try_parse_from([
        "trusty-installer",
        "doctor",
        "--self-check",
        "trusty-search",
    ])
    .expect("doctor --self-check parses");
    assert!(matches!(
        cli.command,
        Commands::Doctor {
            self_check: true,
            member: Some(_)
        }
    ));
}

/// Parse `trusty-installer ui`.
#[test]
fn parse_ui() {
    let cli = Cli::try_parse_from(["trusty-installer", "ui"]).expect("ui parses");
    assert!(matches!(cli.command, Commands::Ui { .. }));
}

/// `trusty-installer port --json-port` succeeds (standalone).
#[test]
fn parse_port_json_port() {
    let cli = Cli::try_parse_from(["trusty-installer", "port", "--json-port"])
        .expect("port --json-port parses");
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
    let cli = Cli::try_parse_from(["trusty-installer", "port", "--json", "--json-port"])
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
    let cli = Cli::try_parse_from(["trusty-installer", "--json", "port", "--json-port"])
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

/// Generic passthrough: `trusty-installer trusty-search doctor`.
#[test]
fn parse_passthrough() {
    let cli = Cli::try_parse_from(["trusty-installer", "trusty-search", "doctor"])
        .expect("passthrough parses");
    if let Commands::Passthrough(args) = &cli.command {
        assert_eq!(args.as_slice(), &["trusty-search", "doctor"]);
    } else {
        panic!("expected Passthrough");
    }
}

/// Bare `trusty-installer` with no subcommand must fail (arg_required_else_help).
#[test]
fn bare_installer_fails() {
    assert!(Cli::try_parse_from(["trusty-installer"]).is_err());
}

/// Global `--scope` propagates to nested subcommands.
#[test]
fn global_scope_propagates() {
    let cli = Cli::try_parse_from(["trusty-installer", "--scope", "project", "stack", "health"])
        .expect("scope propagates");
    assert_eq!(cli.scope, Some(ScopeArg::Project));
}

/// Global `--timeout` propagates to nested subcommands.
#[test]
fn global_timeout_propagates() {
    let cli = Cli::try_parse_from(["trusty-installer", "--timeout", "30", "version"])
        .expect("timeout propagates");
    assert_eq!(cli.timeout, Some(30));
}

/// `--yes` / `-y` are equivalent.
#[test]
fn yes_short_flag() {
    let long = Cli::try_parse_from(["trusty-installer", "--yes", "restart"]).expect("--yes parses");
    let short = Cli::try_parse_from(["trusty-installer", "-y", "restart"]).expect("-y parses");
    assert!(long.yes);
    assert!(short.yes);
}

/// `trusty-installer self-update` parses to `Commands::SelfUpdate` (Phase 3).
#[test]
fn parse_self_update() {
    let cli = Cli::try_parse_from(["trusty-installer", "self-update"]).expect("self-update parses");
    assert!(matches!(cli.command, Commands::SelfUpdate));
}

/// `tctl self-update` resolves identically (alias binary same entry point).
#[test]
fn tctl_alias_self_update() {
    let cli = Cli::try_parse_from(["tctl", "self-update"]).expect("tctl self-update parses");
    assert!(matches!(cli.command, Commands::SelfUpdate));
}
