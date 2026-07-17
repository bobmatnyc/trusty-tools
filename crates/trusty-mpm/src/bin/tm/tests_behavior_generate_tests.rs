//! CLI parse tests for `tm generate` (issue #2913) — extracted into its own
//! file (rather than added to `tests.rs`) because `tests.rs` was already at
//! the 1500-SLOC test-file cap; follows the existing `tests_behavior_a/b/c/
//! d/e` split convention.
//!
//! Why: `Command::Generate`/`GenerateAction` need the same parse-round-trip
//! coverage every other command group gets (see `cli_parses_agent_list` in
//! `tests.rs` for the pattern this mirrors).
//! What: parse round-trips for `tm generate capabilities` (write mode) and
//! `tm generate capabilities --check` (the CI drift-gate mode).
//! Test: `cargo test -p trusty-mpm` runs this file as part of the `tm`
//! binary test suite.

use clap::Parser;

use crate::cli::{Cli, Command, GenerateAction};

#[test]
fn cli_parses_generate_capabilities() {
    // `tm generate capabilities` (no --check) writes the regenerated
    // tm-capabilities files.
    let cli = Cli::try_parse_from(["trusty-mpm", "generate", "capabilities"]).unwrap();
    match cli.command.unwrap() {
        Command::Generate {
            action: GenerateAction::Capabilities { check },
        } => assert!(!check),
        other => panic!("expected Command::Generate capabilities, got {other:?}"),
    }
}

#[test]
fn cli_parses_generate_capabilities_check() {
    // `tm generate capabilities --check` is the CI drift gate — diffs only,
    // never writes.
    let cli = Cli::try_parse_from(["trusty-mpm", "generate", "capabilities", "--check"]).unwrap();
    match cli.command.unwrap() {
        Command::Generate {
            action: GenerateAction::Capabilities { check },
        } => assert!(check),
        other => panic!("expected Command::Generate capabilities --check, got {other:?}"),
    }
}
