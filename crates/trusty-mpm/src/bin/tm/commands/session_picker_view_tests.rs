//! Unit tests for the in-picker sort/filter grammar and the pinned default
//! (#3552).
//!
//! Why: the picker's key handling has always been split into a pure
//! input→decision seam and an I/O driver precisely so behavior like this is
//! assertable without a terminal, a tmux server, or a daemon. Every test here
//! drives pure functions — `parse_view_command`, `apply_view_command`,
//! `default_index`, `prepare_menu`, `command_legend` — so the whole feature is
//! covered with no TTY.
//! What: the four properties the feature is specified by — the sort key cycles
//! and wraps, the filter narrows and clears, the pinned default survives a
//! re-sort, and the legend names both keys — plus the grammar's boundaries.
//! Test: this file.

use trusty_mpm::client::ManagedSessionSummary;

use super::{ViewCommand, apply_view_command, default_index, parse_view_command, view_status_line};
use crate::commands::session_picker::{
    PickerScope, SessionFilter, SessionSortArg, parse_picker_choice,
};
use crate::commands::session_picker_order::prepare_menu;
use crate::commands::session_picker_render::command_legend;

/// Minimal `ManagedSessionSummary` fixture: a name, a state, and a slot.
///
/// `id` is derived from `name` so the pinned-selection tests can name a session
/// the same way the driver does.
fn session(name: &str, state: &str, slot: u32) -> ManagedSessionSummary {
    ManagedSessionSummary {
        id: format!("{name}-id"),
        name: name.to_string(),
        state: state.to_string(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot,
        deleted: false,
        auto_resume_parked: None,
    }
}

/// Three active sessions whose alphabetical order differs from their slot
/// order, so a re-sort is observable.
fn fleet() -> Vec<ManagedSessionSummary> {
    vec![
        session("zeta", "active", 1),
        session("alpha-api", "active", 2),
        session("middle", "active", 3),
    ]
}

/// A scope with no filter, no pin, and the default sort.
fn scope() -> PickerScope {
    PickerScope::project("owner/repo", "/repo")
}

// ── parse_view_command grammar ───────────────────────────────────────────────

#[test]
fn parse_view_command_s_cycles_sort() {
    assert_eq!(parse_view_command("s"), Some(ViewCommand::CycleSort));
}

#[test]
fn parse_view_command_sort_word_cycles_sort() {
    assert_eq!(parse_view_command("sort"), Some(ViewCommand::CycleSort));
}

#[test]
fn parse_view_command_is_case_insensitive() {
    assert_eq!(parse_view_command("S\n"), Some(ViewCommand::CycleSort));
    assert_eq!(parse_view_command("  SORT "), Some(ViewCommand::CycleSort));
}

#[test]
fn parse_view_command_slash_sets_the_filter() {
    assert_eq!(
        parse_view_command("/api"),
        Some(ViewCommand::SetFilter("api".to_string()))
    );
}

/// A multi-word filter survives intact — the term is matched as a substring,
/// so `auth fix` must not be split into two needles.
#[test]
fn parse_view_command_slash_filter_is_trimmed() {
    assert_eq!(
        parse_view_command("/  auth fix  \n"),
        Some(ViewCommand::SetFilter("auth fix".to_string()))
    );
}

/// Backspacing the filter box back to a bare `/` clears it.
#[test]
fn parse_view_command_bare_slash_clears_the_filter() {
    assert_eq!(parse_view_command("/"), Some(ViewCommand::ClearFilter));
    assert_eq!(parse_view_command("/   \n"), Some(ViewCommand::ClearFilter));
}

/// Esc arrives as a raw `\x1b` inside the line, not as a keypress.
#[test]
fn parse_view_command_esc_clears_the_filter() {
    assert_eq!(parse_view_command("\u{1b}"), Some(ViewCommand::ClearFilter));
    assert_eq!(
        parse_view_command("  \u{1b}\n"),
        Some(ViewCommand::ClearFilter)
    );
}

/// The view grammar is a strict ADDITION: every pre-existing picker token must
/// still fall through to the parser's own branches.
#[test]
fn parse_view_command_declines_every_other_token() {
    for token in [
        "",
        "q",
        "ls",
        "list",
        "n",
        "n auth",
        "1",
        "12",
        "d1",
        "d tm-*",
        "r1 new-name",
        "start",
        "session",
    ] {
        assert_eq!(
            parse_view_command(token),
            None,
            "'{token}' must not be read as a view command"
        );
    }
}

// ── Property 1: the sort key cycles through the documented orders and wraps ──

#[test]
fn sort_cycle_visits_every_documented_order_and_wraps() {
    let mut seen = Vec::new();
    let mut sort = SessionSortArg::Recent;
    for _ in 0..SessionSortArg::CYCLE.len() {
        seen.push(sort);
        sort = sort.next();
    }
    assert_eq!(seen, SessionSortArg::CYCLE.to_vec());
    assert_eq!(
        sort,
        SessionSortArg::Recent,
        "one full cycle must wrap back to where it started"
    );
}

#[test]
fn sort_next_wraps_from_the_last_order() {
    assert_eq!(SessionSortArg::Recent.next(), SessionSortArg::Alpha);
    assert_eq!(SessionSortArg::Alpha.next(), SessionSortArg::Recent);
}

/// Pressing `[s]` from inside the picker advances the scope's sort and, on the
/// second press, wraps — the same cycle the legend advertises.
#[test]
fn apply_view_command_cycle_advances_and_wraps() {
    let mut scope = scope();
    assert_eq!(scope.sort, SessionSortArg::Recent);
    apply_view_command(&mut scope, &ViewCommand::CycleSort);
    assert_eq!(scope.sort, SessionSortArg::Alpha);
    apply_view_command(&mut scope, &ViewCommand::CycleSort);
    assert_eq!(scope.sort, SessionSortArg::Recent);
}

/// The cycled order reaches the render: after `[s]` the menu is alphabetical.
#[test]
fn cycling_sort_reorders_the_rendered_menu() {
    let mut scope = scope();
    let before = prepare_menu(fleet(), &scope);
    assert_eq!(before.sessions[0].name, "zeta", "recent keeps daemon order");

    apply_view_command(&mut scope, &ViewCommand::CycleSort);
    let after = prepare_menu(fleet(), &scope);
    let names: Vec<&str> = after.sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha-api", "middle", "zeta"]);
}

// ── Property 2: the filter narrows, and clearing it restores ────────────────

#[test]
fn apply_view_command_set_filter_narrows_then_clear_restores() {
    let mut scope = scope();
    assert_eq!(prepare_menu(fleet(), &scope).sessions.len(), 3);

    apply_view_command(&mut scope, &ViewCommand::SetFilter("alpha".to_string()));
    let narrowed = prepare_menu(fleet(), &scope);
    let names: Vec<&str> = narrowed.sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha-api"], "the filter must narrow the rows");

    apply_view_command(&mut scope, &ViewCommand::ClearFilter);
    assert_eq!(
        prepare_menu(fleet(), &scope).sessions.len(),
        3,
        "clearing the filter must restore every row"
    );
}

/// The whole path, from typed line to rendered rows: `/alpha` narrows, Esc
/// restores.
#[test]
fn typing_a_filter_then_esc_restores_the_rows() {
    let mut scope = scope();
    let typed = parse_view_command("/alpha").expect("a slash line is a view command");
    apply_view_command(&mut scope, &typed);
    assert_eq!(prepare_menu(fleet(), &scope).sessions.len(), 1);

    let esc = parse_view_command("\u{1b}").expect("Esc is a view command");
    apply_view_command(&mut scope, &esc);
    assert_eq!(prepare_menu(fleet(), &scope).sessions.len(), 3);
}

// ── Property 3: the pinned default survives a re-sort or filter ─────────────

#[test]
fn default_index_pins_the_selected_session_across_a_resort() {
    let sessions = fleet();
    assert_eq!(default_index(&sessions, Some("middle-id")), 2);
}

#[test]
fn default_index_falls_back_to_zero_when_the_pin_is_filtered_out() {
    let sessions = vec![session("alpha-api", "active", 2)];
    assert_eq!(default_index(&sessions, Some("middle-id")), 0);
}

#[test]
fn default_index_falls_back_to_zero_without_a_pin() {
    assert_eq!(default_index(&fleet(), None), 0);
}

/// The property the feature is specified by: cycling the sort reorders the
/// rows, and [Enter] still targets the SAME session.
#[test]
fn prepare_menu_keeps_the_pinned_default_across_a_resort() {
    let mut scope = scope();
    scope.selected_id = Some("middle-id".to_string());

    let before = prepare_menu(fleet(), &scope);
    let pinned = before.sessions[before.default_idx].id.clone();
    assert_eq!(pinned, "middle-id");

    apply_view_command(&mut scope, &ViewCommand::CycleSort);
    let after = prepare_menu(fleet(), &scope);
    assert_ne!(
        after.sessions[0].id, pinned,
        "the re-sort must actually move the pinned session off row 0"
    );
    assert_eq!(
        after.sessions[after.default_idx].id, pinned,
        "[Enter] must still target the pinned session after a re-sort"
    );
}

/// A pin the filter hides releases the default to row 0 rather than pointing at
/// a row nobody can see.
#[test]
fn prepare_menu_releases_a_pin_the_filter_hid() {
    let mut scope = scope();
    scope.selected_id = Some("middle-id".to_string());
    apply_view_command(&mut scope, &ViewCommand::SetFilter("alpha".to_string()));

    let menu = prepare_menu(fleet(), &scope);
    assert_eq!(menu.default_idx, 0);
    assert_eq!(menu.sessions[menu.default_idx].id, "alpha-api-id");
}

/// Bare Enter goes through the pinned index, not position 0.
#[test]
fn parse_picker_choice_bare_enter_targets_the_pinned_default() {
    let sessions = fleet();
    assert_eq!(
        parse_picker_choice("", &sessions, 2, false),
        crate::commands::session_picker::PickerDecision::Resume(2)
    );
}

// ── Property 4: the legend names both keys ──────────────────────────────────

#[test]
fn command_legend_names_the_sort_and_filter_keys() {
    for slot in [None, Some(4)] {
        let lines = command_legend(slot);
        let sort_row = lines
            .iter()
            .find(|l| l.starts_with("[s]"))
            .unwrap_or_else(|| panic!("the sort row must be present for {slot:?}"));
        assert!(
            sort_row.contains("recent") && sort_row.contains("alpha"),
            "the sort row must name the orders it cycles: {sort_row}"
        );
        let filter_row = lines
            .iter()
            .find(|l| l.starts_with("[/<text>]"))
            .unwrap_or_else(|| panic!("the filter row must be present for {slot:?}"));
        assert!(
            filter_row.contains("clears"),
            "the filter row must name how to clear it: {filter_row}"
        );
    }
}

// ── view_status_line ────────────────────────────────────────────────────────

#[test]
fn view_status_line_names_the_active_sort() {
    let line = view_status_line(SessionSortArg::Alpha, None);
    assert!(line.contains("sort=alpha"), "{line}");
    assert!(line.contains("[s]"), "{line}");
}

#[test]
fn view_status_line_names_the_active_filter_and_its_undo() {
    let filter = SessionFilter::visible("API");
    let line = view_status_line(SessionSortArg::Recent, Some(&filter));
    assert!(line.contains("filter='api'"), "{line}");
    assert!(line.contains("[/] clears"), "{line}");
}

#[test]
fn view_status_line_omits_the_filter_when_none_is_set() {
    let line = view_status_line(SessionSortArg::Recent, None);
    assert!(!line.contains("filter"), "{line}");
}
