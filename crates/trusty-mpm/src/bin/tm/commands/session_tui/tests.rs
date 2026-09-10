//! Unit coverage for the `tm ls` session TUI (#7224).
//!
//! Why: a TUI's failure modes are silent — a selection that jumps after a
//! refresh, a confirm step a stray key skips, a column set that wraps at 80
//! columns. Every one of those is decidable from a pure function or a
//! `TestBackend` frame, so none of them needs a terminal to catch.
//!
//! What: the width-driven column layout, the scroll window, the whole state
//! machine (navigation, the delete confirm and its self-session guard, the
//! shared rename validator), the delete-outcome wording, the key mapping, and
//! `TestBackend` renders at 80x24, 200x50, and 20x5.

use ratatui::{Terminal, backend::TestBackend};
use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::session_manager::rename::validate_session_name;

use super::layout::{self, Column};
use super::render;
use super::state::{Action, Input, Mode, Severity, TuiState, is_self_session};
use super::{delete_outcome, map_key};
use crate::commands::picker_delete::DeleteReport;

/// Minimal `ManagedSessionSummary` fixture.
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

/// A three-row fleet: one active, one stopped, one attached.
fn fleet() -> Vec<ManagedSessionSummary> {
    let mut a = session("tm-alpha-01", "active", 1);
    a.source_id = Some("bobmatnyc/trusty-tools".to_string());
    a.pane_id = Some("%11".to_string());
    let b = session("tm-bravo-02", "stopped", 2);
    let mut c = session("tm-charlie-03", "active", 3);
    c.attached = true;
    vec![a, b, c]
}

/// A state browsing `fleet()`, outside any managed pane.
fn browsing() -> TuiState {
    let mut state = TuiState::new(None, None);
    state.sync(&fleet());
    state
}

// ── layout: which columns fit (#7224 requirement 2) ─────────────────────────

#[test]
fn layout_columns_full_set_at_wide_width() {
    assert_eq!(
        layout::visible_columns(200),
        vec![
            Column::Name,
            Column::Id,
            Column::Project,
            Column::State,
            Column::Tmux
        ]
    );
}

#[test]
fn layout_columns_drops_tmux_then_id_at_eighty() {
    // 80 columns fits name + state + project but neither id nor tmux, and the
    // survivors still come back in DISPLAY order, not priority order.
    assert_eq!(
        layout::visible_columns(80),
        vec![Column::Name, Column::Project, Column::State]
    );
    // The full set costs exactly 92 (marker 2 + 28 + 8 + 24 + 18 + 8 + four
    // gutters); one column short of that drops only TMUX, the lowest priority.
    assert_eq!(
        layout::visible_columns(91),
        vec![Column::Name, Column::Id, Column::Project, Column::State]
    );
    assert_eq!(
        layout::visible_columns(92),
        vec![
            Column::Name,
            Column::Id,
            Column::Project,
            Column::State,
            Column::Tmux
        ],
        "92 is the exact width the full set fits in"
    );
}

#[test]
fn layout_columns_keeps_name_at_absurdly_narrow_width() {
    // A row with no name is not a row anyone can act on, so NAME survives even
    // when it cannot be drawn in full.
    for width in [0u16, 1, 5, 20] {
        assert_eq!(
            layout::visible_columns(width),
            vec![Column::Name],
            "width {width}"
        );
    }
}

// ── layout: cell contents ───────────────────────────────────────────────────

#[test]
fn layout_cell_shortens_the_id_without_an_ellipsis() {
    let s = session("tm-alpha-01", "active", 1);
    let cell = layout::cell(Column::Id, &s);
    assert_eq!(cell.chars().count(), layout::SHORT_ID_LEN);
    assert!(
        !cell.contains('…'),
        "a short id must stay pasteable: {cell}"
    );
    assert!(s.id.starts_with(&cell));
}

#[test]
fn layout_cell_truncates_a_long_name_with_an_ellipsis() {
    let s = session(&"x".repeat(80), "active", 1);
    let cell = layout::cell(Column::Name, &s);
    assert_eq!(cell.chars().count(), layout::width(Column::Name) as usize);
    assert!(cell.ends_with('…'), "expected an ellipsis, got {cell}");
}

#[test]
fn layout_cell_placeholders_absent_project_and_pane() {
    let s = session("tm-alpha-01", "active", 1);
    assert_eq!(layout::cell(Column::Project, &s), "-");
    assert_eq!(layout::cell(Column::Tmux, &s), "-");
}

#[test]
fn layout_cell_state_uses_the_shared_annotations() {
    let mut s = session("tm-alpha-01", "stopped", 1);
    s.unresumable = true;
    assert!(layout::state_cell(&s).contains("[dead]"));
    let mut attached = session("tm-charlie-03", "active", 3);
    attached.attached = true;
    assert_eq!(layout::state_cell(&attached), "attached");
}

// ── layout: the scroll window ───────────────────────────────────────────────

#[test]
fn window_keeps_selection_visible_scrolling_down() {
    // 20 rows, a 5-row viewport, selection at 12: the window must contain 12.
    let (start, len) = layout::visible_window(20, 12, 5, 0);
    assert_eq!(len, 5);
    assert!((start..start + len).contains(&12), "start {start}");
}

#[test]
fn window_holds_still_while_the_selection_stays_visible() {
    // Selection moves from 7 to 6 inside the window [5,9]; the window must not
    // scroll, or the whole table jitters on every arrow key.
    assert_eq!(layout::visible_window(20, 6, 5, 5), (5, 5));
}

#[test]
fn window_clamps_to_the_end_of_the_list() {
    // A stale offset past the end (the list shrank) must not index out of range.
    let (start, len) = layout::visible_window(6, 5, 5, 99);
    assert_eq!((start, len), (1, 5));
    assert!(start + len <= 6);
}

#[test]
fn window_zero_height_is_empty() {
    assert_eq!(layout::visible_window(20, 3, 0, 0), (0, 0));
    assert_eq!(layout::visible_window(0, 0, 10, 0), (0, 0));
}

// ── state: navigation ───────────────────────────────────────────────────────

#[test]
fn state_navigation_moves_and_clamps_at_both_ends() {
    let sessions = fleet();
    let mut state = browsing();
    assert_eq!(state.selected(), 0);
    assert_eq!(state.apply(Input::Down, &sessions), Action::Redraw);
    assert_eq!(state.selected(), 1);
    assert_eq!(state.apply(Input::End, &sessions), Action::Redraw);
    assert_eq!(state.selected(), 2);
    // Already at the last row: nothing changed, so no redraw is owed.
    assert_eq!(state.apply(Input::Down, &sessions), Action::Ignore);
    assert_eq!(state.apply(Input::Home, &sessions), Action::Redraw);
    assert_eq!(state.selected(), 0);
    assert_eq!(state.apply(Input::Up, &sessions), Action::Ignore);
}

#[test]
fn state_vi_keys_mirror_the_arrows() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Char('j'), &sessions);
    assert_eq!(state.selected(), 1);
    state.apply(Input::Char('k'), &sessions);
    assert_eq!(state.selected(), 0);
    state.apply(Input::Char('G'), &sessions);
    assert_eq!(state.selected(), 2);
    state.apply(Input::Char('g'), &sessions);
    assert_eq!(state.selected(), 0);
}

#[test]
fn state_quit_keys_all_leave() {
    let sessions = fleet();
    for input in [Input::Char('q'), Input::Escape, Input::Cancel] {
        let mut state = browsing();
        assert_eq!(state.apply(input, &sessions), Action::Quit, "{input:?}");
    }
}

#[test]
fn state_enter_opens_the_selected_row() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Down, &sessions);
    assert_eq!(state.apply(Input::Enter, &sessions), Action::Open(1));
}

#[test]
fn state_enter_on_an_empty_fleet_does_nothing() {
    let mut state = TuiState::new(None, None);
    state.sync(&[]);
    assert_eq!(state.apply(Input::Enter, &[]), Action::Ignore);
}

#[test]
fn state_refresh_key_asks_for_a_refetch() {
    let sessions = fleet();
    let mut state = browsing();
    assert_eq!(state.apply(Input::Char('R'), &sessions), Action::Refresh);
}

#[test]
fn state_help_opens_and_any_key_closes_it() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Char('?'), &sessions);
    assert_eq!(state.mode(), &Mode::Help);
    state.apply(Input::Char('x'), &sessions);
    assert_eq!(state.mode(), &Mode::Browse);
}

// ── state: selection survives a refresh (#7224) ─────────────────────────────

#[test]
fn state_sync_follows_the_pinned_session_across_a_reorder() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Down, &sessions); // pin tm-bravo-02
    let reordered = vec![
        sessions[2].clone(),
        sessions[1].clone(),
        sessions[0].clone(),
    ];
    state.sync(&reordered);
    assert_eq!(state.selected(), 1);
    assert_eq!(reordered[state.selected()].name, "tm-bravo-02");
}

#[test]
fn state_sync_clamps_when_the_pinned_session_is_gone() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::End, &sessions); // pin the third row
    let shrunk = vec![sessions[0].clone()];
    state.sync(&shrunk);
    assert_eq!(state.selected(), 0);
    // Re-pinned to what it landed on, so the next refresh follows THAT row.
    state.sync(&shrunk);
    assert_eq!(state.selected(), 0);
}

#[test]
fn state_sync_drops_an_overlay_whose_row_vanished() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::End, &sessions);
    state.apply(Input::Char('r'), &sessions);
    assert!(matches!(state.mode(), Mode::Rename { .. }));
    state.sync(&sessions[..1]);
    assert_eq!(state.mode(), &Mode::Browse);
}

// ── state: delete (#7224 requirement 4) ─────────────────────────────────────

#[test]
fn state_delete_refuses_the_attached_self_session() {
    let sessions = fleet();
    // This terminal is inside tm-alpha-01, the row the selection starts on.
    let mut state = TuiState::new(Some("tm-alpha-01-id".to_string()), None);
    state.sync(&sessions);
    assert_eq!(state.apply(Input::Char('d'), &sessions), Action::Redraw);
    assert_eq!(state.mode(), &Mode::Browse, "no confirm dialog may open");
    let (text, severity) = state.message().expect("a visible refusal");
    assert_eq!(severity, Severity::Error);
    assert!(
        text.contains("tm-alpha-01") && text.contains("attached"),
        "refusal must name the session and why: {text}"
    );
}

#[test]
fn state_delete_on_a_stopped_row_confirms_with_yes() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Down, &sessions); // tm-bravo-02, stopped
    assert_eq!(state.apply(Input::Char('d'), &sessions), Action::Redraw);
    assert_eq!(
        state.mode(),
        &Mode::Confirm {
            index: 1,
            force: false,
            stop_first: false,
            typed: String::new()
        }
    );
    state.apply(Input::Char('y'), &sessions);
    assert_eq!(
        state.apply(Input::Enter, &sessions),
        Action::Delete {
            index: 1,
            force: false,
            stop_first: false
        }
    );
}

/// An ERRORED row confirms with a plain `y`, and the action it yields carries
/// `stop_first` — the runtime-stop leg the daemon's refusal used to send the
/// operator out to a shell to run (#7224). `force` stays false: the delete that
/// follows the stop is still unforced, so the daemon's tmux probe remains the
/// authority on whether the record may go.
#[test]
fn state_delete_on_an_errored_row_asks_to_stop_and_delete() {
    let mut sessions = fleet();
    sessions[1].state = "errored".to_string();
    let mut state = browsing();
    state.sync(&sessions);
    state.apply(Input::Down, &sessions); // tm-bravo-02, errored
    assert_eq!(state.apply(Input::Char('d'), &sessions), Action::Redraw);
    assert_eq!(
        state.mode(),
        &Mode::Confirm {
            index: 1,
            force: false,
            stop_first: true,
            typed: String::new()
        },
        "an errored row must open a stop-and-delete confirm, not a plain one"
    );
    state.apply(Input::Char('y'), &sessions);
    assert_eq!(
        state.apply(Input::Enter, &sessions),
        Action::Delete {
            index: 1,
            force: false,
            stop_first: true
        }
    );
}

/// The stop-first leg is scoped to `errored`: a `stopped` row has no runtime to
/// stop, and a running one takes the force-confirm path instead — neither may
/// pick up a stop request it does not need (#7224).
#[test]
fn state_delete_sets_stop_first_only_for_errored() {
    let sessions = fleet();
    for (index, expect_stop) in [(0usize, false), (1, false)] {
        let mut state = browsing();
        state.apply(
            if index == 0 { Input::Home } else { Input::Down },
            &sessions,
        );
        state.apply(Input::Char('d'), &sessions);
        match state.mode() {
            Mode::Confirm { stop_first, .. } => {
                assert_eq!(*stop_first, expect_stop, "row {index}");
            }
            other => panic!("row {index} must open a confirm, got {other:?}"),
        }
    }
}

#[test]
fn state_delete_on_a_running_row_requires_the_force_word() {
    let sessions = fleet();
    let mut state = browsing(); // tm-alpha-01, active
    state.apply(Input::Char('d'), &sessions);
    assert!(matches!(state.mode(), Mode::Confirm { force: true, .. }));
    // `y` is not enough for a running session — the CLI's own rule.
    for c in "y".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    assert_eq!(state.apply(Input::Enter, &sessions), Action::Redraw);
    assert_eq!(state.mode(), &Mode::Browse);
    assert_eq!(
        state.message().expect("cancel notice").0,
        "delete cancelled"
    );

    let mut state = browsing();
    state.apply(Input::Char('d'), &sessions);
    for c in "force".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    assert_eq!(
        state.apply(Input::Enter, &sessions),
        Action::Delete {
            index: 0,
            force: true,
            stop_first: false
        }
    );
}

#[test]
fn state_confirm_backspace_and_escape_both_step_back() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Down, &sessions);
    state.apply(Input::Char('d'), &sessions);
    state.apply(Input::Char('y'), &sessions);
    state.apply(Input::Backspace, &sessions);
    // The `y` is gone, so Enter cancels rather than deleting.
    assert_eq!(state.apply(Input::Enter, &sessions), Action::Redraw);

    let mut state = browsing();
    state.apply(Input::Down, &sessions);
    state.apply(Input::Char('d'), &sessions);
    assert_eq!(state.apply(Input::Escape, &sessions), Action::Redraw);
    assert_eq!(state.mode(), &Mode::Browse);
}

#[test]
fn is_self_session_matches_by_env_id() {
    let s = session("tm-alpha-01", "active", 1);
    assert!(is_self_session(&s, Some("tm-alpha-01-id"), None));
    assert!(!is_self_session(&s, Some("someone-else"), None));
}

#[test]
fn is_self_session_matches_by_tmux_name() {
    // The env var can be lost to a nested shell; the tmux session name IS the
    // managed session's identity name, so it answers on its own.
    let s = session("tm-alpha-01", "active", 1);
    assert!(is_self_session(&s, None, Some("tm-alpha-01")));
    assert!(!is_self_session(&s, None, Some("tm-other-09")));
}

#[test]
fn is_self_session_false_outside_a_managed_pane() {
    let s = session("tm-alpha-01", "active", 1);
    assert!(!is_self_session(&s, None, None));
}

// ── state: rename (#7224 requirement 5) ─────────────────────────────────────

#[test]
fn state_rename_seeds_the_overlay_with_the_current_name() {
    let sessions = fleet();
    let mut state = browsing();
    assert_eq!(state.apply(Input::Char('r'), &sessions), Action::Redraw);
    assert_eq!(
        state.mode(),
        &Mode::Rename {
            index: 0,
            typed: "tm-alpha-01".to_string()
        }
    );
}

#[test]
fn state_rename_rejects_an_invalid_name_with_the_shared_validator() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Char('r'), &sessions);
    // Clear the seeded name, then type one the daemon would reject.
    for _ in 0.."tm-alpha-01".len() {
        state.apply(Input::Backspace, &sessions);
    }
    for c in "bad name!".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    assert_eq!(state.apply(Input::Enter, &sessions), Action::Redraw);
    // Still in the overlay, with the DAEMON'S own message — not a second
    // client-side spelling of the rule.
    assert!(matches!(state.mode(), Mode::Rename { .. }));
    let (text, severity) = state.message().expect("a validation error");
    assert_eq!(severity, Severity::Error);
    assert_eq!(
        text,
        validate_session_name("bad name!").expect_err("fixture must be invalid")
    );
}

#[test]
fn state_rename_sends_the_trimmed_validated_name() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Char('r'), &sessions);
    for _ in 0.."tm-alpha-01".len() {
        state.apply(Input::Backspace, &sessions);
    }
    for c in "tm-renamed ".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    assert_eq!(
        state.apply(Input::Enter, &sessions),
        Action::Rename {
            index: 0,
            // The trailing space the validator trims never reaches the daemon.
            name: "tm-renamed".to_string()
        }
    );
    assert_eq!(state.mode(), &Mode::Browse);
}

#[test]
fn state_rename_to_the_same_name_is_not_a_round_trip() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Char('r'), &sessions);
    assert_eq!(state.apply(Input::Enter, &sessions), Action::Redraw);
    assert_eq!(state.message().expect("a notice").0, "name unchanged");
}

#[test]
fn state_rename_escape_cancels() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Char('r'), &sessions);
    assert_eq!(state.apply(Input::Escape, &sessions), Action::Redraw);
    assert_eq!(state.mode(), &Mode::Browse);
    assert_eq!(state.message().expect("a notice").0, "rename cancelled");
}

#[test]
fn state_rename_overlay_treats_action_keys_as_text() {
    // `d`, `q` and `r` are action keys while browsing and literal characters
    // while typing a name; only the mode can tell them apart.
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Char('r'), &sessions);
    for _ in 0.."tm-alpha-01".len() {
        state.apply(Input::Backspace, &sessions);
    }
    for c in "dqr".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    assert_eq!(
        state.mode(),
        &Mode::Rename {
            index: 0,
            typed: "dqr".to_string()
        }
    );
}

// ── delete outcome wording ──────────────────────────────────────────────────

#[test]
fn delete_outcome_names_the_soft_delete() {
    let (text, severity) = delete_outcome(
        "tm-bravo-02",
        Ok(DeleteReport::Deleted {
            name: "tm-bravo-02".to_string(),
            prior_state: "stopped".to_string(),
            local: false,
        }),
    );
    assert_eq!(severity, Severity::Info);
    assert!(
        text.contains("--deleted--") && text.contains("still listed"),
        "a managed delete is soft and must say so: {text}"
    );
}

#[test]
fn delete_outcome_reports_the_local_store() {
    let (text, _) = delete_outcome(
        "local-01",
        Ok(DeleteReport::Deleted {
            name: "local-01".to_string(),
            prior_state: "stopped".to_string(),
            local: true,
        }),
    );
    assert!(text.contains("project store"), "{text}");
}

#[test]
fn delete_outcome_surfaces_a_refusal() {
    let (text, severity) = delete_outcome(
        "tm-alpha-01",
        Ok(DeleteReport::Refused("session is running\n".to_string())),
    );
    assert_eq!(severity, Severity::Error);
    assert_eq!(text, "delete refused: session is running");
}

/// A failed stop leg must read as "nothing was deleted", carrying the daemon's
/// own status and body — never as a delete that merely did not happen (#7224).
#[test]
fn delete_outcome_reports_a_failed_stop_as_not_deleted() {
    let (text, severity) = delete_outcome(
        "tm-bravo-02",
        Ok(DeleteReport::StopFailed(
            "stop returned HTTP 500 Internal Server Error: tmux driver unavailable".to_string(),
        )),
    );
    assert_eq!(severity, Severity::Error);
    assert!(text.contains("NOT deleted"), "{text}");
    assert!(text.contains("tmux driver unavailable"), "{text}");
    assert!(text.contains("500"), "{text}");
}

#[test]
fn delete_outcome_reports_not_found() {
    let (text, severity) = delete_outcome("gone-01", Ok(DeleteReport::NotFound));
    assert_eq!(severity, Severity::Info);
    assert!(text.contains("already gone"), "{text}");
}

#[test]
fn delete_outcome_reports_a_transport_error() {
    let (text, severity) =
        delete_outcome("tm-alpha-01", Err(anyhow::anyhow!("connection refused")));
    assert_eq!(severity, Severity::Error);
    assert!(text.contains("connection refused"), "{text}");
}

// ── key mapping ─────────────────────────────────────────────────────────────

#[test]
fn input_maps_ctrl_c_to_cancel() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for c in ['c', 'd'] {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(map_key(key), Some(Input::Cancel), "ctrl-{c}");
    }
}

#[test]
fn input_ignores_key_release() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let key = KeyEvent::new_with_kind_and_state(
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
        KeyEventState::NONE,
    );
    assert_eq!(map_key(key), None);
}

#[test]
fn input_passes_characters_through_unresolved() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
    assert_eq!(map_key(key), Some(Input::Char('d')));
    let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(map_key(key), Some(Input::Escape));
}

// ── TestBackend renders (#7224 requirement 2) ───────────────────────────────

/// Draw one frame at `w`x`h` and return the flattened screen text, one line
/// per terminal row.
fn draw(w: u16, h: u16, sessions: &[ManagedSessionSummary], state: &mut TuiState) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    terminal
        .draw(|frame| render::render(frame, sessions, state))
        .expect("render must not panic");
    let buffer = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn render_at_eighty_by_twentyfour_drops_the_widest_columns() {
    let sessions = fleet();
    let mut state = browsing();
    let screen = draw(80, 24, &sessions, &mut state);
    let joined = screen.join("\n");
    assert!(joined.contains("NAME"), "{joined}");
    assert!(joined.contains("PROJECT"), "{joined}");
    assert!(joined.contains("STATE"), "{joined}");
    // ID and TMUX do not fit at 80 columns and must be absent, not wrapped.
    assert!(!joined.contains("TMUX"), "{joined}");
    assert!(joined.contains("tm-alpha-01"), "{joined}");
    // Nothing may exceed the terminal width.
    for line in &screen {
        assert!(line.chars().count() <= 80, "overlong row: {line}");
    }
    // The footer key hints are always on screen.
    assert!(joined.contains("Enter open"), "{joined}");
    assert!(joined.contains("q quit"), "{joined}");
}

#[test]
fn render_at_two_hundred_by_fifty_shows_every_column() {
    let sessions = fleet();
    let mut state = browsing();
    let joined = draw(200, 50, &sessions, &mut state).join("\n");
    for header in ["NAME", "ID", "PROJECT", "STATE", "TMUX"] {
        assert!(joined.contains(header), "missing {header} in\n{joined}");
    }
    // Every row is drawn — a 50-row terminal has room for three sessions.
    for name in ["tm-alpha-01", "tm-bravo-02", "tm-charlie-03"] {
        assert!(joined.contains(name), "missing {name} in\n{joined}");
    }
    assert!(joined.contains("bobmatnyc/trusty-tools"), "{joined}");
    assert!(joined.contains("%11"), "{joined}");
}

#[test]
fn render_tiny_terminal_does_not_panic() {
    let sessions = fleet();
    // 20x5 and smaller, in every mode, including the overlays whose centered
    // popup is where a hand-rolled Rect would go out of bounds.
    for (w, h) in [(20u16, 5u16), (10, 3), (5, 2), (1, 1)] {
        for opener in ['?', 'd', 'r'] {
            let mut state = browsing();
            state.apply(Input::Char(opener), &sessions);
            let screen = draw(w, h, &sessions, &mut state);
            assert_eq!(screen.len(), h as usize, "{w}x{h} mode {opener}");
        }
    }
}

#[test]
fn render_empty_fleet_says_so() {
    let mut state = TuiState::new(None, None);
    state.sync(&[]);
    let joined = draw(80, 24, &[], &mut state).join("\n");
    assert!(joined.contains("no sessions"), "{joined}");
}

#[test]
fn render_scrolls_to_keep_the_selection_visible() {
    // 40 sessions in a 10-row terminal: the selected row must be on screen.
    let sessions: Vec<_> = (1..=40)
        .map(|i| session(&format!("tm-row-{i:02}"), "stopped", i))
        .collect();
    let mut state = TuiState::new(None, None);
    state.sync(&sessions);
    for _ in 0..30 {
        state.apply(Input::Down, &sessions);
    }
    let joined = draw(120, 10, &sessions, &mut state).join("\n");
    assert!(
        joined.contains("tm-row-31"),
        "selection off screen:\n{joined}"
    );
}

#[test]
fn render_confirm_overlay_names_the_word_it_wants() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Down, &sessions); // stopped row → plain confirm
    state.apply(Input::Char('d'), &sessions);
    let joined = draw(100, 24, &sessions, &mut state).join("\n");
    assert!(joined.contains("tm-bravo-02"), "{joined}");
    assert!(joined.contains("Type y"), "{joined}");

    let mut state = browsing(); // active row → force confirm
    state.apply(Input::Char('d'), &sessions);
    let joined = draw(100, 24, &sessions, &mut state).join("\n");
    assert!(joined.contains("force"), "{joined}");
}

#[test]
fn render_help_overlay_lists_every_action_key() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Char('?'), &sessions);
    let joined = draw(100, 30, &sessions, &mut state).join("\n");
    for token in ["rename", "delete", "refresh", "quit"] {
        assert!(joined.contains(token), "help omits {token}:\n{joined}");
    }
}

/// Every rendered row carries its session's name AND its state, on one line.
///
/// Why (#7224 requirement 1): the two cells an operator picks a row by are the
/// friendly name and the lifecycle state, and a table that draws them on
/// different lines — or drops the state column at a width that fits it — is
/// unusable for choosing what to resume. Asserting per ROW rather than
/// per-frame is what makes this fail if the columns ever come apart.
#[test]
fn render_rows_pair_each_name_with_its_state() {
    let sessions = fleet();
    let mut state = browsing();
    let lines = draw(140, 24, &sessions, &mut state);
    // `attached` outranks the raw state word — see `layout::state_cell`.
    for (name, expected_state) in [
        ("tm-alpha-01", "active"),
        ("tm-bravo-02", "stopped"),
        ("tm-charlie-03", "attached"),
    ] {
        let row = lines
            .iter()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("no row for {name} in\n{}", lines.join("\n")));
        assert!(
            row.contains(expected_state),
            "row for {name} omits state {expected_state}: {row:?}"
        );
    }
}

/// A rename is visible in the list, and the selection follows the renamed row.
///
/// Why (#7224 requirement 5): the rename is only done when the operator can see
/// it. The overlay yields the validated name, the daemon applies it, and the
/// re-fetch that follows replaces the whole `Vec` — so this drives that exact
/// sequence and asserts the new name is on screen, the old one is gone, and
/// `sync` re-pointed the selection at the row by ID rather than by index.
#[test]
fn render_shows_a_rename_after_the_refetch() {
    let mut sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Down, &sessions); // select tm-bravo-02
    state.apply(Input::Char('r'), &sessions);
    for _ in 0..11 {
        state.apply(Input::Backspace, &sessions); // clear the seeded name
    }
    for c in "tm-renamed-02".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    let action = state.apply(Input::Enter, &sessions);
    let Action::Rename { index, name } = action else {
        panic!("expected a rename action, got {action:?}");
    };
    assert_eq!(name, "tm-renamed-02");

    // What the daemon does, then the re-fetch the driver performs after it.
    sessions[index].name = name.clone();
    let refetched: Vec<_> = sessions.iter().rev().cloned().collect(); // order churns
    state.sync(&refetched);

    let joined = draw(140, 24, &refetched, &mut state).join("\n");
    assert!(
        joined.contains("tm-renamed-02"),
        "rename not shown:\n{joined}"
    );
    assert!(
        !joined.contains("tm-bravo-02"),
        "old name still shown:\n{joined}"
    );
    assert_eq!(
        refetched[state.selected()].name,
        "tm-renamed-02",
        "selection must follow the renamed session across the re-fetch"
    );
}

// ── status-line wrapping: never cut a token in half (#7224) ─────────────────

/// The exact refusal the daemon's delete guard produces, for a session whose
/// friendly name and UUID are both realistic. This is the string the owner saw
/// cut off mid-UUID in the picker.
const GUARD_REFUSAL: &str = "delete refused: session 'tm-quiet-falcon' is errored — stop it first \
     with `tm session stop` (or `tm session decommission`), or pass --force to delete the record \
     anyway. Session id:\n8fc5db8f-b0a5-5c47-9a5b-59cd2a1e4f77";

/// The UUID in the guard refusal, which no wrap may split.
const GUARD_UUID: &str = "8fc5db8f-b0a5-5c47-9a5b-59cd2a1e4f77";

#[test]
fn wrap_message_keeps_a_uuid_whole_at_eighty_columns() {
    let lines = layout::wrap_message(GUARD_REFUSAL, 80, layout::STATUS_MAX_LINES);
    for line in &lines {
        assert!(
            line.chars().count() <= 80,
            "wrapped line exceeds the width: {line}"
        );
    }
    assert!(
        lines.iter().any(|l| l.contains(GUARD_UUID)),
        "the full session id must survive the wrap, got:\n{lines:#?}"
    );
}

#[test]
fn wrap_message_honours_explicit_newlines() {
    let lines = layout::wrap_message("first line\nsecond line", 80, 4);
    assert_eq!(lines, vec!["first line", "second line"]);
}

#[test]
fn wrap_message_elides_on_a_token_boundary() {
    // Ten five-character tokens at width 12 need five lines; capped at two, the
    // second must end in an ellipsis and never inside a token.
    let text = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh iiiii jjjjj";
    let lines = layout::wrap_message(text, 12, 2);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].ends_with('…'), "{lines:#?}");
    for line in &lines {
        assert!(line.chars().count() <= 12, "{line}");
        for token in line.trim_end_matches('…').split_whitespace() {
            assert_eq!(token.chars().count(), 5, "token cut mid-word: {token}");
        }
    }
}

#[test]
fn wrap_message_zero_width_is_empty() {
    assert!(layout::wrap_message("anything", 0, 4).is_empty());
    assert!(layout::wrap_message("anything", 80, 0).is_empty());
}

/// The regression the owner reported: at 80 columns the picker's status line
/// showed the delete refusal cut off inside the session UUID. The full id must
/// now appear somewhere on screen, unbroken (#7224).
#[test]
fn render_wraps_a_long_refusal_without_cutting_the_uuid() {
    let sessions = fleet();
    let mut state = browsing();
    state.set_message(GUARD_REFUSAL, Severity::Error);
    let screen = draw(80, 24, &sessions, &mut state);
    let joined = screen.join("\n");
    assert!(
        joined.contains(GUARD_UUID),
        "the session id was cut by the status line:\n{joined}"
    );
    for line in &screen {
        assert!(line.chars().count() <= 80, "overlong row: {line}");
    }
    // The table and the key footer must survive the taller status region.
    assert!(joined.contains("NAME"), "{joined}");
    assert!(joined.contains("q quit"), "{joined}");
}

#[test]
fn render_status_uses_one_row_when_there_is_no_message() {
    let sessions = fleet();
    let mut state = browsing();
    let screen = draw(80, 24, &sessions, &mut state);
    // Three sessions plus a header, a title, an empty status row and a footer —
    // the layout must not have grown a status region with nothing in it.
    assert_eq!(screen.len(), 24);
    assert!(screen.last().is_some_and(|l| l.contains("q quit")));
}
