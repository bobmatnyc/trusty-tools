//! CLI parse tests for `tm f`, the type-to-filter session picker.
//!
//! Why: split out of `tests_behavior_d_tests.rs`, which crossed the 3000-SLOC
//! test-file cap (#9097). The `tm f` parse surface is self-contained and needs
//! none of the parent file's session-row builders.
//! What: `cli_parses_f_*` parse round-trips, the `--source-id`/`--current`
//! conflict, and the no-shadowing check. The NAME-only filter-scope test stays
//! in the parent file beside `ls_test_session`.
//! Test: `cargo test -p trusty-mpm` runs this file as part of the `tm` binary
//! test suite.

use clap::Parser;

use crate::cli::{Cli, Command};

// ── `tm f` — the type-to-filter picker's CLI surface ─────────────────────────
//
// `f` is a new top-level command under `infer_subcommands = true`. No other
// subcommand starts with `f`, so it shadows nothing; these tests pin that the
// exact spelling resolves to `Command::F` and that its flags mirror `ls`.

/// `tm f api` captures the pattern positionally.
#[test]
fn cli_parses_f_pattern() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f", "api"]).unwrap();
    match cli.command.unwrap() {
        Command::F(a) => assert_eq!(a.pattern, vec!["api".to_string()]),
        other => panic!("expected top-level f, got {other:?}"),
    }
}

/// Multi-word patterns are captured in order; the handler joins them with a
/// single space. Unlike `ls`, the first word is NEVER a sort keyword — `alpha`
/// here is part of the pattern.
#[test]
fn cli_parses_f_multi_word_pattern() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f", "alpha", "api"]).unwrap();
    match cli.command.unwrap() {
        Command::F(a) => {
            assert_eq!(a.pattern, vec!["alpha".to_string(), "api".to_string()])
        }
        other => panic!("expected top-level f, got {other:?}"),
    }
}

/// Bare `tm f` opens the picker with an empty filter box — the pattern is a
/// seed, not a required argument.
#[test]
fn cli_parses_f_bare_is_allowed() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f"]).unwrap();
    match cli.command.unwrap() {
        Command::F(a) => assert!(a.pattern.is_empty()),
        other => panic!("expected top-level f, got {other:?}"),
    }
}

/// `--json` parses and rides alongside the pattern; the handler uses it to skip
/// the interactive path entirely.
#[test]
fn cli_parses_f_json() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f", "--json", "api"]).unwrap();
    match cli.command.unwrap() {
        Command::F(a) => {
            assert_eq!(a.pattern, vec!["api".to_string()]);
            assert!(a.json);
        }
        other => panic!("expected top-level f --json, got {other:?}"),
    }
}

/// `--source-id` and `--current` are mutually exclusive, matching `tm ls`.
#[test]
fn cli_f_source_id_and_current_conflict() {
    let err = Cli::try_parse_from([
        "trusty-mpm",
        "f",
        "--source-id",
        "owner/repo",
        "--current",
        "api",
    ]);
    assert!(err.is_err(), "--source-id and --current must conflict");
}

/// `tm f` must not shadow any pre-existing subcommand. Under
/// `infer_subcommands`, an `f` that collided with (say) a hypothetical `fix`
/// would either resolve to that command or error as ambiguous — this pins that
/// it resolves to `Command::F` itself.
#[test]
fn cli_f_does_not_shadow_another_subcommand() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f", "x"]).unwrap();
    assert!(matches!(cli.command, Some(Command::F(_))));
}
