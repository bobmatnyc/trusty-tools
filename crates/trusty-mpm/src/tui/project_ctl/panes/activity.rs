//! Activity pane (bottom strip, 4 rows) — DOC-35 §5 pane 3, live-wired
//! (#2119). Explicit "unavailable" state after persistent per-session fetch
//! failures (#2470).
//!
//! Why: DOC-35 §5.4 sources this pane from `GET .../{id}/activity` (`state`,
//! `summary`, `pending_decision`, `proposed_default`, plus a short
//! `raw_pane` tail per the §5.1 mockup); #2118 shipped a skeleton that
//! rendered only the focused session's already-polled STATIC record fields
//! because that live wiring was explicitly this issue's scope. This module
//! now prefers [`ProjectCtlState::activity_for_selected`] (populated by
//! [`super::super::poll::refresh_activity`] on the same poll cadence as the
//! Projects/Sessions panes) and falls back to the static [`SessionRow`]
//! fields — with an explanatory suffix, never silently — for the windows
//! where no live snapshot is available: right after a selection changes (the
//! fetch for the new selection has not landed), while the daemon is down
//! with no prior live fetch to show as stale, and — #2470 — when the daemon
//! IS reachable but this session's `/activity` fetch has failed
//! non-transport-classified [`super::super::state::ACTIVITY_UNAVAILABLE_THRESHOLD`]
//! times in a row (a 404 from an older daemon lacking the endpoint, a GC'd
//! session, a malformed body): without this branch, `activity_for_selected`
//! stays `None` forever and the pane would render "loading…" indefinitely
//! instead of ever surfacing the persistent failure.
//! What: [`activity_lines`] is a pure builder (unit tested without a
//! terminal) covering five shapes: no session selected; live data available
//! (optionally suffixed `[stale]` when the last fetch failed but a prior one
//! is being kept); no live snapshot and the failure streak has crossed the
//! threshold ("activity unavailable for this session"); daemon reachable, no
//! live snapshot yet, streak below threshold ("loading…"); daemon down with
//! nothing live to show yet ("daemon unreachable"). [`render`] wraps it in a
//! bordered `Paragraph`.
//! Test: `tests` covers all five shapes above.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::project_ctl::state::{Pane, ProjectCtlState};

/// Format the shared "pending decision" segment from a question/default pair.
///
/// Why: both the live-activity render path and the static-fallback path build
/// the identical `" · pending: \"...\" · proposed default: ..."` /
/// `" · no pending decision"` segment off differently-sourced (but
/// identically-shaped) `Option<String>` pairs; factoring it out avoids
/// duplicating the three-way match.
/// What: `(Some(q), Some(d))` → both; `(Some(q), None)` → question only;
/// otherwise → the "no pending decision" fallback.
fn decision_segment(pending: &Option<String>, proposed: &Option<String>) -> String {
    match (pending, proposed) {
        (Some(q), Some(d)) => format!(" · pending: \"{q}\" · proposed default: {d}"),
        (Some(q), None) => format!(" · pending: \"{q}\""),
        _ => " · no pending decision".to_string(),
    }
}

/// Build the Activity pane's body lines for the currently selected session.
///
/// Why: a pure text builder keeps every rendered shape testable without a
/// terminal; see the module doc for the four shapes.
/// What: no selection → a placeholder. A live snapshot that matches the
/// current selection (`state.activity_for_selected()`) → header, `state:
/// <word><decision segment>[ [stale]]`, `summary: <text>`, then the raw-pane
/// tail lines verbatim. No live snapshot, and
/// `state.activity_unavailable_for_selected()` (#2470) → header plus an
/// explicit "activity unavailable for this session" line — checked BEFORE
/// the loading/daemon-down fallbacks below, since a persistent per-session
/// failure is a more specific, more useful signal than either. No live
/// snapshot yet, daemon reachable, streak below threshold → header plus the
/// static record's state/decision segment suffixed `· loading live
/// activity…`. No live snapshot yet, daemon down → header plus an explicit
/// "daemon unreachable" line (DOC-35's graceful-daemon-down requirement: an
/// explicit message, never a silent blank pane).
/// Test: `activity_lines_no_selection`, `activity_lines_shows_live_activity`,
/// `activity_lines_shows_pending_decision_from_live_activity`,
/// `activity_lines_marks_stale_activity`,
/// `activity_lines_falls_back_while_loading`,
/// `activity_lines_reports_daemon_unreachable_with_no_prior_fetch`,
/// `activity_lines_reports_unavailable_after_persistent_failures`.
pub fn activity_lines(state: &ProjectCtlState) -> Vec<String> {
    let Some(session) = state.selected_session() else {
        return vec![
            "no session selected".to_string(),
            "(select a project and a session to see its live activity)".to_string(),
        ];
    };

    let header = format!("session {} ({})", session.name, session.short_id);

    if let Some(activity) = state.activity_for_selected() {
        let stale_suffix = if activity.stale { " [stale]" } else { "" };
        let mut lines = vec![
            header,
            format!(
                "state: {}{}{stale_suffix}",
                activity.state,
                decision_segment(&activity.pending_decision, &activity.proposed_default)
            ),
            format!("summary: {}", activity.summary),
        ];
        lines.extend(activity.raw_pane_tail.iter().cloned());
        return lines;
    }

    if state.activity_unavailable_for_selected() {
        return vec![
            header,
            "activity unavailable for this session".to_string(),
            "(the daemon is reachable, but repeated fetches for this session's activity failed)"
                .to_string(),
        ];
    }

    if state.daemon_reachable {
        vec![
            header,
            format!(
                "state: {}{} · loading live activity…",
                session.state,
                decision_segment(&session.pending_decision, &session.proposed_default)
            ),
        ]
    } else {
        vec![
            header,
            "daemon unreachable — live activity unavailable".to_string(),
            format!("last known state: {}", session.state),
        ]
    }
}

/// Draw the Activity pane into `area`.
///
/// Why: the single entry point [`super::super::layout::render`] calls for the
/// bottom strip.
/// What: a bordered, titled (`ACTIVITY`) `Paragraph` over [`activity_lines`];
/// the title is styled cyan/bold when this pane has focus.
pub fn render(frame: &mut Frame, area: Rect, state: &ProjectCtlState) {
    let focused = state.focus == Pane::Activity;
    let title_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let lines: Vec<Line<'static>> = activity_lines(state).into_iter().map(Line::from).collect();
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Line::from("ACTIVITY").style(title_style)),
    );
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::project_ctl::state::{ActivityInfo, ProjectRow, SessionRow};

    fn state_with_session(session: SessionRow) -> ProjectCtlState {
        let mut state = ProjectCtlState {
            projects: vec![ProjectRow {
                name: "widget".to_string(),
                repo_url: "https://github.com/acme/widget".to_string(),
                live_count: 1,
                total_count: 1,
            }],
            ..Default::default()
        };
        state
            .sessions_by_project
            .insert("widget".to_string(), vec![session]);
        state.projects_nav.sync_len(state.projects.len());
        state.sessions_nav.sync_len(state.current_sessions().len());
        state
    }

    fn base_session() -> SessionRow {
        SessionRow {
            id: "4f9ca1b2ffff".to_string(),
            short_id: "4f9ca1b2".to_string(),
            name: "widget-session".to_string(),
            branch: Some("main".to_string()),
            task: Some("ship the thing".to_string()),
            state: "active".to_string(),
            pending_decision: None,
            proposed_default: None,
            deliverable_id: None,
        }
    }

    fn live_activity(session_id: &str) -> ActivityInfo {
        ActivityInfo {
            session_id: session_id.to_string(),
            state: "blocked_on_permission".to_string(),
            summary: "waiting on a ci.yml write approval".to_string(),
            pending_decision: None,
            proposed_default: None,
            raw_pane_tail: vec!["$ echo hi".to_string(), "hi".to_string()],
            stale: false,
        }
    }

    #[test]
    fn activity_lines_no_selection() {
        let state = ProjectCtlState::default();
        let lines = activity_lines(&state);
        assert!(lines[0].contains("no session selected"));
    }

    #[test]
    fn activity_lines_shows_live_activity() {
        let mut state = state_with_session(base_session());
        state.activity = Some(live_activity("4f9ca1b2ffff"));
        let lines = activity_lines(&state);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("state: blocked_on_permission"))
        );
        assert!(lines.iter().any(|l| l.contains("no pending decision")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("summary: waiting on a ci.yml write approval"))
        );
        assert!(lines.iter().any(|l| l.contains("$ echo hi")));
        assert!(!lines.iter().any(|l| l.contains("[stale]")));
    }

    #[test]
    fn activity_lines_shows_pending_decision_from_live_activity() {
        let mut activity = live_activity("4f9ca1b2ffff");
        activity.pending_decision = Some("write to ci.yml?".to_string());
        activity.proposed_default = Some("yes".to_string());
        let mut state = state_with_session(base_session());
        state.activity = Some(activity);
        let lines = activity_lines(&state);
        assert!(lines.iter().any(|l| l.contains("write to ci.yml?")));
        assert!(lines.iter().any(|l| l.contains("proposed default: yes")));
    }

    #[test]
    fn activity_lines_marks_stale_activity() {
        let mut activity = live_activity("4f9ca1b2ffff");
        activity.stale = true;
        let mut state = state_with_session(base_session());
        state.activity = Some(activity);
        let lines = activity_lines(&state);
        assert!(lines.iter().any(|l| l.contains("[stale]")));
    }

    #[test]
    fn activity_lines_falls_back_while_loading() {
        // No live activity yet for this selection, but the daemon is up — the
        // static session-record fields are shown, clearly labeled "loading".
        let mut state = state_with_session(base_session());
        state.daemon_reachable = true;
        let lines = activity_lines(&state);
        assert!(lines.iter().any(|l| l.contains("state: active")));
        assert!(lines.iter().any(|l| l.contains("loading live activity")));
    }

    #[test]
    fn activity_lines_falls_back_while_activity_is_stale_selection() {
        // A live fetch landed, but for a DIFFERENT session than the one now
        // selected (the selection moved between polls) — it must not render
        // under this session's header.
        let mut state = state_with_session(base_session());
        state.daemon_reachable = true;
        state.activity = Some(live_activity("some-other-session"));
        let lines = activity_lines(&state);
        assert!(lines.iter().any(|l| l.contains("loading live activity")));
    }

    #[test]
    fn activity_lines_reports_daemon_unreachable_with_no_prior_fetch() {
        let mut state = state_with_session(base_session());
        state.daemon_reachable = false;
        let lines = activity_lines(&state);
        assert!(lines.iter().any(|l| l.contains("daemon unreachable")));
        assert!(lines.iter().any(|l| l.contains("last known state: active")));
    }

    /// #2470 — a persistent per-session `/activity` failure (daemon healthy,
    /// this session's fetch itself keeps failing) must eventually stop
    /// rendering "loading…" forever and report an explicit unavailable
    /// state instead.
    #[test]
    fn activity_lines_reports_unavailable_after_persistent_failures() {
        let session = base_session();
        let mut state = state_with_session(session.clone());
        state.daemon_reachable = true;
        for _ in 0..3 {
            state.activity_failures.record_failure(&session.id);
        }
        let lines = activity_lines(&state);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("activity unavailable for this session"))
        );
        assert!(!lines.iter().any(|l| l.contains("loading live activity")));
    }
}
