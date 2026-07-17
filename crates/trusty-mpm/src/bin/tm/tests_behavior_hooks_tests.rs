//! CLI parse tests for `tm hooks` (issue #2940).
//!
//! Why: split into its own file rather than growing `tests.rs` (which was
//! already close to the 1500-SLOC test-file cap) — mirrors the
//! `tests_behavior_generate_tests.rs` precedent for a small, focused new
//! command-group parse suite.
//! What: `cli_parses_hooks_clean`, `cli_parses_hooks_clean_with_path_and_force`.
//! Test: this module IS the test suite for the `tm hooks` CLI surface.

use clap::Parser;

use crate::cli::{Cli, Command, HooksAction};

#[test]
fn cli_parses_hooks_clean() {
    let cli = Cli::try_parse_from(["trusty-mpm", "hooks", "clean"]).unwrap();
    match cli.command.unwrap() {
        Command::Hooks {
            action: HooksAction::Clean { path, force },
        } => {
            assert_eq!(path, None);
            assert!(!force);
        }
        other => panic!("expected Command::Hooks(Clean), got {other:?}"),
    }
}

#[test]
fn cli_parses_hooks_clean_with_path_and_force() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "hooks",
        "clean",
        "--path",
        "/tmp/some-project",
        "--force",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Hooks {
            action: HooksAction::Clean { path, force },
        } => {
            assert_eq!(path, Some("/tmp/some-project".to_string()));
            assert!(force);
        }
        other => panic!("expected Command::Hooks(Clean), got {other:?}"),
    }
}
