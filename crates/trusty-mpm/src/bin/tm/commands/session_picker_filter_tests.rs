//! Unit tests for the `tm f` type-to-filter picker.
//!
//! Why: a raw-mode key loop is not directly testable, so every decision it
//! makes was pushed into pure functions — the non-TTY gate, the match
//! predicate, and the state machine. Those are what this file covers. The
//! terminal drawing itself (`draw`, `read_filter_selection`) is hand-verified
//! and deliberately untested here; it contains no branching policy.
//!
//! Test: this file IS the test module for `commands::session_picker_filter`.

use super::*;

/// A session summary with everything but `name`/`task`/`source_id` defaulted.
fn s(name: &str, task: Option<&str>, source_id: Option<&str>) -> ManagedSessionSummary {
    ManagedSessionSummary {
        id: format!("id-{name}"),
        name: name.to_string(),
        state: "active".into(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: source_id.map(str::to_string),
        task: task.map(str::to_string),
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot: 1,
        deleted: false,
    }
}

// ── the non-TTY fallback gate ────────────────────────────────────────────────
//
// The highest-risk regression in `tm f`: raw mode on a pipe hangs any script
// that pipes `tm`. Each of these asserts one way the gate must say "no".

/// Why: a piped stdin EOFs instantly and a piped stdout must stay a clean,
/// greppable table. Either one alone is enough to refuse raw mode.
#[test]
fn interactive_filter_allowed_requires_both_ttys() {
    assert!(!interactive_filter_allowed(
        false,
        true,
        false,
        false,
        Some("xterm-256color")
    ));
    assert!(!interactive_filter_allowed(
        true,
        false,
        false,
        false,
        Some("xterm-256color")
    ));
    assert!(!interactive_filter_allowed(
        false,
        false,
        false,
        false,
        Some("xterm-256color")
    ));
}

/// Why: `--json` is a machine contract and `--all` is the forensic listing —
/// neither is a connect target, so both force static output even on a terminal.
#[test]
fn interactive_filter_allowed_false_for_json_and_all() {
    assert!(!interactive_filter_allowed(
        true,
        true,
        true,
        false,
        Some("xterm")
    ));
    assert!(!interactive_filter_allowed(
        true,
        true,
        false,
        true,
        Some("xterm")
    ));
}

/// Why: a dumb (or unset) `TERM` has no cursor addressing, so the in-place
/// redraw would smear the list down the screen instead of replacing it.
#[test]
fn interactive_filter_allowed_false_for_dumb_or_missing_term() {
    assert!(!interactive_filter_allowed(true, true, false, false, None));
    assert!(!interactive_filter_allowed(
        true,
        true,
        false,
        false,
        Some("")
    ));
    assert!(!interactive_filter_allowed(
        true,
        true,
        false,
        false,
        Some("dumb")
    ));
    assert!(!interactive_filter_allowed(
        true,
        true,
        false,
        false,
        Some("DUMB")
    ));
}

/// Why: the gate must still say yes in the one case it exists to serve, or
/// `tm f` is a slower `tm ls`.
#[test]
fn interactive_filter_allowed_true_on_a_real_terminal() {
    assert!(interactive_filter_allowed(
        true,
        true,
        false,
        false,
        Some("xterm-256color")
    ));
}

/// Why: `NO_COLOR` means "do not colour", not "do not be interactive". The row
/// renderer honours it; the gate must not conflate the two and silently drop
/// the operator into the static list.
#[test]
fn interactive_filter_allowed_ignores_no_color() {
    // The gate takes no NO_COLOR input at all — this asserts the signature has
    // not grown one, i.e. the same TTY inputs still decide yes.
    assert!(interactive_filter_allowed(
        true,
        true,
        false,
        false,
        Some("screen-256color")
    ));
}

// ── the match predicate ──────────────────────────────────────────────────────

/// Why: the operator types lowercase; session names carry mixed case.
#[test]
fn matches_pattern_is_case_insensitive_on_name() {
    let sess = s("tm-TrustyTools-01", None, None);
    assert!(matches_pattern(&sess, "trustytools"));
    assert!(matches_pattern(&sess, "TRUSTY"));
    assert!(!matches_pattern(&sess, "nope"));
}

/// Why: this is the whole reason `tm f` exists next to `tm ls` — a task
/// description or project slug mentioning the pattern must NOT match, or the
/// narrow lookup is just the broad one again.
#[test]
fn matches_pattern_ignores_task_and_project() {
    let sess = s("tm-alpha-01", Some("fix the api"), Some("acme/api-server"));
    assert!(!matches_pattern(&sess, "api"));
    assert!(matches_pattern(&sess, "alpha"));
}

/// Why: an empty filter box is the starting state; it must show everything.
#[test]
fn matches_pattern_empty_matches_everything() {
    assert!(matches_pattern(&s("anything", None, None), ""));
}

/// Why: the selection resolves through these indices back into the ORIGINAL
/// list, so they must be original-list positions, not positions in the
/// filtered copy.
#[test]
fn visible_indices_narrows_to_matching_names() {
    let sessions = vec![
        s("tm-api-01", None, None),
        s("tm-web-01", None, None),
        s("tm-api-02", None, None),
    ];
    assert_eq!(visible_indices(&sessions, "api"), vec![0, 2]);
    assert_eq!(visible_indices(&sessions, ""), vec![0, 1, 2]);
    assert!(visible_indices(&sessions, "zzz").is_empty());
}

// ── the state machine ────────────────────────────────────────────────────────

/// Why: typing must extend the pattern AND put the cursor back on the first
/// row — the row under the cursor a keystroke ago is a different session now.
#[test]
fn filter_state_char_appends_and_resets_selection() {
    let mut st = FilterState::new("ap");
    assert_eq!(st.apply(FilterKey::Down, 5), FilterAction::Redraw);
    assert_eq!(st.selected(), 1);
    assert_eq!(st.apply(FilterKey::Char('i'), 5), FilterAction::Redraw);
    assert_eq!(st.pattern(), "api");
    assert_eq!(st.selected(), 0, "an edit must re-anchor the selection");
}

/// Why: an edit that shrinks the list under a moved cursor is exactly how a
/// fuzzy picker opens the wrong session. The reset must hold when the new
/// visible count is smaller than the old selection index.
#[test]
fn filter_state_edit_resets_selection_when_list_shrinks() {
    let mut st = FilterState::new("");
    st.apply(FilterKey::Down, 10);
    st.apply(FilterKey::Down, 10);
    assert_eq!(st.selected(), 2);
    st.apply(FilterKey::Char('z'), 10);
    assert_eq!(st.selected(), 0);
    // …and Accept against the now-1-row list resolves to row 0, in range.
    assert_eq!(st.apply(FilterKey::Accept, 1), FilterAction::Accept);
    assert_eq!(st.selected(), 0);
}

/// Why: Backspace on an empty pattern is a no-op, not a redraw — redrawing on
/// every rejected keystroke flickers the list.
#[test]
fn filter_state_backspace_on_empty_pattern_is_ignored() {
    let mut st = FilterState::new("");
    assert_eq!(st.apply(FilterKey::Backspace, 3), FilterAction::Ignore);
    assert_eq!(st.pattern(), "");
    let mut st = FilterState::new("ab");
    assert_eq!(st.apply(FilterKey::Backspace, 3), FilterAction::Redraw);
    assert_eq!(st.pattern(), "a");
}

/// Why: Ctrl-U clears in one stroke; on an already-empty box it must be inert.
#[test]
fn filter_state_clear_pattern_empties_and_resets() {
    let mut st = FilterState::new("api");
    st.apply(FilterKey::Down, 4);
    assert_eq!(st.apply(FilterKey::ClearPattern, 4), FilterAction::Redraw);
    assert_eq!(st.pattern(), "");
    assert_eq!(st.selected(), 0);
    assert_eq!(st.apply(FilterKey::ClearPattern, 4), FilterAction::Ignore);
}

/// Why: the cursor must never run past the last visible row — an out-of-range
/// selection would index a session that is not on screen.
#[test]
fn filter_state_down_saturates_at_last_row() {
    let mut st = FilterState::new("");
    for _ in 0..10 {
        st.apply(FilterKey::Down, 3);
    }
    assert_eq!(st.selected(), 2);
    assert_eq!(st.apply(FilterKey::Down, 3), FilterAction::Ignore);
    // An empty result set has no row to move onto at all.
    let mut empty = FilterState::new("");
    assert_eq!(empty.apply(FilterKey::Down, 0), FilterAction::Ignore);
    assert_eq!(empty.selected(), 0);
}

/// Why: symmetric guard at the top of the list.
#[test]
fn filter_state_up_saturates_at_first_row() {
    let mut st = FilterState::new("");
    assert_eq!(st.apply(FilterKey::Up, 3), FilterAction::Ignore);
    assert_eq!(st.selected(), 0);
}

/// Why: Enter on "no matches" must do nothing. Accepting there would resolve
/// `visible[0]` on an empty vec — the picker's one genuine panic risk.
#[test]
fn filter_state_accept_with_no_visible_rows_is_ignored() {
    let mut st = FilterState::new("zzz");
    assert_eq!(st.apply(FilterKey::Accept, 0), FilterAction::Ignore);
    assert_eq!(st.apply(FilterKey::Accept, 1), FilterAction::Accept);
}

/// Why: Esc/Ctrl-C must leave, whatever else is on screen.
#[test]
fn filter_state_cancel_always_cancels() {
    let mut st = FilterState::new("api");
    assert_eq!(st.apply(FilterKey::Cancel, 0), FilterAction::Cancel);
    assert_eq!(st.apply(FilterKey::Cancel, 9), FilterAction::Cancel);
}

/// Why: the seed is the `tm f <pattern>` argument — it must arrive pre-typed
/// and editable, not as a locked-in pre-filter.
#[test]
fn filter_state_seeds_from_pattern_and_stays_editable() {
    let mut st = FilterState::new("api");
    assert_eq!(st.pattern(), "api");
    st.apply(FilterKey::Backspace, 2);
    assert_eq!(st.pattern(), "ap");
}
