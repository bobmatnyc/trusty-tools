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

/// The `TERM` a real interactive terminal reports (#7224).
///
/// Why: every gate case below is about a flag or a TTY, not about `TERM`, so
/// they all pass the one value that keeps the terminal question out of the way.
/// The `TERM` question itself is
/// `ls_connector_dumb_term_reaches_the_static_renderer`.
const REAL_TERM: Option<&str> = Some("xterm-256color");

/// The picker opens only on a fully-interactive terminal with ≥1 session.
#[test]
fn ls_connector_should_show_picker_interactive_with_sessions() {
    assert!(should_show_picker(
        true, true, false, false, false, false, REAL_TERM, 1
    ));
    assert!(should_show_picker(
        true, true, false, false, false, false, REAL_TERM, 5
    ));
}

/// A non-TTY stdin OR stdout forces the static (pipeable) list path — never a
/// blocking picker. Mirrors guided.rs's non-TTY gate.
#[test]
fn ls_connector_should_show_picker_non_tty_static() {
    assert!(
        !should_show_picker(false, true, false, false, false, false, REAL_TERM, 3),
        "piped stdin -> static"
    );
    assert!(
        !should_show_picker(true, false, false, false, false, false, REAL_TERM, 3),
        "piped stdout -> static"
    );
    assert!(!should_show_picker(
        false, false, false, false, false, false, REAL_TERM, 3
    ));
}

/// `--json` and `--all` force static output even on a TTY, and 0 sessions never
/// opens an empty picker.
#[test]
fn ls_connector_should_show_picker_flags_and_empty_static() {
    assert!(
        !should_show_picker(true, true, true, false, false, false, REAL_TERM, 3),
        "--json -> static"
    );
    assert!(
        !should_show_picker(true, true, false, true, false, false, REAL_TERM, 3),
        "--all -> static"
    );
    assert!(
        !should_show_picker(true, true, false, false, false, false, REAL_TERM, 0),
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
        !should_show_picker(true, true, false, false, true, false, REAL_TERM, 3),
        "--attached -> static even with sessions on a TTY"
    );
    assert!(
        should_show_picker(true, true, false, false, false, false, REAL_TERM, 3),
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
        !should_show_picker(true, true, false, false, false, true, REAL_TERM, 3),
        "--plain -> static even with sessions on a TTY"
    );
    assert!(
        should_show_picker(true, true, false, false, false, false, REAL_TERM, 3),
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
    // (stdin_tty, stdout_tty, plain, term) — every non-interactive shape.
    let static_cases = [
        (true, true, true, REAL_TERM),      // --plain on a full TTY
        (true, false, false, REAL_TERM),    // piped stdout
        (false, true, false, REAL_TERM),    // piped stdin
        (false, false, false, REAL_TERM),   // both piped
        (true, false, true, REAL_TERM),     // piped stdout AND --plain
        (true, true, false, Some("dumb")),  // #7224: two TTYs, dumb terminal
        (true, true, false, None),          // #7224: two TTYs, TERM unset
        (true, true, false, Some("")),      // #7224: two TTYs, TERM empty
        (true, true, false, Some("DUMB")),  // #7224: the check is case-insensitive
        (false, true, false, Some("dumb")), // piped stdin AND dumb
    ];
    for (stdin_tty, stdout_tty, plain, term) in static_cases {
        assert!(
            prints_static_table(stdin_tty, stdout_tty, false, false, false, plain, term),
            "({stdin_tty},{stdout_tty},plain={plain},term={term:?}) must reach the static renderer"
        );
        assert!(
            !should_show_picker(stdin_tty, stdout_tty, false, false, false, plain, term, 3),
            "({stdin_tty},{stdout_tty},plain={plain},term={term:?}) must never open the TUI"
        );
    }
    // The one shape that does NOT take the static branch is the interactive one.
    assert!(
        !prints_static_table(true, true, false, false, false, false, REAL_TERM),
        "a full TTY with no forcing flag is the TUI path"
    );
    assert!(should_show_picker(
        true, true, false, false, false, false, REAL_TERM, 3
    ));
}

/// `TERM=dumb` under a real pty prints the static table and never opens the TUI.
///
/// Why (#7224): the TTY check alone is not the raw-mode question. `script`, an
/// Emacs shell buffer, and a CI pty all give `tm ls` two real TTYs while
/// reporting a `TERM` with no cursor addressing — the exact combination
/// `interactive_filter_allowed` has always refused for `tm f`, and the one
/// `should_show_picker` used to let through into raw mode.
/// What: pins both gates on the dumb/unset/empty values against an otherwise
/// fully interactive invocation, and asserts the same invocation with a real
/// `TERM` still opens the TUI — so the fix is a `TERM` gate, not a blanket off
/// switch.
#[test]
fn ls_connector_dumb_term_reaches_the_static_renderer() {
    for term in [Some("dumb"), Some("Dumb"), Some(""), None] {
        assert!(
            prints_static_table(true, true, false, false, false, false, term),
            "TERM={term:?} on two TTYs must take the static branch"
        );
        assert!(
            !should_show_picker(true, true, false, false, false, false, term, 3),
            "TERM={term:?} on two TTYs must never open the TUI"
        );
    }
    assert!(
        should_show_picker(true, true, false, false, false, false, Some("screen"), 3),
        "a real TERM on the same invocation still opens the TUI"
    );
}

// ── bare `tm` reaches the same surface (#7224) ───────────────────────────────

use crate::commands::session_ls_connector::bare_tm_opens_session_tui;

/// Bare `tm` on a real terminal opens the session TUI, exactly as `tm ls` does.
///
/// Why (#7224): this is the owner ruling — "bare tm should also open the TUI" —
/// and it FAILS on the parent commit, where `guided::try_show_picker` called
/// `run_tty_picker` unconditionally and no gate existed to assert against. It
/// is the positive mirror of
/// `ls_connector_dumb_term_reaches_the_static_renderer`: same operands, same
/// shape, opposite claim.
/// What: two TTYs, a cursor-addressable `TERM`, no managed pane, and a
/// non-empty fleet must open the TUI — for every `TERM` a real terminal
/// reports.
#[test]
fn bare_tm_opens_the_session_tui_on_two_ttys_with_a_capable_term() {
    for term in [
        Some("xterm-256color"),
        Some("screen"),
        Some("tmux-256color"),
    ] {
        assert!(
            bare_tm_opens_session_tui(true, true, term, false, 1),
            "TERM={term:?}: bare `tm` on two TTYs with a live fleet must open the TUI"
        );
    }
    assert!(
        bare_tm_opens_session_tui(true, true, REAL_TERM, false, 12),
        "fleet size is not a reason to refuse"
    );
}

/// Inside a tm-managed pane bare `tm` never opens the TUI.
///
/// Why (#7224): a managed pane is the pane whose agent bare `tm` relaunches in
/// place. A full-screen alternate-screen surface would take that pane over
/// instead, so the relaunch/"this pane" behavior has to survive the new
/// surface. `tm ls` in that same pane still opens the TUI — it reads the same
/// id as its self-delete guard, not as a refusal — which is why the operand
/// lives on this wrapper and not in `should_show_picker`.
/// What: a managed pane refuses on an otherwise fully interactive invocation
/// with a live fleet, and refuses for every `TERM` — the pane, not the
/// terminal, is what settles it.
#[test]
fn bare_tm_never_opens_the_tui_inside_a_managed_pane() {
    for term in [Some("xterm-256color"), Some("screen"), Some("dumb"), None] {
        assert!(
            !bare_tm_opens_session_tui(true, true, term, true, 3),
            "TERM={term:?}: a managed pane keeps the line picker, never a full-screen TUI"
        );
    }
    assert!(
        bare_tm_opens_session_tui(true, true, REAL_TERM, false, 3),
        "the same invocation outside a managed pane opens the TUI"
    );
}

/// Bare `tm` and `tm ls` refuse the TUI on exactly the same inputs.
///
/// Why (#7224): "a gate present on one surface and missing from the other" is
/// the defect round 2 removed when `tm f`'s `TERM` check turned out to be
/// absent from `tm ls`. Pinning the delegation as an IDENTITY — not a list of
/// cases — is what makes a future gate added to `should_show_picker` reach bare
/// `tm` for free, because a wrapper that stopped delegating would fail here.
/// What: over every TTY / `TERM` / fleet-size combination, with no managed
/// pane, the two decisions agree.
#[test]
fn bare_tm_and_ls_refuse_the_tui_on_the_same_inputs() {
    for stdin_tty in [true, false] {
        for stdout_tty in [true, false] {
            for term in [Some("xterm-256color"), Some("dumb"), Some(""), None] {
                for count in [0usize, 1, 7] {
                    assert_eq!(
                        bare_tm_opens_session_tui(stdin_tty, stdout_tty, term, false, count),
                        should_show_picker(
                            stdin_tty, stdout_tty, false, false, false, false, term, count
                        ),
                        "bare `tm` and `tm ls` disagree at \
                         stdin={stdin_tty} stdout={stdout_tty} TERM={term:?} count={count}"
                    );
                }
            }
        }
    }
}
