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
//!
//! Also carries the `#1809`/`#3034`/`#3044` decommissioned/deleted-tombstone
//! picker-filter coverage (`is_live_session_state_*`, `picker_filter_*`),
//! moved here from `tests_behavior_c_tests.rs` when the #3044 reconciliation's
//! added tests pushed that file over the 1500-SLOC test cap — this file had
//! headroom under the same `tests_behavior_c/d/e` split convention.
//! Test: `cargo test -p trusty-mpm` runs this file as part of the `tm` binary
//! test suite.

use clap::Parser;

use crate::cli::{Cli, Command, SessionAction};
use crate::commands::managed::{filter_live_sessions, is_live_session_state};

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

// ── #1809: decommissioned-tombstone filter ────────────────────────────────────

#[test]
fn picker_filter_live_state_excludes_decommissioned() {
    // Why (#1809): `is_live_session_state` is the canonical predicate for
    // "should this session appear in the picker / sessions list by default?".
    // Test: concrete state → expected bool, not derived from the same expression.
    // None of these are #3034 slot tombstones, so `is_slot_tombstone` is `false`
    // throughout — it is only consulted when `state == "deleted"`.
    assert!(
        !is_live_session_state("decommissioned", false),
        "decommissioned must be excluded from default view"
    );
    // Active sessions must always be visible.
    assert!(
        is_live_session_state("active", false),
        "active must be included in default view"
    );
    // Stopped/errored sessions can still be resumed — they must show.
    assert!(
        is_live_session_state("stopped", false),
        "stopped must be included in default view"
    );
    assert!(
        is_live_session_state("errored", false),
        "errored must be included in default view"
    );
    // Provisioning sessions are in-flight — they must show.
    assert!(
        is_live_session_state("provisioning", false),
        "provisioning must be included in default view"
    );
}

#[test]
fn is_live_session_state_excludes_soft_deleted_record() {
    // Reconciles the #3302 hardening commit (51243ea5, code-critic CRITICAL)
    // with #3034/#3044: a `"deleted"` row backed by a REAL, still-in-store
    // record (soft-deleted via `tm sessions delete`, #2012 — NOT a #3034
    // numbered-slot tombstone, so `is_slot_tombstone` is `false`) must stay
    // excluded from the default picker/list exactly like `decommissioned`, so
    // it is never offered as a resume target (which would resurrect it).
    assert!(
        !is_live_session_state("deleted", false),
        "a soft-deleted, still-in-store record must be excluded from the \
         default picker/list view"
    );
    assert!(!is_live_session_state("decommissioned", false));
    assert!(is_live_session_state("active", false));
    assert!(is_live_session_state("stopped", false));
}

#[test]
fn is_live_session_state_keeps_slot_tombstone_visible() {
    // Why (#3034/#3044): a stable-numbering SLOT tombstone — the daemon's
    // `tombstone_summary` placeholder for a slot whose record left the store
    // entirely — is ALSO rendered with wire `state == "deleted"`, but it must
    // stay VISIBLE at its slot in the default view, or the entire point of
    // stable numbering (an operator seeing exactly why a captured number no
    // longer resolves) is defeated. `is_slot_tombstone == true` is what
    // distinguishes it from the soft-deleted-record case above.
    assert!(
        is_live_session_state("deleted", true),
        "a #3034 slot tombstone must remain visible in the default view"
    );
    // Resurrection-safety for this visible row is NOT this predicate's job —
    // see `guided_resume_tests`/`session_picker.rs`'s `decide_for_index` for
    // the separate guards that keep it non-resumable regardless.
}

#[test]
fn picker_filter_excludes_decommissioned_keeps_active() {
    // Why (#1809): `filter_live_sessions` must drop decommissioned tombstones and
    // retain all other states. We construct a mixed slice and assert concrete counts
    // and membership — not the same expression used to compute the filter.
    let sessions: Vec<trusty_mpm::client::ManagedSessionSummary> =
        serde_json::from_value(serde_json::json!([
            { "id": "a1", "name": "sess-active",        "state": "active" },
            { "id": "b2", "name": "sess-dead-1",        "state": "decommissioned" },
            { "id": "c3", "name": "sess-stopped",       "state": "stopped" },
            { "id": "d4", "name": "sess-dead-2",        "state": "decommissioned" },
            { "id": "e5", "name": "sess-provisioning",  "state": "provisioning" },
        ]))
        .expect("test data must deserialize");

    let filtered = filter_live_sessions(sessions);

    // Exactly 3 of the 5 sessions survive the filter.
    assert_eq!(
        filtered.len(),
        3,
        "filter must keep exactly 3 live sessions (active, stopped, provisioning)"
    );
    // Active session must be present.
    assert!(
        filtered.iter().any(|s| s.state == "active"),
        "active session must survive filter"
    );
    // Stopped session must be present (can be resumed).
    assert!(
        filtered.iter().any(|s| s.state == "stopped"),
        "stopped session must survive filter"
    );
    // Provisioning session must be present (in-flight).
    assert!(
        filtered.iter().any(|s| s.state == "provisioning"),
        "provisioning session must survive filter"
    );
    // Neither decommissioned session must appear.
    assert!(
        !filtered.iter().any(|s| s.state == "decommissioned"),
        "decommissioned tombstones must be excluded"
    );
}

#[test]
fn picker_filter_all_live_sessions_unchanged() {
    // Why: when no sessions are decommissioned, `filter_live_sessions` must
    // return all sessions unchanged — no unexpected truncation.
    let sessions: Vec<trusty_mpm::client::ManagedSessionSummary> =
        serde_json::from_value(serde_json::json!([
            { "id": "x1", "name": "sess-a", "state": "active" },
            { "id": "x2", "name": "sess-b", "state": "stopped" },
            { "id": "x3", "name": "sess-c", "state": "errored" },
        ]))
        .expect("test data must deserialize");

    let filtered = filter_live_sessions(sessions);
    assert_eq!(
        filtered.len(),
        3,
        "all-live input must pass through unchanged (3 sessions)"
    );
}

#[test]
fn picker_filter_all_decommissioned_returns_empty() {
    // Why: if every session is decommissioned, the picker must show an empty list
    // (not crash or return some sessions).
    let sessions: Vec<trusty_mpm::client::ManagedSessionSummary> =
        serde_json::from_value(serde_json::json!([
            { "id": "z1", "name": "old-1", "state": "decommissioned" },
            { "id": "z2", "name": "old-2", "state": "decommissioned" },
        ]))
        .expect("test data must deserialize");

    let filtered = filter_live_sessions(sessions);
    assert!(
        filtered.is_empty(),
        "all-decommissioned input must produce empty list"
    );
}

#[test]
fn picker_filter_keeps_slot_tombstone_hides_soft_deleted_record() {
    // Why (#3034/#3044 reconciliation): both rows below serialize wire
    // `state == "deleted"`, but only the slot-tombstone row sets the
    // dedicated `deleted: bool` field — `filter_live_sessions` must tell them
    // apart via that field rather than the state string alone.
    let sessions: Vec<trusty_mpm::client::ManagedSessionSummary> =
        serde_json::from_value(serde_json::json!([
            { "id": "a1", "name": "sess-active", "state": "active" },
            // #3034 numbered-slot tombstone — must stay visible.
            { "id": "", "name": "", "state": "deleted", "slot": 2, "deleted": true },
            // #3302 soft-deleted, still-in-store record — must stay hidden.
            { "id": "b2", "name": "sess-soft-deleted", "state": "deleted" },
        ]))
        .expect("test data must deserialize");

    let filtered = filter_live_sessions(sessions);

    assert_eq!(
        filtered.len(),
        2,
        "exactly the active session and the slot tombstone must survive"
    );
    assert!(
        filtered.iter().any(|s| s.deleted),
        "the slot tombstone (deleted: true) must survive the filter"
    );
    assert!(
        !filtered.iter().any(|s| s.state == "deleted" && !s.deleted),
        "the soft-deleted, still-in-store record (deleted: false) must be excluded"
    );
}
