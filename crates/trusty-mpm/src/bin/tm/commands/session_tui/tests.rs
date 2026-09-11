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
use trusty_mpm::project::Project;
use trusty_mpm::session_manager::rename::validate_session_name;

use super::layout::{self, Column};
use super::new_session::{
    self, NewProject, NewSessionFlow, NewSessionRequest, ProjectIdentity, Step, Target,
};
use super::render;
use super::state::{Action, Input, Mode, Severity, TuiState, is_self_session};
use super::{delete_outcome, map_key};
use crate::commands::picker_delete::DeleteReport;
use crate::commands::tmux_attach::AttachOutcome;

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

/// The TUI overlay renders the SHARED errored question (#7224), so an operator
/// who learns what `y` does in the numbered fallback picker reads the same
/// promise here.
#[test]
fn confirm_overlay_asks_the_shared_errored_question() {
    let sessions = vec![session("tm-quiet-falcon", "errored", 4)];
    let mut state = TuiState::new(None, None);
    state.sync(&sessions);
    state.apply(Input::Char('d'), &sessions);
    let joined = draw(200, 50, &sessions, &mut state).join("\n");
    let ask = crate::commands::picker_delete::errored_confirm_ask("tm-quiet-falcon");
    assert!(joined.contains(&ask), "expected {ask:?} in\n{joined}");
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

/// #7224: the session id is the point of the refusal, so it has to survive the
/// wrap at the narrow widths a split pane actually has. Packing greedily from
/// the front filled all four lines with the first paragraph at 40 and 60
/// columns and dropped the id paragraph entirely.
#[test]
fn wrap_message_keeps_the_session_id_at_narrow_widths() {
    for width in [40usize, 60, 80] {
        let lines = layout::wrap_message(GUARD_REFUSAL, width, layout::STATUS_MAX_LINES);
        assert!(
            lines.len() <= layout::STATUS_MAX_LINES,
            "width {width} overflowed the status region:\n{lines:#?}"
        );
        for line in &lines {
            assert!(
                line.chars().count() <= width,
                "width {width}: line exceeds it: {line}"
            );
        }
        assert!(
            lines.iter().any(|l| l.contains(GUARD_UUID)),
            "width {width}: the full session id must survive, got:\n{lines:#?}"
        );
    }
}

/// A token longer than the width has no wrap that shows it whole, so it is
/// emitted alone and intact — splitting it is the exact failure the wrap exists
/// to prevent, and the elide helper must leave a line it cannot cut at a
/// whitespace boundary exactly as it is.
#[test]
fn wrap_message_keeps_an_overlong_token_whole() {
    let token = "z".repeat(200);
    let lines = layout::wrap_message(&token, 40, layout::STATUS_MAX_LINES);
    assert_eq!(lines, vec![token]);
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

// ── new-session flow (#7395) ────────────────────────────────────────────────

/// A registry row fixture. Built through serde so every optional field takes
/// the same default the daemon's own listing gives it.
fn project(name: &str, repo_url: &str) -> Project {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "repo_url": repo_url,
        "default_branch": "main",
    }))
    .expect("project fixture")
}

/// Two registered projects, plus the typed-path escape `targets_from` appends.
fn targets() -> Vec<Target> {
    new_session::targets_from(
        &[
            project("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
            project("apex", "https://github.com/duetto/apex"),
        ],
        &[],
    )
}

/// Stand-in for the real git resolution, so the unregistered-path branch is
/// decidable without a checkout on disk.
fn stub_identity(typed: &str) -> Option<ProjectIdentity> {
    match typed {
        "/w/widgets" => Some(ProjectIdentity {
            name: "widgets".to_string(),
            repo_url: "https://github.com/acme/widgets".to_string(),
            root: "/w/widgets".to_string(),
        }),
        // The SAME project the registry already holds, reached by its checkout.
        "/w/trusty-tools" => Some(ProjectIdentity {
            name: "trusty-tools".to_string(),
            repo_url: "https://github.com/bobmatnyc/trusty-tools".to_string(),
            root: "/w/trusty-tools".to_string(),
        }),
        _ => None,
    }
}

/// A state browsing `fleet()` with the new-session flow already open.
fn creating() -> TuiState {
    let mut state = browsing();
    state.open_new_session(NewSessionFlow::with_resolver(targets(), stub_identity));
    state
}

#[test]
fn new_session_targets_from_puts_the_path_escape_last() {
    let targets = targets();
    assert_eq!(
        targets.first(),
        Some(&Target::Registered {
            name: "trusty-tools".to_string(),
            repo: "https://github.com/bobmatnyc/trusty-tools".to_string(),
        })
    );
    assert_eq!(targets.last(), Some(&Target::Other));
    // An empty registry still offers the escape, or a fresh host could never
    // create anything from this surface.
    assert_eq!(new_session::targets_from(&[], &[]), vec![Target::Other]);
}

#[test]
fn new_session_key_opens_the_flow() {
    let sessions = fleet();
    let mut state = browsing();
    // `n` only ASKS — the registry read belongs to the driver, so the state
    // machine never issues one itself.
    assert_eq!(state.apply(Input::Char('n'), &sessions), Action::NewSession);
    assert_eq!(state.mode(), &Mode::Browse, "the flow opens on the answer");
    state.open_new_session(NewSessionFlow::with_resolver(targets(), stub_identity));
    assert!(matches!(state.mode(), Mode::New(_)));
}

/// Confirming a registered row creates in THAT project and registers nothing.
///
/// Why (#7395 requirement 3): this is the acceptance case for the
/// existing-project half — the create call has to receive the repo of the row
/// the operator moved onto, not the first row and not the cwd. Driving it from
/// the keystrokes, then through [`new_session::perform_with`] with fakes, is
/// what makes an implementation that creates something else fail here.
#[tokio::test]
async fn new_session_confirming_a_registered_project_creates_without_registering() {
    let sessions = fleet();
    let mut state = creating();
    // Move onto the SECOND registered project before confirming.
    assert_eq!(state.apply(Input::Down, &sessions), Action::Redraw);
    let action = state.apply(Input::Enter, &sessions);
    let Action::Create(request) = action else {
        panic!("expected a create action, got {action:?}");
    };
    assert_eq!(
        request,
        NewSessionRequest {
            register: None,
            repo: "https://github.com/duetto/apex".to_string(),
            label: "apex".to_string(),
        }
    );
    assert_eq!(state.mode(), &Mode::Browse, "the overlay closes on confirm");

    let calls = std::cell::RefCell::new(Vec::new());
    let outcome = new_session::perform_with(
        request,
        |p: NewProject| {
            calls.borrow_mut().push(format!("register:{}", p.name));
            async { anyhow::Ok(()) }
        },
        |repo: String| {
            calls.borrow_mut().push(format!("create:{repo}"));
            async { anyhow::Ok(AttachOutcome::Attached) }
        },
    )
    .await
    .expect("the create leg must succeed");
    assert_eq!(outcome, AttachOutcome::Attached);
    assert_eq!(
        calls.into_inner(),
        vec!["create:https://github.com/duetto/apex".to_string()],
        "a registered project must not be registered again"
    );
}

/// An unregistered path is registered FIRST, then created in.
///
/// Why (#7395 requirement 2): the daemon cannot spawn into a project it has
/// never heard of, so the ORDER is the requirement — asserting only that both
/// calls happened would pass for an implementation that registers afterwards.
#[tokio::test]
async fn new_session_unregistered_path_registers_before_creating() {
    let sessions = fleet();
    let mut state = creating();
    // Walk to the "other" row; two registered projects precede it.
    state.apply(Input::Down, &sessions);
    state.apply(Input::Down, &sessions);
    assert_eq!(
        state.apply(Input::Enter, &sessions),
        Action::Redraw,
        "the path entry opens rather than creating"
    );
    for c in "/w/widgets".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    let action = state.apply(Input::Enter, &sessions);
    let Action::Create(request) = action else {
        panic!("expected a create action, got {action:?}");
    };
    assert_eq!(
        request,
        NewSessionRequest {
            register: Some(NewProject {
                name: "widgets".to_string(),
                repo_url: "https://github.com/acme/widgets".to_string(),
            }),
            // The local working-tree root, not the URL — that is what lets the
            // daemon resolve `source_id` for a checkout already on disk.
            repo: "/w/widgets".to_string(),
            label: "widgets".to_string(),
        }
    );

    let calls = std::cell::RefCell::new(Vec::new());
    new_session::perform_with(
        request,
        |p: NewProject| {
            calls.borrow_mut().push(format!("register:{}", p.repo_url));
            async { anyhow::Ok(()) }
        },
        |repo: String| {
            calls.borrow_mut().push(format!("create:{repo}"));
            async { anyhow::Ok(AttachOutcome::Attached) }
        },
    )
    .await
    .expect("both legs must succeed");
    assert_eq!(
        calls.into_inner(),
        vec![
            "register:https://github.com/acme/widgets".to_string(),
            "create:/w/widgets".to_string(),
        ],
        "registration must land before the session is created"
    );
}

#[tokio::test]
async fn new_session_a_failed_registration_never_creates() {
    let request = NewSessionRequest {
        register: Some(NewProject {
            name: "widgets".to_string(),
            repo_url: "https://github.com/acme/widgets".to_string(),
        }),
        repo: "/w/widgets".to_string(),
        label: "widgets".to_string(),
    };
    let created = std::cell::Cell::new(false);
    let err = new_session::perform_with(
        request,
        |_: NewProject| async { Err(anyhow::anyhow!("registry refused it")) },
        |_: String| {
            created.set(true);
            async { anyhow::Ok(AttachOutcome::Attached) }
        },
    )
    .await
    .expect_err("a failed registration must abort the flow");
    assert!(
        !created.get(),
        "nothing may be created after a failed register"
    );
    assert!(
        format!("{err:#}").contains("registry refused it"),
        "the daemon's own reason must survive: {err:#}"
    );
}

#[test]
fn new_session_known_path_skips_registration() {
    // The typed checkout resolves to a project the registry ALREADY holds, so
    // re-registering would replace its stored record with these two fields.
    let request = new_session::request_for_path("/w/trusty-tools", &targets(), stub_identity)
        .expect("a known checkout is usable");
    assert_eq!(request.register, None);
    assert_eq!(request.repo, "/w/trusty-tools");
}

#[test]
fn new_session_rejects_a_path_that_is_not_a_checkout() {
    let sessions = fleet();
    let mut state = creating();
    state.apply(Input::Down, &sessions);
    state.apply(Input::Down, &sessions);
    state.apply(Input::Enter, &sessions);
    for c in "/tmp/nope".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    assert_eq!(state.apply(Input::Enter, &sessions), Action::Redraw);
    let (text, severity) = state.message().expect("a refusal must be shown");
    assert!(text.contains("/tmp/nope"), "{text}");
    assert_eq!(severity, Severity::Error);
    // The entry stays open with what was typed, so it can be corrected.
    match state.mode() {
        Mode::New(flow) => assert_eq!(flow.typed(), Some("/tmp/nope")),
        other => panic!("the flow must stay open, got {other:?}"),
    }
}

#[test]
fn new_session_rejects_an_empty_path() {
    let err = new_session::request_for_path("   ", &targets(), stub_identity)
        .expect_err("an empty path is not a project");
    assert!(err.contains("path"), "{err}");
}

#[test]
fn new_session_escape_cancels_from_both_steps() {
    let sessions = fleet();
    // From the project list.
    let mut state = creating();
    assert_eq!(state.apply(Input::Escape, &sessions), Action::Redraw);
    assert_eq!(state.mode(), &Mode::Browse);
    // From the path entry.
    let mut state = creating();
    state.apply(Input::Down, &sessions);
    state.apply(Input::Down, &sessions);
    state.apply(Input::Enter, &sessions);
    state.apply(Input::Char('/'), &sessions);
    assert_eq!(state.apply(Input::Escape, &sessions), Action::Redraw);
    assert_eq!(state.mode(), &Mode::Browse);
}

/// Esc out of the flow leaves every observable part of the surface as it was.
///
/// Why (#7395 requirement 4): "returns to the list unchanged" is a claim about
/// the selection, the scroll offset and the status line, not just the mode — a
/// flow that reset the pinned row on the way out would still pass a mode-only
/// assertion.
#[test]
fn new_session_escape_leaves_the_state_identical() {
    let sessions = fleet();
    let mut state = browsing();
    state.apply(Input::Down, &sessions);
    let before = (
        state.mode().clone(),
        state.selected(),
        state.scroll(),
        state.message().map(|(t, s)| (t.to_string(), s)),
    );

    assert_eq!(state.apply(Input::Char('n'), &sessions), Action::NewSession);
    state.open_new_session(NewSessionFlow::with_resolver(targets(), stub_identity));
    state.apply(Input::Down, &sessions);
    state.apply(Input::Down, &sessions);
    state.apply(Input::Enter, &sessions);
    state.apply(Input::Char('/'), &sessions);
    state.apply(Input::Char('w'), &sessions);
    state.apply(Input::Escape, &sessions);

    let after = (
        state.mode().clone(),
        state.selected(),
        state.scroll(),
        state.message().map(|(t, s)| (t.to_string(), s)),
    );
    assert_eq!(after, before);
}

#[test]
fn new_session_rows_window_keeps_the_selection_visible() {
    // #7421: zero-padded so the alphabetical order the picker now applies is
    // also the numeric one, and the window arithmetic stays readable.
    let many: Vec<Project> = (0..12)
        .map(|i| {
            project(
                &format!("p{i:02}"),
                &format!("https://example.test/p{i:02}"),
            )
        })
        .collect();
    let mut flow =
        NewSessionFlow::with_resolver(new_session::targets_from(&many, &[]), stub_identity);
    // Eight rows is the whole window, so a short registry never scrolls.
    assert_eq!(flow.rows().len(), 8);
    assert!(flow.rows()[0].starts_with('▸'));
    for _ in 0..12 {
        flow.apply(Input::Down);
    }
    // Twelve projects plus the escape row: twelve `Down`s land on the escape.
    let rows = flow.rows();
    assert_eq!(rows.len(), 8);
    assert!(
        rows.last()
            .is_some_and(|r| r.starts_with('▸') && r.contains("other")),
        "the selection scrolled off the window: {rows:?}"
    );
    assert!(
        rows[0].contains("p05"),
        "the window did not follow the selection down: {rows:?}"
    );
}

#[test]
fn render_new_session_overlay_lists_the_registered_projects() {
    let sessions = fleet();
    let mut state = creating();
    let joined = draw(110, 30, &sessions, &mut state).join("\n");
    assert!(joined.contains("Start a new session in:"), "{joined}");
    // #7406: the row is the project's identity, not its stored URL.
    assert!(joined.contains("duetto/apex"), "{joined}");
    assert!(!joined.contains("https://"), "{joined}");
    assert!(joined.contains("other — type a project path"), "{joined}");
    assert!(joined.contains("Esc cancels"), "{joined}");
}

#[test]
fn render_new_session_overlay_shows_the_typed_path() {
    let sessions = fleet();
    let mut state = creating();
    state.apply(Input::Down, &sessions);
    state.apply(Input::Down, &sessions);
    state.apply(Input::Enter, &sessions);
    for c in "/w/widgets".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    let joined = draw(110, 30, &sessions, &mut state).join("\n");
    assert!(joined.contains("/w/widgets"), "{joined}");
    assert!(joined.contains("registers it"), "{joined}");
}

/// The footer advertises the create key at the narrowest supported width.
///
/// Why (#7395 requirement 1): a key nobody can discover is not a feature, and
/// the footer is the only place it is visible without opening `?`. Asserting at
/// 80 columns is what catches a legend that has grown past the width it has to
/// fit in.
#[test]
fn render_footer_offers_the_new_session_key() {
    let sessions = fleet();
    let mut state = browsing();
    let screen = draw(80, 24, &sessions, &mut state);
    let footer = screen.last().expect("a footer row");
    assert!(footer.contains("n new"), "{footer}");
    assert!(
        footer.contains("q quit"),
        "the legend was clipped: {footer}"
    );
    // And the `?` reference says what the key does.
    state.apply(Input::Char('?'), &sessions);
    let joined = draw(110, 30, &sessions, &mut state).join("\n");
    assert!(joined.contains("new session"), "{joined}");
}

// ── new-session picker polish (#7406) ───────────────────────────────────────

/// The overlay's own rows, unwrapped from the border columns around them.
fn overlay_rows(screen: &[String]) -> Vec<String> {
    screen
        .iter()
        .filter_map(|line| {
            let mut parts = line.split('│');
            parts.next()?;
            Some(parts.next()?.trim_end().to_string())
        })
        .collect()
}

/// A registered row built straight, bypassing the offerable filter.
fn target(name: &str, repo: &str) -> Target {
    Target::Registered {
        name: name.to_string(),
        repo: repo.to_string(),
    }
}

/// Three registered rows, in the alphabetical order #7421 renders them:
/// `acme/widgets`, `bobmatnyc/trusty-tools`, `duetto/apex`.
fn three_projects() -> Vec<Target> {
    new_session::targets_from(
        &[
            project("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
            project("apex", "https://github.com/duetto/apex"),
            project("widgets", "https://github.com/acme/widgets"),
        ],
        &[],
    )
}

/// The overlay opens on the project of the row the cursor was on.
///
/// Why (#7406 requirement 1): opening on row 0 made the operator scroll past
/// the project they were already looking at on every create. The fixture puts
/// that project THIRD, so an implementation that keeps the old behaviour — or
/// that matches on the first row by accident — fails here.
#[test]
fn new_session_preselects_the_cursor_sessions_project() {
    let targets = three_projects();
    // The session spells the remote differently from the registry row; the
    // shared `repo_url_matches` is what makes the two agree.
    let mut cursor = session("a1", "Active", 1);
    cursor.repo_url = Some("git@github.com:duetto/apex.git".to_string());
    assert_eq!(new_session::preselect_index(&targets, Some(&cursor)), 2);

    let flow = NewSessionFlow::with_resolver(targets.clone(), stub_identity)
        .preselected_for(Some(&cursor));
    let rows = flow.rows();
    assert_eq!(rows[2], "▸ duetto/apex", "{rows:?}");
    assert!(!rows[0].starts_with('▸'), "{rows:?}");

    // A session with no `repo_url` is matched on its `owner/repo` source id.
    let mut by_source = session("t1", "Active", 2);
    by_source.source_id = Some("bobmatnyc/trusty-tools".to_string());
    assert_eq!(new_session::preselect_index(&targets, Some(&by_source)), 1);
}

#[test]
fn new_session_preselect_falls_back_to_the_first_row() {
    let targets = three_projects();
    // No cursor session at all.
    assert_eq!(new_session::preselect_index(&targets, None), 0);
    // A cursor session whose project is not registered.
    let mut stranger = session("s1", "Active", 1);
    stranger.repo_url = Some("https://github.com/other/thing".to_string());
    assert_eq!(new_session::preselect_index(&targets, Some(&stranger)), 0);
    // A cursor session that names no project at all.
    let bare = session("b1", "Active", 1);
    assert_eq!(new_session::preselect_index(&targets, Some(&bare)), 0);
}

/// Every row is `owner/repo` or `<basename> (local)` — never a URL or a path.
#[test]
fn new_session_rows_show_owner_repo_only() {
    let flow = NewSessionFlow::with_resolver(
        vec![
            target(
                "trusty-tools",
                "https://github.com/bobmatnyc/trusty-tools.git",
            ),
            target("widgets", "/Users/me/code/widgets"),
            target("nameless", ""),
            Target::Other,
        ],
        stub_identity,
    );
    assert_eq!(
        flow.rows(),
        vec![
            "▸ bobmatnyc/trusty-tools".to_string(),
            "  widgets (local)".to_string(),
            "  nameless (local)".to_string(),
            "  other — type a project path…".to_string(),
        ]
    );
}

/// The host appears only when two rows would otherwise read identically.
#[test]
fn new_session_labels_disambiguate_by_host() {
    let labels = new_session::row_labels(&[
        target("apex", "https://github.com/duetto/apex"),
        target("apex-gl", "https://gitlab.com/duetto/apex"),
        target("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
    ]);
    assert_eq!(
        labels,
        vec![
            "github.com/duetto/apex".to_string(),
            "gitlab.com/duetto/apex".to_string(),
            // Unique on its own, so it keeps the short form.
            "bobmatnyc/trusty-tools".to_string(),
        ]
    );
}

/// A registration nobody can work in never reaches the picker.
///
/// Why (#7406 requirement 3): the owner's screen listed a probe registered at a
/// `/private/tmp/…/scratchpad/…` path. The fixture carries that row, a row
/// whose path is gone, and two rows that must survive.
#[test]
fn new_session_targets_from_drops_a_scratchpad_registration() {
    let targets = new_session::targets_from(
        &[
            project(
                "mcp-probe-scratch-4181",
                "/private/tmp/claude-502/abc/scratchpad/mcp-probe-scratch-4181",
            ),
            project("gone", "/nonexistent/7406/no-such-checkout"),
            project("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
            project("apex", "https://github.com/duetto/apex"),
        ],
        &[],
    );
    assert_eq!(
        targets,
        vec![
            target("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
            target("apex", "https://github.com/duetto/apex"),
            Target::Other,
        ]
    );
}

/// At 80 columns the rows are short, exact, and never wrap.
///
/// Why (#7406 requirement 2): the owner's screen wrapped a path onto a second
/// line, which is what made the list unreadable. Asserting the EXACT row text
/// at the narrowest supported width is what catches a row that grows back.
#[test]
fn render_new_session_rows_at_eighty_columns() {
    let sessions = fleet();
    let mut state = browsing();
    state.open_new_session(NewSessionFlow::with_resolver(
        vec![
            target("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
            target("widgets", "/Users/me/code/widgets"),
            Target::Other,
        ],
        stub_identity,
    ));
    let screen = draw(80, 24, &sessions, &mut state);
    let rows = overlay_rows(&screen);
    // The popup is 80% of 80 columns; its inner width is what a row may use.
    for row in &rows {
        assert!(
            row.chars().count() <= 62,
            "row over the pane width: {row:?}"
        );
    }
    assert!(
        rows.contains(&"▸ bobmatnyc/trusty-tools".to_string()),
        "{rows:?}"
    );
    assert!(rows.contains(&"  widgets (local)".to_string()), "{rows:?}");
    let joined = screen.join("\n");
    assert!(
        !joined.contains("https://"),
        "a full URL reached a row: {joined}"
    );
    assert!(
        !joined.contains("/Users/me/code"),
        "an absolute path reached a row: {joined}"
    );
}

// ── new-session picker order, position and filter (#7421) ───────────────────

/// Six registrations, spelled in one order; the caller may reshuffle them.
fn six_projects() -> Vec<Project> {
    vec![
        project("apex", "https://github.com/duetto/apex"),
        project("widgets", "https://github.com/acme/widgets"),
        project("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
        project("zeta", "https://github.com/zed/zeta"),
        project("mid", "https://github.com/mid/mid"),
        project("early", "https://github.com/early/early"),
    ]
}

/// The same registry contents render the same rows, whatever order they arrive
/// in.
///
/// Why (#7421 acceptance 1 and 5): the registry is a `HashMap`, so
/// `registry_list_projects` hands back `values()` in an order that varies run to
/// run. Against the pre-#7421 `targets_from` — which mapped the input straight
/// through — a permuted input produced a permuted row list, so this fails there.
#[test]
fn new_session_order_is_independent_of_registry_iteration_order() {
    let forward = new_session::targets_from(&six_projects(), &[]);
    let mut shuffled = six_projects();
    shuffled.reverse();
    shuffled.swap(0, 3);
    let reversed = new_session::targets_from(&shuffled, &[]);
    assert_eq!(forward, reversed, "the row order followed the input order");
    // And the order is the alphabetical one the labels spell, escape hatch last.
    assert_eq!(
        new_session::row_labels(&forward),
        vec![
            "acme/widgets".to_string(),
            "bobmatnyc/trusty-tools".to_string(),
            "duetto/apex".to_string(),
            "early/early".to_string(),
            "mid/mid".to_string(),
            "zed/zeta".to_string(),
            "other — type a project path…".to_string(),
        ]
    );
}

/// A project with a live session outranks every project without one.
///
/// Why (#7421 acceptance 1 and 5): the owner's four projects with running
/// sessions were the ones scrolling out of the eight-row window. `zed/zeta`
/// sorts LAST alphabetically, so an implementation that only sorts by label —
/// the pre-#7421 behaviour plus a plain sort — fails here.
#[test]
fn new_session_order_puts_a_live_session_project_first() {
    let mut live = session("z1", "active", 1);
    live.repo_url = Some("git@github.com:zed/zeta.git".to_string());
    live.last_activity_at = Some("2026-09-11T10:00:00Z".to_string());
    // A second session, older and merely stopped, in a project that would
    // otherwise sort first.
    let mut older = session("w1", "stopped", 2);
    older.source_id = Some("acme/widgets".to_string());
    older.last_activity_at = Some("2026-09-01T10:00:00Z".to_string());

    let rows = new_session::targets_from(&six_projects(), &[live, older]);
    let labels = new_session::row_labels(&rows);
    assert_eq!(
        labels.first().map(String::as_str),
        Some("zed/zeta"),
        "the live-session project did not sort first: {labels:?}"
    );
    assert_eq!(
        labels.get(1).map(String::as_str),
        Some("acme/widgets"),
        "the stopped-session project did not outrank the session-less ones: {labels:?}"
    );
    assert_eq!(
        labels.get(2).map(String::as_str),
        Some("bobmatnyc/trusty-tools"),
        "the session-less rows lost their alphabetical order: {labels:?}"
    );
}

/// Typing narrows the rows; Esc clears the filter before it cancels the flow.
///
/// Why (#7421 acceptance 3): a twenty-project registry is unusable by arrow key
/// alone, and an Esc that threw the whole flow away on a mistyped filter would
/// make typing more expensive than scrolling.
#[test]
fn new_session_typing_filters_the_rows() {
    let mut flow = NewSessionFlow::with_resolver(
        new_session::targets_from(&six_projects(), &[]),
        stub_identity,
    );
    assert_eq!(flow.rows().len(), 7);

    assert_eq!(flow.apply(Input::Char('z')), Step::Redraw);
    assert_eq!(flow.filter(), "z");
    // The matching row, plus the typed-path escape, which never filters away.
    assert_eq!(
        flow.rows(),
        vec![
            "▸ zed/zeta".to_string(),
            "  other — type a project path…".to_string(),
        ],
        "the filter did not narrow the rows"
    );

    // Matching is case-insensitive and runs over the whole label.
    assert_eq!(flow.apply(Input::Backspace), Step::Redraw);
    assert_eq!(flow.apply(Input::Char('A')), Step::Redraw);
    assert_eq!(flow.apply(Input::Char('C')), Step::Redraw);
    assert_eq!(flow.rows().len(), 2, "{:?}", flow.rows());
    assert!(flow.rows()[0].contains("acme/widgets"), "{:?}", flow.rows());

    // Esc clears the filter; a second Esc, with nothing left to clear, cancels.
    assert_eq!(flow.apply(Input::Escape), Step::Redraw);
    assert_eq!(flow.filter(), "");
    assert_eq!(flow.rows().len(), 7);
    assert_eq!(flow.apply(Input::Escape), Step::Cancel);
}

/// A filter that hides the highlighted row re-seats it on the first match.
#[test]
fn new_session_filter_keeps_the_selection_valid() {
    let mut flow = NewSessionFlow::with_resolver(
        new_session::targets_from(&six_projects(), &[]),
        stub_identity,
    );
    // Move onto `bobmatnyc/trusty-tools`, then filter it away.
    assert_eq!(flow.apply(Input::Down), Step::Redraw);
    assert_eq!(flow.apply(Input::Char('z')), Step::Redraw);
    let rows = flow.rows();
    assert!(
        rows[0].starts_with("▸ zed/zeta"),
        "the highlight stayed on a hidden row: {rows:?}"
    );
    // And Enter confirms the row the highlight is actually on.
    assert_eq!(
        flow.apply(Input::Enter),
        Step::Create(NewSessionRequest {
            register: None,
            repo: "https://github.com/zed/zeta".to_string(),
            label: "zeta".to_string(),
        })
    );
}

/// The overlay says where in the list the eight-row window sits.
///
/// Why (#7421 acceptance 2): with twenty-seven registrations and no indicator,
/// a project below the fold was indistinguishable from one that is not
/// registered at all.
#[test]
fn new_session_position_indicator_reports_the_window() {
    let many: Vec<Project> = (0..26)
        .map(|i| {
            project(
                &format!("p{i:02}"),
                &format!("https://ex.test/o{i:02}/p{i:02}"),
            )
        })
        .collect();
    let mut flow =
        NewSessionFlow::with_resolver(new_session::targets_from(&many, &[]), stub_identity);
    // Twenty-six projects plus the typed-path escape.
    assert_eq!(flow.position(), Some("1–8 of 27".to_string()));
    for _ in 0..9 {
        flow.apply(Input::Down);
    }
    assert_eq!(flow.position(), Some("3–10 of 27".to_string()));
    // A short list needs no indicator at all.
    let short = NewSessionFlow::with_resolver(new_session::targets_from(&[], &[]), stub_identity);
    assert_eq!(short.position(), None);
}

/// The rendered overlay carries both the position and the typed filter.
#[test]
fn render_new_session_overlay_shows_the_position_and_filter() {
    let sessions = fleet();
    let many: Vec<Project> = (0..26)
        .map(|i| {
            project(
                &format!("p{i:02}"),
                &format!("https://ex.test/o{i:02}/p{i:02}"),
            )
        })
        .collect();
    let mut state = browsing();
    state.open_new_session(NewSessionFlow::with_resolver(
        new_session::targets_from(&many, &[]),
        stub_identity,
    ));
    let joined = draw(110, 30, &sessions, &mut state).join("\n");
    assert!(joined.contains("1–8 of 27"), "{joined}");
    assert!(joined.contains("Type to filter"), "{joined}");

    state.apply(Input::Char('o'), &sessions);
    state.apply(Input::Char('1'), &sessions);
    let joined = draw(110, 30, &sessions, &mut state).join("\n");
    assert!(joined.contains("filter: o1"), "{joined}");
}

/// A row wider than the pane is cut with `…` rather than wrapped.
#[test]
fn render_new_session_overlay_truncates_a_long_row() {
    let sessions = fleet();
    let mut state = browsing();
    let long = format!("https://github.com/{}/{}", "o".repeat(40), "r".repeat(40));
    state.open_new_session(NewSessionFlow::with_resolver(
        vec![target("long", &long), Target::Other],
        stub_identity,
    ));
    let screen = draw(80, 24, &sessions, &mut state);
    let joined = screen.join("\n");
    assert!(joined.contains('…'), "the long row was not cut: {joined}");
    assert!(
        !joined.contains(&"r".repeat(40)),
        "the long row wrapped instead of being cut: {joined}"
    );
}

// ── new-session picker grouping and free-text entry (#7488) ─────────────────

/// A registry spanning two GitHub owners, a self-hosted forge, a GitLab owner
/// and one registration with no remote at all.
///
/// Why: the four cases the grouping has to tell apart, in one fixture — and
/// `alpha.example.com` deliberately hosts a `zed/infra` whose LABEL sorts near
/// the end, so a plain label sort cannot produce the expected order by accident.
fn grouped_projects() -> Vec<Project> {
    vec![
        project("api", "https://github.com/acme/api"),
        project("charts", "https://gitlab.com/zeta/charts"),
        project("infra", "https://alpha.example.com/zed/infra"),
        project("notes", ""),
        project("tools", "git@gitlab.com:zeta/tools.git"),
        project("trusty-tools", "https://github.com/bobmatnyc/trusty-tools"),
        project("widgets", "https://github.com/acme/widgets"),
    ]
}

/// Rows group by owner-or-domain, then by name, whatever order they arrive in.
///
/// Why (#7488 acceptance 1): the owner asked for the list to be structured by
/// who owns the project. `zed/infra` is on a self-hosted host, so it groups
/// under `alpha.example.com` — between `acme` and `bobmatnyc` — while a plain
/// alphabetical sort of the LABELS would put it second from last. That gap is
/// what makes this fail against #7421's ordering.
#[test]
fn new_session_order_groups_by_owner_then_domain() {
    let forward = new_session::targets_from(&grouped_projects(), &[]);
    assert_eq!(
        new_session::row_labels(&forward),
        vec![
            // github.com → the owner is the group.
            "acme/api".to_string(),
            "acme/widgets".to_string(),
            // Not a known forge → the host is the group, so it sorts under `a`.
            "zed/infra".to_string(),
            "bobmatnyc/trusty-tools".to_string(),
            "zeta/charts".to_string(),
            "zeta/tools".to_string(),
            // No remote → one fixed group, always last.
            "notes (local)".to_string(),
            "other — type a project path…".to_string(),
        ]
    );

    // Two differently shuffled inputs, one order (#7488 acceptance 1).
    let mut shuffled = grouped_projects();
    shuffled.reverse();
    shuffled.swap(0, 4);
    shuffled.swap(1, 5);
    assert_eq!(
        new_session::targets_from(&shuffled, &[]),
        forward,
        "the row order followed the input order"
    );
    let mut other = grouped_projects();
    other.rotate_left(3);
    other.swap(2, 6);
    assert_eq!(
        new_session::targets_from(&other, &[]),
        forward,
        "a second shuffle produced a third order"
    );
}

/// A live session lifts its whole OWNER, and stays first inside it.
///
/// Why (#7488 acceptance 1): #7421's "the projects I am working in come first"
/// has to survive the grouping, and the least surprising way to keep both is to
/// move the group rather than pull one row out of it. `zeta` sorts last of the
/// remote groups, so an implementation that ignores sessions fails here, and one
/// that hoists the single project out of its group fails on the second row.
#[test]
fn new_session_order_lifts_the_whole_group_of_a_live_session() {
    let mut live = session("z1", "active", 1);
    live.source_id = Some("zeta/tools".to_string());
    live.last_activity_at = Some("2026-09-11T10:00:00Z".to_string());

    let rows = new_session::targets_from(&grouped_projects(), &[live]);
    assert_eq!(
        new_session::row_labels(&rows),
        vec![
            "zeta/tools".to_string(),
            "zeta/charts".to_string(),
            "acme/api".to_string(),
            "acme/widgets".to_string(),
            "zed/infra".to_string(),
            "bobmatnyc/trusty-tools".to_string(),
            "notes (local)".to_string(),
            "other — type a project path…".to_string(),
        ]
    );
}

/// Group keys: the owner on a known forge, the host anywhere else, one fixed
/// key with no remote.
#[test]
fn new_session_order_group_of_reads_owner_then_domain() {
    use super::new_session_order::group_of;
    assert_eq!(
        group_of("https://github.com/Acme/api"),
        (0, "acme".to_string())
    );
    assert_eq!(
        group_of("git@gitlab.com:zeta/tools.git"),
        (0, "zeta".to_string())
    );
    assert_eq!(
        group_of("https://alpha.example.com/zed/infra"),
        (0, "alpha.example.com".to_string())
    );
    // A local path and an empty registration are the same, always-last group.
    assert_eq!(
        group_of("/Users/me/code/widgets"),
        (1, "(local)".to_string())
    );
    assert_eq!(group_of(""), (1, "(local)".to_string()));
}

/// The three spellings the picker accepts as a project it can clone.
#[test]
fn new_session_entry_parses_the_three_shapes() {
    use super::new_session_entry::{ProjectEntry, parse_project_entry};
    let expect = |text: &str, name: &str, repo_url: &str| {
        assert_eq!(
            parse_project_entry(text),
            Some(ProjectEntry {
                name: name.to_string(),
                repo_url: repo_url.to_string(),
            }),
            "{text}"
        );
    };
    // (a) a full clone URL, kept verbatim so an ssh entry still clones over ssh.
    expect(
        "https://github.com/acme/widgets",
        "widgets",
        "https://github.com/acme/widgets",
    );
    expect(
        "https://github.com/acme/widgets.git",
        "widgets",
        "https://github.com/acme/widgets.git",
    );
    expect(
        "git@github.com:acme/widgets.git",
        "widgets",
        "git@github.com:acme/widgets.git",
    );
    expect(
        "ssh://git@git.example.com/ops/infra",
        "infra",
        "ssh://git@git.example.com/ops/infra",
    );
    // (b) `owner/repo` means github.com.
    expect("acme/widgets", "widgets", "https://github.com/acme/widgets");
    expect(
        "acme/widgets.git",
        "widgets",
        "https://github.com/acme/widgets",
    );
    // (c) `domain/owner/repo` spells the host.
    expect(
        "git.example.com/ops/infra",
        "infra",
        "https://git.example.com/ops/infra",
    );
    expect(
        "localhost/ops/infra",
        "infra",
        "https://localhost/ops/infra",
    );
}

/// Text that names no project is refused, never guessed at.
#[test]
fn new_session_entry_rejects_malformed_text() {
    use super::new_session_entry::parse_project_entry;
    for bad in [
        "",
        "   ",
        "widgets",
        "acme/",
        "/acme/widgets",
        "acme widgets",
        "acme/widgets/extra/deep",
        // A path is the checkout resolver's job, not a clone target.
        "/Users/me/code/widgets",
        "~/code/widgets",
        "./widgets",
        // Two segments whose first is a domain names no owner.
        "example.com/widgets",
        // A URL shape the one remote parser refuses.
        "https://github.com/acme",
    ] {
        assert_eq!(parse_project_entry(bad), None, "{bad:?} was accepted");
    }
}

/// An unregistered entry carries the register leg AND the clone URL.
#[test]
fn new_session_entry_builds_a_clone_and_register_request() {
    let request = super::new_session_entry::request_for_entry("acme/widgets", &targets())
        .expect("owner/repo is a project");
    assert_eq!(
        request,
        NewSessionRequest {
            register: Some(NewProject {
                name: "widgets".to_string(),
                repo_url: "https://github.com/acme/widgets".to_string(),
            }),
            // The daemon clones this, exactly as `tm session new <url>` does.
            repo: "https://github.com/acme/widgets".to_string(),
            label: "widgets".to_string(),
        }
    );
}

/// An entry naming a project the registry already holds never re-registers it.
///
/// Why: `register` is an unqualified upsert, so a redundant call would replace
/// the stored record's optional fields with the two this flow can supply.
#[test]
fn new_session_entry_reuses_a_registered_project() {
    let request = super::new_session_entry::request_for_entry("bobmatnyc/trusty-tools", &targets())
        .expect("a registered project is still a project");
    assert_eq!(request.register, None);
    assert_eq!(request.repo, "https://github.com/bobmatnyc/trusty-tools");
    assert_eq!(request.label, "trusty-tools");
}

/// Typing a project the registry lacks and pressing Enter clones it.
///
/// Why (#7488 acceptance 2): the filter box is where the operator already types
/// a project's name; before this, a name matching nothing emptied the list and
/// Enter only opened a blank path prompt.
#[test]
fn new_session_entry_from_the_filter_creates_a_clone_request() {
    let mut flow = NewSessionFlow::with_resolver(targets(), stub_identity);
    for c in "acme/widgets".chars() {
        assert_eq!(flow.apply(Input::Char(c)), Step::Redraw);
    }
    match flow.apply(Input::Enter) {
        Step::Create(request) => {
            assert_eq!(
                request.register,
                Some(NewProject {
                    name: "widgets".to_string(),
                    repo_url: "https://github.com/acme/widgets".to_string(),
                })
            );
            assert_eq!(request.repo, "https://github.com/acme/widgets");
        }
        other => panic!("the typed project was not confirmed, got {other:?}"),
    }
}

/// A filter that still matches a row keeps Enter meaning "open that row".
#[test]
fn new_session_entry_does_not_hijack_a_matching_filter() {
    let mut flow = NewSessionFlow::with_resolver(targets(), stub_identity);
    for c in "duetto".chars() {
        flow.apply(Input::Char(c));
    }
    assert_eq!(
        flow.apply(Input::Enter),
        Step::Create(NewSessionRequest {
            register: None,
            repo: "https://github.com/duetto/apex".to_string(),
            label: "apex".to_string(),
        })
    );
}

/// Malformed free text is refused inline, with the flow and the filter intact.
///
/// Why (#7488 acceptance 3): nothing may be silently dropped, and no session
/// may be created from text the picker could not read.
#[test]
fn new_session_entry_from_the_filter_rejects_malformed_text() {
    let sessions = fleet();
    let mut state = creating();
    for c in "nope!!".chars() {
        state.apply(Input::Char(c), &sessions);
    }
    assert_eq!(
        state.apply(Input::Enter, &sessions),
        Action::Redraw,
        "malformed text created a session"
    );
    let (text, severity) = state.message().expect("a refusal must be shown");
    assert!(text.contains("nope!!"), "{text}");
    assert_eq!(severity, Severity::Error);
    match state.mode() {
        Mode::New(flow) => assert_eq!(flow.filter(), "nope!!"),
        other => panic!("the flow must stay open, got {other:?}"),
    }
}

/// The typed-path entry takes a clone URL too, not only a checkout path.
#[test]
fn new_session_entry_path_step_accepts_a_clone_url() {
    let request =
        new_session::request_for_path("https://github.com/acme/widgets", &targets(), stub_identity)
            .expect("a clone URL is a project");
    assert_eq!(
        request.register,
        Some(NewProject {
            name: "widgets".to_string(),
            repo_url: "https://github.com/acme/widgets".to_string(),
        })
    );
    assert_eq!(request.repo, "https://github.com/acme/widgets");
}
