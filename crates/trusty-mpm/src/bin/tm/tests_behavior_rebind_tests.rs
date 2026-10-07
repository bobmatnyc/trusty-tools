//! CLI parse tests for `tm sessions rebind` (#9313).
//!
//! What: the one-session form with and without `--tmux`, the `--all` form,
//! and the parse errors for neither or both.

use clap::Parser;

use crate::cli::{Cli, Command, SessionAction};

/// `tm sessions rebind <id-or-name> [--tmux <session>]`.
#[test]
fn cli_parses_sessions_rebind() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "sessions",
        "rebind",
        "tm-supervisor",
        "--tmux",
        "tm-architect",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Sessions {
            action: SessionAction::Rebind { id, tmux, all },
        } => {
            assert_eq!(id.as_deref(), Some("tm-supervisor"));
            assert_eq!(tmux.as_deref(), Some("tm-architect"));
            assert!(!all);
        }
        other => panic!("expected sessions rebind, got {other:?}"),
    }
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "rebind", "tm-dogfood"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Sessions {
            action: SessionAction::Rebind {
                tmux: None,
                all: false,
                ..
            }
        }
    ));
}

/// `tm sessions rebind --all`; neither an id nor `--all`, or `--all` with an
/// id or `--tmux`, is a parse error.
#[test]
fn cli_parses_sessions_rebind_all() {
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "rebind", "--all"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessions {
            action: SessionAction::Rebind { id, tmux, all },
        } => {
            assert_eq!(id, None);
            assert_eq!(tmux, None);
            assert!(all);
        }
        other => panic!("expected sessions rebind --all, got {other:?}"),
    }
    for argv in [
        vec!["trusty-mpm", "sessions", "rebind"],
        vec!["trusty-mpm", "sessions", "rebind", "x", "--all"],
        vec!["trusty-mpm", "sessions", "rebind", "--all", "--tmux", "y"],
    ] {
        assert!(
            Cli::try_parse_from(&argv).is_err(),
            "{argv:?} must not parse"
        );
    }
}
