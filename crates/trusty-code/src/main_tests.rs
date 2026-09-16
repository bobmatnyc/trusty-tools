//! Clap-surface tests for the `tcode` binary (#8031).
//!
//! Why: the flag-to-field mapping in `main.rs` is the one link in the
//! `--no-delegate` chain that no library test can reach — `Command::RunTask`'s
//! fields are only produced by clap. A flag that parses into the wrong field,
//! or defaults the wrong way, would otherwise ship silently.
//! What: parses argv through the real `Cli` derive and asserts the parsed
//! variant's fields. No process is spawned and no daemon is contacted.
//! Test: this module is itself the test surface.

use clap::Parser;

use super::{Cli, Command};

/// Parse argv through the real `Cli` and return the `RunTask` variant's
/// `no_delegate` field.
fn no_delegate_of(argv: &[&str]) -> bool {
    let cli = Cli::try_parse_from(argv).expect("argv must parse");
    match cli.command {
        Command::RunTask { no_delegate, .. } => no_delegate,
        other => panic!("expected RunTask, got {:?}", std::mem::discriminant(&other)),
    }
}

/// `--no-delegate` sets the flag; omitting it leaves the pre-#8031 default.
///
/// Why: the issue's closure command is `tcode run-task engineer --no-delegate
/// "<task>"`, so the exact spelling must reach `Command::RunTask.no_delegate`.
/// What: parses the closure command and its no-flag twin.
/// Test: this test.
#[test]
fn run_task_no_delegate_flag_parses() {
    assert!(
        no_delegate_of(&["tcode", "run-task", "engineer", "--no-delegate", "do it"]),
        "--no-delegate must set no_delegate"
    );
    assert!(
        !no_delegate_of(&["tcode", "run-task", "engineer", "do it"]),
        "omitting --no-delegate must leave it false"
    );
}
