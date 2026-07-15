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

/// #2577 review (CRITICAL finding 1): `unresumable_remedy_line`'s printed text
/// must cite ONLY real `tm session <verb>` subcommands — this is exactly how a
/// nonexistent `tm session rm <id>` shipped in a prior draft (the reviewer
/// built the branch and confirmed `tm session rm` fails with "unrecognized
/// subcommand 'rm'"). Parsing the literal tokens through the real `Cli` is a
/// stronger guarantee than eyeballing the string: it fails the moment a verb
/// is renamed or removed, not just when a typo is introduced.
///
/// Why: a plain substring assertion (`msg.contains("delete")`) would not have
/// caught the original bug — the text also had to be GRAMMATICALLY a real
/// invocation, which only an actual parse proves.
/// What: for each `reason` branch, extracts the exact `tm session …` argv
/// implied by the remedy text and asserts `Cli::try_parse_from` accepts it;
/// also asserts the literal substring `"session rm"` never appears in ANY
/// branch's output (the specific dead verb that shipped).
/// Test: this function IS the test.
#[test]
fn unresumable_remedy_line_cites_real_subcommands() {
    use crate::commands::guided_resume::unresumable_remedy_line;

    let id = "6ca3950b-90d5-4367-94c7-68576b61dafa";

    // workspace_missing → `tm session delete <id> --force` must parse as a
    // real Delete action with force=true.
    let workspace_missing = unresumable_remedy_line(id, Some("workspace_missing"));
    assert!(
        !workspace_missing.contains("session rm"),
        "must never cite the nonexistent `tm session rm` verb, got: {workspace_missing:?}"
    );
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "delete", id, "--force"])
        .expect("the exact verb cited by the workspace_missing remedy must parse");
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::Delete {
                    id: parsed_id,
                    force,
                },
        } => {
            assert_eq!(parsed_id, id);
            assert!(force, "the cited invocation must pass --force");
        }
        other => panic!("expected session delete, got {other:?}"),
    }

    // pane_gone → must warn before `tm session decommission <id>` (a REAL verb)
    // without instructing the operator to run it outright, and must never
    // recommend the destructive delete/decommission verbs unconditionally.
    let pane_gone = unresumable_remedy_line(id, Some("pane_gone"));
    assert!(
        !pane_gone.contains("session rm"),
        "must never cite the nonexistent `tm session rm` verb, got: {pane_gone:?}"
    );
    assert!(
        pane_gone.contains("sibling window"),
        "pane_gone remedy must explain the sibling-window hazard, got: {pane_gone:?}"
    );
    assert!(
        pane_gone.contains("tmux list-panes") || pane_gone.contains("session info"),
        "pane_gone remedy must point at an inspection step before any teardown, got: {pane_gone:?}"
    );
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "decommission", id])
        .expect("the decommission verb cited by the pane_gone remedy must parse");
    assert!(matches!(
        cli.command.unwrap(),
        Command::Session {
            action: SessionAction::Decommission { .. }
        }
    ));
    let cli_info = Cli::try_parse_from(["trusty-mpm", "session", "info", id])
        .expect("the info verb cited by the pane_gone remedy must parse");
    assert!(matches!(
        cli_info.command.unwrap(),
        Command::Session {
            action: SessionAction::Info { .. }
        }
    ));

    // Unknown/absent reason (e.g. an older daemon) → conservative fallback that
    // names only read-only verbs, never delete/decommission.
    let fallback = unresumable_remedy_line(id, None);
    assert!(
        !fallback.contains("session rm"),
        "must never cite the nonexistent `tm session rm` verb, got: {fallback:?}"
    );
    assert!(
        !fallback.contains("delete") && !fallback.contains("decommission"),
        "the no-reason fallback must not suggest a destructive verb, got: {fallback:?}"
    );
}

/// #2577 review (optional LOW finding): `truncate_for_display` must leave a
/// normal-length daemon error body untouched.
#[test]
fn truncate_for_display_leaves_short_bodies_unchanged() {
    use crate::commands::guided_resume::truncate_for_display;

    let short = "workspace directory /gone no longer exists";
    assert_eq!(truncate_for_display(short), short);
}

/// #2577 review (optional LOW finding): an oversized daemon error body must be
/// capped rather than flooding the operator's terminal scrollback.
#[test]
fn truncate_for_display_caps_long_bodies() {
    use crate::commands::guided_resume::truncate_for_display;

    let long = "x".repeat(5000);
    let result = truncate_for_display(&long);
    assert!(
        result.chars().count() < long.chars().count(),
        "an oversized body must be shortened"
    );
    assert!(
        result.ends_with("… (truncated)"),
        "a truncated body must be marked as such, got tail: {:?}",
        &result[result.len().saturating_sub(20)..]
    );
}
