//! CLI parsing for the bulk-read diversion pair (#6887).
//!
//! Why: split out of `tests_behavior_a.rs`, which sits 6 SLOC under the
//! 500-SLOC production cap — the same companion-file convention
//! `install_policy_tests.rs` and `tests_behavior_2903_skills_tests.rs` follow.
//! What: asserts `tm hook --divert-check` and `tm divert bulk-read` parse. The
//! launcher registers the first as a `PreToolUse` hook and the first's block
//! reason names the second, so a parse failure in either wires the feature to a
//! command that does not exist.
//! Test: this module IS the test suite.

use clap::Parser;

use crate::cli::{Cli, Command};

/// Why (#6887): the managed launcher registers `tm hook --divert-check` as the
/// bulk-read diversion hook, and `tm divert bulk-read` is the command its block
/// reason names. Both must parse, or the feature is wired to commands that do
/// not exist.
#[test]
fn cli_parses_hook_divert_check() {
    let cli = Cli::try_parse_from(["trusty-mpm", "hook", "--divert-check"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Hook {
            pm_guard: false,
            divert_check: true
        }
    ));
}

/// Why (#6887): see above — this is the worker half of the pair.
/// What: the files are required, `--prompt` is optional, and `--timeout-secs`
/// carries its default.
#[test]
fn cli_parses_divert_bulk_read() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "divert",
        "bulk-read",
        "a.rs",
        "b.rs",
        "--prompt",
        "what does this do?",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Divert {
            action:
                crate::cli::DivertAction::BulkRead {
                    files,
                    prompt,
                    timeout_secs,
                },
        } => {
            assert_eq!(files.len(), 2);
            assert_eq!(prompt.as_deref(), Some("what does this do?"));
            assert_eq!(
                timeout_secs,
                crate::commands::divert_worker::DEFAULT_TIMEOUT_SECS
            );
        }
        other => panic!("expected Divert, got {other:?}"),
    }

    // At least one file is required.
    assert!(
        Cli::try_parse_from(["trusty-mpm", "divert", "bulk-read"]).is_err(),
        "bulk-read must require at least one file"
    );
}
