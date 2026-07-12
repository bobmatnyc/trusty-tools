//! Sessions pane (right column, 75% width) — DOC-35 §5 pane 2.
//!
//! Why: the selected project's sessions, numbered per DOC-16 §3.2, each
//! showing a lifecycle-state glyph, branch, and a one-line detail. The glyph
//! is driven ONLY by the session record's static `ManagedSessionState` word —
//! there is no live `/activity` polling in this issue's scope (#2119 wires
//! that live "awaiting approval" / "idle Nm" detail into this pane later).
//! A second, independent glyph (DOC-35 §10.6, #2383) marks a row whose
//! `deliverable_id` is bound to a Deliverable — resolved (`◆`, cyan) when it
//! matches one of [`ProjectCtlState::deliverables`], dangling (`◈`, dim red)
//! when it CONFIRMS the id does not resolve, and no glyph at all when the
//! link state is [`DeliverableLinkState::Unknown`] (no successfully fetched
//! Deliverable set exists yet — a transient fetch failure must never be
//! rendered as a confirmed-dangling reference, see
//! [`ProjectCtlState::deliverable_link_state`]'s own doc for the full
//! rationale). The spec (§12 as filed) enumerates the WORK ITEM but does not
//! pin an exact glyph/color — this is a disclosed design choice, not a
//! literal spec transcription; see the PR body for the full disclosure.
//! What: [`state_glyph`] / [`deliverable_glyph`] / [`session_line`] are pure
//! builders (unit tested without a terminal); [`render`] composes them into a
//! ratatui stateful `List`, passing the current tick's [`DeliverableLinkState`]
//! per row so the glyph resolves without a second daemon round trip per row.
//! Test: `tests` covers all three glyph states and the line content.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem},
};

use crate::tui::project_ctl::state::{DeliverableLinkState, Pane, ProjectCtlState, SessionRow};

/// Glyph for a session whose runtime is currently running.
pub const ACTIVE_GLYPH: char = '●';
/// Glyph for a session whose workspace is being provisioned.
pub const PROVISIONING_GLYPH: char = '◍';
/// Glyph for a session with an intact, resumable, stopped runtime.
pub const STOPPED_GLYPH: char = '○';
/// Glyph for a session whose provisioning or runtime spawn failed.
pub const ERRORED_GLYPH: char = '✗';
/// Glyph for a decommissioned (tombstoned) session.
pub const DECOMMISSIONED_GLYPH: char = '⊘';

/// Pick the lifecycle-state glyph for one session's `state` word.
///
/// Why: the static `ManagedSessionState` word (`active`/`provisioning`/
/// `stopped`/`errored`/`decommissioned`, see `session_manager::record`) is
/// the only state this issue's skeleton has available — the live
/// "awaiting approval" / "idle Nm" detail the mockup shows comes from the
/// `/activity` endpoint, deferred to #2119. The `"decommissioned"` arm is
/// kept for completeness and stays directly unit tested, but is DEAD in the
/// live poll path as of #2476:
/// [`super::super::poll::live_session_rows`] drops every decommissioned
/// session before a [`SessionRow`] is ever built, so this function never
/// actually receives that word from [`render`] — a decommissioned session's
/// row simply stops existing rather than rendering tombstoned.
/// What: maps each of the five known state words to its glyph; any other
/// (forward-compat) value falls back to [`STOPPED_GLYPH`].
/// Test: `state_glyph_maps_every_known_state`, `state_glyph_unknown_falls_back`.
pub fn state_glyph(state: &str) -> char {
    match state {
        "active" => ACTIVE_GLYPH,
        "provisioning" => PROVISIONING_GLYPH,
        "errored" => ERRORED_GLYPH,
        "decommissioned" => DECOMMISSIONED_GLYPH,
        _ => STOPPED_GLYPH,
    }
}

/// Glyph for a session bound to a Deliverable that resolves (DOC-35 §10.6,
/// #2383).
pub const DELIVERABLE_GLYPH: char = '◆';
/// Glyph for a session bound to a Deliverable id that does NOT resolve
/// against the currently fetched deliverable set — a dangling reference
/// (e.g. the Deliverable was deleted out from under the session).
pub const DELIVERABLE_DANGLING_GLYPH: char = '◈';

/// Pick the deliverable-link glyph (and whether to show one at all) for a
/// session row.
///
/// Why: a session row's `deliverable_id` alone cannot say whether the link is
/// live — that requires checking it against the project's currently fetched
/// Deliverable set (DOC-35 §10.6) via [`DeliverableLinkState`]. Kept as a
/// small pure function so all three states are unit-testable independent of
/// rendering. The dangling case is styled DIM (in addition to red) so it
/// reads as visually distinct from [`ERRORED_GLYPH`]'s full-intensity red —
/// two identical reds on one row would otherwise conflate "this session
/// errored" with "this session's Deliverable link is dangling".
/// What: `None` when `deliverable_id` is `None` (unbound — the common case)
/// OR `link_state` is [`DeliverableLinkState::Unknown`] (nothing confirmed
/// yet — showing a glyph here would be a guess, never a confirmed dangling
/// ref). `Some((`[`DELIVERABLE_GLYPH`]`, cyan))` when [`DeliverableLinkState::Resolved`].
/// `Some((`[`DELIVERABLE_DANGLING_GLYPH`]`, dim red))` when
/// [`DeliverableLinkState::Dangling`].
/// Test: `deliverable_glyph_none_when_unbound`,
/// `deliverable_glyph_none_when_unknown`,
/// `deliverable_glyph_resolved_and_dangling`.
pub fn deliverable_glyph(
    deliverable_id: Option<&str>,
    link_state: DeliverableLinkState,
) -> Option<(char, Style)> {
    deliverable_id?;
    match link_state {
        DeliverableLinkState::Unknown => None,
        DeliverableLinkState::Resolved => {
            Some((DELIVERABLE_GLYPH, Style::default().fg(Color::Cyan)))
        }
        DeliverableLinkState::Dangling => Some((
            DELIVERABLE_DANGLING_GLYPH,
            Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
        )),
    }
}

/// Build one numbered session row: `N. <glyph> <short-id>  <branch>  <detail>`,
/// with an optional trailing deliverable-link glyph (DOC-35 §10.6, #2383).
///
/// Why: DOC-16 §3.2 numbers session rows so an operator can refer to one by
/// its list position; the detail column prefers the task description and
/// falls back to the raw state word when no task is recorded.
/// `deliverable_link_state` is resolved by the caller ([`render`]) via
/// [`ProjectCtlState::deliverable_link_state`] so this function stays a pure,
/// terminal-free builder.
/// What: returns e.g. `1. ● 4f9ca1b2  main  ship the thing ◆` when bound and
/// resolved, `… ◈` (dim) when bound and confirmed dangling, or no trailing
/// glyph when unbound OR the link state is still
/// [`DeliverableLinkState::Unknown`].
/// Test: `session_line_shows_number_glyph_branch_and_task`,
/// `session_line_falls_back_to_state_word`,
/// `session_line_appends_deliverable_glyph_when_bound`.
pub fn session_line(
    number: usize,
    row: &SessionRow,
    deliverable_link_state: DeliverableLinkState,
) -> Line<'static> {
    let glyph = state_glyph(&row.state);
    let branch = row.branch.clone().unwrap_or_else(|| "-".to_string());
    let detail = row.task.clone().unwrap_or_else(|| row.state.clone());
    let mut spans = vec![
        Span::styled(
            format!("{number:>2}. "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(format!("{glyph} "), glyph_style(&row.state)),
        Span::styled(
            format!("{}  ", row.short_id),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{branch}  ")),
        Span::styled(detail, Style::default().fg(Color::DarkGray)),
    ];
    if let Some((glyph, style)) =
        deliverable_glyph(row.deliverable_id.as_deref(), deliverable_link_state)
    {
        spans.push(Span::styled(format!("  {glyph}"), style));
    }
    Line::from(spans)
}

/// Style a state glyph by its lifecycle state.
fn glyph_style(state: &str) -> Style {
    match state {
        "active" => Style::default().fg(Color::Green),
        "provisioning" => Style::default().fg(Color::Yellow),
        "errored" => Style::default().fg(Color::Red),
        "decommissioned" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::Gray),
    }
}

/// Draw the Sessions pane into `area`.
///
/// Why: the single entry point [`super::super::layout::render`] calls for the
/// right column.
/// What: a bordered, titled (`SESSIONS — <project> (N)`) stateful `List`
/// scoped to `state.current_sessions()`; the title names the selected
/// project, or reads `SESSIONS` when none is selected (empty registry).
pub fn render(frame: &mut Frame, area: Rect, state: &mut ProjectCtlState) {
    let focused = state.focus == Pane::Sessions;
    let sessions = state.current_sessions().to_vec();
    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let link_state = s
                .deliverable_id
                .as_deref()
                .map(|id| state.deliverable_link_state(id))
                .unwrap_or(DeliverableLinkState::Unknown);
            ListItem::new(session_line(i + 1, s, link_state))
        })
        .collect();
    let project_label = state.selected_project_name().unwrap_or("-");
    let title = Line::from(format!("SESSIONS — {project_label} ({})", sessions.len()))
        .style(title_style(focused));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, area, state.sessions_nav.state_mut());
}

/// Style the pane title, brighter when it holds focus.
fn title_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: &str) -> SessionRow {
        SessionRow {
            id: "4f9ca1b2ffff".to_string(),
            short_id: "4f9ca1b2".to_string(),
            name: "session".to_string(),
            branch: Some("feat/x".to_string()),
            task: Some("ship the thing".to_string()),
            state: state.to_string(),
            pending_decision: None,
            proposed_default: None,
            deliverable_id: None,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn state_glyph_maps_every_known_state() {
        assert_eq!(state_glyph("active"), ACTIVE_GLYPH);
        assert_eq!(state_glyph("provisioning"), PROVISIONING_GLYPH);
        assert_eq!(state_glyph("stopped"), STOPPED_GLYPH);
        assert_eq!(state_glyph("errored"), ERRORED_GLYPH);
        assert_eq!(state_glyph("decommissioned"), DECOMMISSIONED_GLYPH);
    }

    #[test]
    fn state_glyph_unknown_falls_back() {
        assert_eq!(state_glyph("something-new"), STOPPED_GLYPH);
    }

    #[test]
    fn session_line_shows_number_glyph_branch_and_task() {
        let text = line_text(&session_line(
            1,
            &row("active"),
            DeliverableLinkState::Unknown,
        ));
        assert!(text.starts_with(" 1. "), "missing number: {text}");
        assert!(text.contains(ACTIVE_GLYPH), "missing glyph: {text}");
        assert!(text.contains("4f9ca1b2"), "missing short id: {text}");
        assert!(text.contains("feat/x"), "missing branch: {text}");
        assert!(text.contains("ship the thing"), "missing task: {text}");
    }

    #[test]
    fn session_line_falls_back_to_state_word() {
        let mut r = row("stopped");
        r.task = None;
        let text = line_text(&session_line(2, &r, DeliverableLinkState::Unknown));
        assert!(text.contains("stopped"), "missing state fallback: {text}");
    }

    #[test]
    fn deliverable_glyph_none_when_unbound() {
        assert_eq!(
            deliverable_glyph(None, DeliverableLinkState::Resolved),
            None
        );
        assert_eq!(
            deliverable_glyph(None, DeliverableLinkState::Dangling),
            None
        );
        assert_eq!(deliverable_glyph(None, DeliverableLinkState::Unknown), None);
    }

    #[test]
    fn deliverable_glyph_none_when_unknown() {
        // Bound (id is Some), but no successfully fetched Deliverable set
        // exists yet — must show NO glyph, never the dangling one (the bug
        // this fix addresses: a transient fetch failure must not read as
        // "confirmed deleted").
        assert_eq!(
            deliverable_glyph(Some("d-1"), DeliverableLinkState::Unknown),
            None
        );
    }

    #[test]
    fn deliverable_glyph_resolved_and_dangling() {
        let (glyph, style) = deliverable_glyph(Some("d-1"), DeliverableLinkState::Resolved)
            .expect("resolved must show a glyph");
        assert_eq!(glyph, DELIVERABLE_GLYPH);
        assert_eq!(style.fg, Some(Color::Cyan));

        let (glyph, style) = deliverable_glyph(Some("d-1"), DeliverableLinkState::Dangling)
            .expect("dangling must show a glyph");
        assert_eq!(glyph, DELIVERABLE_DANGLING_GLYPH);
        assert_eq!(style.fg, Some(Color::Red));
        assert!(
            style.add_modifier.contains(Modifier::DIM),
            "the dangling glyph must be dimmed so it reads distinct from the \
             full-intensity red ERRORED_GLYPH: {style:?}"
        );
    }

    #[test]
    fn session_line_appends_deliverable_glyph_when_bound() {
        let mut r = row("active");
        r.deliverable_id = None;
        assert!(
            !line_text(&session_line(1, &r, DeliverableLinkState::Resolved))
                .contains(DELIVERABLE_GLYPH),
            "unbound row must show no deliverable glyph"
        );

        r.deliverable_id = Some("d-1".to_string());
        let unknown_text = line_text(&session_line(1, &r, DeliverableLinkState::Unknown));
        assert!(
            !unknown_text.contains(DELIVERABLE_GLYPH)
                && !unknown_text.contains(DELIVERABLE_DANGLING_GLYPH),
            "a bound row with an Unknown link state must show NO deliverable glyph: {unknown_text}"
        );

        let resolved_text = line_text(&session_line(1, &r, DeliverableLinkState::Resolved));
        assert!(
            resolved_text.contains(DELIVERABLE_GLYPH),
            "resolved link must show the resolved glyph: {resolved_text}"
        );

        let dangling_text = line_text(&session_line(1, &r, DeliverableLinkState::Dangling));
        assert!(
            dangling_text.contains(DELIVERABLE_DANGLING_GLYPH),
            "dangling link must show the dangling glyph: {dangling_text}"
        );
    }
}
