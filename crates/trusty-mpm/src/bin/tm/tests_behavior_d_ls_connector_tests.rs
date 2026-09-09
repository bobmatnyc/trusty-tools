//! CLI parse and gate tests for the top-level `tm ls` connector.
//!
//! Why: split out of `tests_behavior_d_tests.rs`, which crossed the 3000-SLOC
//! test-file cap when #7224 added the `--plain` coverage. The `tm ls` connector
//! surface — how the verb parses, and whether an invocation opens the
//! interactive session TUI or prints the static table — is one cohesive claim,
//! so it is the natural slice to move rather than an arbitrary cut.
//! What: `cli_parses_ls_*` parse round-trips, the
//! `ls_connector_should_show_picker_*` gate tests, and the #7224 `--plain`
//! opt-out. The inline sort/filter grammar and the `-a` listing tests stay in
//! the parent file with the rest of the session-manager verb surface.
//! Test: `cargo test -p trusty-mpm` runs this file as part of the `tm` binary
//! test suite.

use clap::Parser;

use crate::cli::{Cli, Command};

// ── Top-level `tm ls` connector CLI parse tests (#2311) ─────────────────────

/// Bare `tm ls` parses as the session connector (no `--projects`, all defaults).
///
/// Why: the top-level `tm ls` is now the interactive managed-session connector;
/// this asserts the default field values that route it to the session path.
/// What: parses `tm ls` and asserts every flag defaults false / `None`.
/// Test: this test.
#[test]
fn cli_parses_ls_connector_bare() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            terms,
            projects,
            json,
            source_id,
            current,
            all,
            attached,
            no_prune,
            plain,
            root,
        } => {
            assert!(
                terms.is_empty(),
                "bare `tm ls` must have no positional terms"
            );
            assert!(!projects, "bare `tm ls` must not set --projects");
            assert!(!json);
            assert!(source_id.is_none());
            assert!(!current);
            assert!(!all);
            assert!(!attached, "bare `tm ls` must not set --attached");
            assert!(!no_prune, "bare `tm ls` keeps the #4702 auto-prune");
            // #7224: bare `tm ls` opens the TUI, so --plain defaults off.
            assert!(!plain, "bare `tm ls` must not set --plain");
            assert!(root.is_none());
        }
        other => panic!("expected top-level ls, got {other:?}"),
    }
}

/// `tm ls --projects` routes to the legacy alias/project registry list.
#[test]
fn cli_parses_ls_projects() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--projects"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { projects, .. } => assert!(projects, "--projects must parse to true"),
        other => panic!("expected top-level ls --projects, got {other:?}"),
    }
}

/// `tm ls -p` is the short alias for `--projects`.
#[test]
fn cli_parses_ls_projects_short() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "-p"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { projects, .. } => assert!(projects, "-p must parse to true"),
        other => panic!("expected top-level ls -p, got {other:?}"),
    }
}

/// `tm ls --json` selects JSON output while staying in session (connector) mode.
#[test]
fn cli_parses_ls_json() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--json"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { json, projects, .. } => {
            assert!(json, "--json must parse to true");
            assert!(!projects, "--json alone must not imply --projects");
        }
        other => panic!("expected top-level ls --json, got {other:?}"),
    }
}

/// `tm ls -a` parses as attached-only, and does NOT set `--all`.
///
/// Why: `-a` conventionally means "all" (`ls -a`, `docker ps -a`), and `--all`
/// is a real neighbouring flag on this very command. This asserts the short
/// binds to `--attached` and leaves `--all` alone, so a future reader cannot
/// quietly swap them.
#[test]
fn cli_parses_ls_attached_short() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "-a"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            attached,
            all,
            projects,
            ..
        } => {
            assert!(attached, "-a must parse to --attached");
            assert!(!all, "-a must NOT set --all");
            assert!(!projects, "-a alone must not imply --projects");
        }
        other => panic!("expected top-level ls -a, got {other:?}"),
    }
}

/// `tm ls --attached` is the long spelling of `-a`.
#[test]
fn cli_parses_ls_attached_long() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--attached"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { attached, all, .. } => {
            assert!(attached, "--attached must parse to true");
            assert!(!all);
        }
        other => panic!("expected top-level ls --attached, got {other:?}"),
    }
}

/// `--all` stays long-only: it and `-a` are independent flags that can coexist.
#[test]
fn cli_parses_ls_all_and_attached_are_independent() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--all"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { all, attached, .. } => {
            assert!(all, "--all must parse to true");
            assert!(!attached, "--all must NOT imply --attached");
        }
        other => panic!("expected top-level ls --all, got {other:?}"),
    }
    let both = Cli::try_parse_from(["trusty-mpm", "ls", "--all", "-a"]).unwrap();
    match both.command.unwrap() {
        Command::Ls { all, attached, .. } => {
            assert!(
                all && attached,
                "--all -a must parse as both, not a conflict"
            );
        }
        other => panic!("expected top-level ls --all -a, got {other:?}"),
    }
}

/// `tm ls --current` derives the source_id scope from the cwd (session mode).
#[test]
fn cli_parses_ls_current() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--current"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            current, source_id, ..
        } => {
            assert!(current, "--current must parse to true");
            assert!(source_id.is_none());
        }
        other => panic!("expected top-level ls --current, got {other:?}"),
    }
}

/// `tm ls --source-id <slug>` sets the explicit fleet scope filter.
#[test]
fn cli_parses_ls_source_id() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--source-id", "owner/repo"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            source_id, current, ..
        } => {
            assert_eq!(source_id, Some("owner/repo".to_string()));
            assert!(!current);
        }
        other => panic!("expected top-level ls --source-id, got {other:?}"),
    }
}

/// `tm ls --current --source-id <slug>` is a parse error (mutually exclusive).
#[test]
fn cli_ls_source_id_and_current_conflict() {
    let result =
        Cli::try_parse_from(["trusty-mpm", "ls", "--current", "--source-id", "owner/repo"]);
    assert!(
        result.is_err(),
        "passing both --current and --source-id to `tm ls` must be a parse error"
    );
}

// ── `tm ls` picker/static gate (#2311) ──────────────────────────────────────

use crate::commands::session_ls_connector::should_show_picker;

/// The picker opens only on a fully-interactive terminal with ≥1 session.
#[test]
fn ls_connector_should_show_picker_interactive_with_sessions() {
    assert!(should_show_picker(
        true, true, false, false, false, false, 1
    ));
    assert!(should_show_picker(
        true, true, false, false, false, false, 5
    ));
}

/// A non-TTY stdin OR stdout forces the static (pipeable) list path — never a
/// blocking picker. Mirrors guided.rs's non-TTY gate.
#[test]
fn ls_connector_should_show_picker_non_tty_static() {
    assert!(
        !should_show_picker(false, true, false, false, false, false, 3),
        "piped stdin -> static"
    );
    assert!(
        !should_show_picker(true, false, false, false, false, false, 3),
        "piped stdout -> static"
    );
    assert!(!should_show_picker(
        false, false, false, false, false, false, 3
    ));
}

/// `--json` and `--all` force static output even on a TTY, and 0 sessions never
/// opens an empty picker.
#[test]
fn ls_connector_should_show_picker_flags_and_empty_static() {
    assert!(
        !should_show_picker(true, true, true, false, false, false, 3),
        "--json -> static"
    );
    assert!(
        !should_show_picker(true, true, false, true, false, false, 3),
        "--all -> static"
    );
    assert!(
        !should_show_picker(true, true, false, false, false, false, 0),
        "0 sessions -> static"
    );
}

/// `-a` forces the static listing even on a full TTY with sessions present.
///
/// Why: the sessions `-a` keeps are exactly the ones a client is already on, so
/// opening the picker to "connect" to one is a no-op. The flag is a listing
/// question, and it must answer in a pipeable table like `--all` does.
#[test]
fn ls_connector_should_show_picker_attached_static() {
    assert!(
        !should_show_picker(true, true, false, false, true, false, 3),
        "--attached -> static even with sessions on a TTY"
    );
    assert!(
        should_show_picker(true, true, false, false, false, false, 3),
        "the same invocation WITHOUT -a still opens the picker"
    );
}

// ── `tm ls --plain` — the TUI opt-out (#7224) ───────────────────────────────

use crate::commands::session_ls_connector::prints_static_table;

/// `tm ls --plain` parses, and bare `tm ls` leaves it off.
///
/// Why (#7224): `--plain` is the escape hatch from the session TUI back to the
/// static table. A default that flipped either way would change what bare
/// `tm ls` does on a terminal without anything saying so.
#[test]
fn cli_parses_ls_plain() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--plain"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { plain, .. } => assert!(plain, "--plain must parse to true"),
        other => panic!("expected top-level ls --plain, got {other:?}"),
    }
    let bare = Cli::try_parse_from(["trusty-mpm", "ls"]).unwrap();
    match bare.command.unwrap() {
        Command::Ls { plain, .. } => assert!(!plain, "bare `tm ls` must not set --plain"),
        other => panic!("expected top-level ls, got {other:?}"),
    }
}

/// `--plain` forces the static listing even on a full TTY with sessions.
#[test]
fn ls_connector_should_show_picker_plain_static() {
    assert!(
        !should_show_picker(true, true, false, false, false, true, 3),
        "--plain -> static even with sessions on a TTY"
    );
    assert!(
        should_show_picker(true, true, false, false, false, false, 3),
        "the same invocation WITHOUT --plain opens the TUI"
    );
}

/// `--plain` and a piped stream take the SAME branch to the static renderer,
/// and neither can reach the TUI (#7224).
///
/// Why: the TUI is the only thing in `tm ls` that puts the terminal into raw
/// mode, and it sits behind [`should_show_picker`] alone. Asserting that both
/// a piped stdout and an explicit `--plain` make `prints_static_table` true AND
/// `should_show_picker` false is what pins "a non-interactive `tm ls` prints
/// the table and never enters raw mode" — the function returns at the static
/// branch, before any terminal setup runs.
#[test]
fn plain_and_non_tty_reach_the_same_static_renderer() {
    // (stdin_tty, stdout_tty, plain) — every non-interactive shape.
    let static_cases = [
        (true, true, true),    // --plain on a full TTY
        (true, false, false),  // piped stdout
        (false, true, false),  // piped stdin
        (false, false, false), // both piped
        (true, false, true),   // piped stdout AND --plain
    ];
    for (stdin_tty, stdout_tty, plain) in static_cases {
        assert!(
            prints_static_table(stdin_tty, stdout_tty, false, false, false, plain),
            "({stdin_tty},{stdout_tty},plain={plain}) must reach the static renderer"
        );
        assert!(
            !should_show_picker(stdin_tty, stdout_tty, false, false, false, plain, 3),
            "({stdin_tty},{stdout_tty},plain={plain}) must never open the TUI"
        );
    }
    // The one shape that does NOT take the static branch is the interactive one.
    assert!(
        !prints_static_table(true, true, false, false, false, false),
        "a full TTY with no forcing flag is the TUI path"
    );
    assert!(should_show_picker(
        true, true, false, false, false, false, 3
    ));
}
