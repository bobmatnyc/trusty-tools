//! CLI parse tests for the `tm session` -> `tm sessions` top-level rename
//! (issue #2116, DOC-35 §2.2/§3.2) — extracted from `tests.rs` to keep it
//! under the 1500-SLOC test-file cap, following the existing
//! `tests_behavior_a/b/c/d` split convention.
//!
//! Why: `cli_parses_session_singular` in `tests.rs` already covers the
//! deprecated singular alias parsing unchanged; this file adds the mirror
//! assertions for the new canonical plural (`cli_parses_sessions_plural_canonical`),
//! the full-verb-surface parity check (`cli_session_and_sessions_agree_for_every_verb`),
//! and the pure deprecation-message-text assertion
//! (`top_level_alias_notice_message`) — the process-level "printed exactly
//! once" property is proven separately by the `tm_sessions_alias_notice`
//! integration test, which spawns the real binary.
//! What: parse round-trips for `Command::Sessions` and a full sweep of every
//! `SessionAction` verb asserting `tm session <verb>` and `tm sessions <verb>`
//! parse to the identical action.
//! Test: `cargo test -p trusty-mpm` runs this file as part of the `tm` binary
//! test suite.

use clap::Parser;

use crate::cli::{Cli, Command, SessionAction};

#[test]
fn cli_parses_sessions_plural_canonical() {
    // #2116: `sessions` (plural) is now the canonical top-level spelling —
    // the mirror image of `cli_parses_session_singular` in `tests.rs`, parsing
    // into the new `Command::Sessions` variant.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "list"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessions {
            action: SessionAction::List { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected sessions list, got {other:?}"),
    }
}

#[test]
fn cli_session_and_sessions_agree_for_every_verb() {
    // #2116: proves `tm session <verb>` (deprecated alias) and `tm sessions
    // <verb>` (canonical) resolve to the IDENTICAL `SessionAction` for every
    // verb the enum carries — zero functional difference between the two
    // top-level spellings, only the notice in `emit_top_level_alias_notice`
    // differs. Comparing `{:?}` output sidesteps `SessionAction` not deriving
    // `PartialEq` (it only derives `Debug` via `#[derive(Debug, Subcommand)]`).
    let cases: &[&[&str]] = &[
        &["start"],
        &["stop", "id-1"],
        &["kill", "id-1"],
        &["list"],
        &["tui"],
        &["clean"],
        &["info", "id-1"],
        &["instructions"],
        &["events", "id-1"],
        &["breakers"],
        &["pause", "id-1"],
        &["resume", "id-1"],
        &["run", "id-1", "echo hi"],
        &["output", "id-1"],
        &["new", "https://example.com/o/r.git", "--task", "t"],
        &["ls"],
        &["activity", "id-1"],
        &["send", "id-1", "text"],
        &["answer", "id-1", "yes"],
        &["attach", "id-1"],
        &["managed-stop", "id-1"],
        &["runtime-stop", "id-1"],
        &["managed-resume", "id-1"],
        &["decommission", "id-1"],
        &["delete", "id-1"],
        &["prune-idle"],
        &["decommission-ephemeral"],
        &["catchup"],
        &["prune", "--state", "all"],
        &["prune-worktrees"],
    ];
    for verb_args in cases {
        let mut singular_args = vec!["trusty-mpm", "session"];
        singular_args.extend_from_slice(verb_args);
        let mut plural_args = vec!["trusty-mpm", "sessions"];
        plural_args.extend_from_slice(verb_args);

        let singular_action = match Cli::try_parse_from(singular_args)
            .unwrap_or_else(|e| panic!("`tm session {verb_args:?}` failed to parse: {e}"))
            .command
            .unwrap()
        {
            Command::Session { action } => action,
            other => panic!("expected Command::Session for {verb_args:?}, got {other:?}"),
        };
        let plural_action = match Cli::try_parse_from(plural_args)
            .unwrap_or_else(|e| panic!("`tm sessions {verb_args:?}` failed to parse: {e}"))
            .command
            .unwrap()
        {
            Command::Sessions { action } => action,
            other => panic!("expected Command::Sessions for {verb_args:?}, got {other:?}"),
        };
        assert_eq!(
            format!("{singular_action:?}"),
            format!("{plural_action:?}"),
            "session/sessions parsed to different actions for {verb_args:?}"
        );
    }
}

#[test]
fn top_level_alias_notice_message() {
    // #2116: the top-level `session` -> `sessions` alias reuses the shared
    // `deprecation_message` builder from the #1205 verb-level precedent — pure
    // message-text assertion, mirroring `deprecation_notice_format` in
    // `tests.rs`, since the `eprintln!` side effect itself is untestable here
    // (covered by the `tm_sessions_alias_notice` integration test spawning the
    // real binary).
    use crate::commands::managed::deprecation_message;
    assert_eq!(
        deprecation_message("session", "sessions"),
        "warning: 'session' is deprecated; use 'sessions'"
    );
}
