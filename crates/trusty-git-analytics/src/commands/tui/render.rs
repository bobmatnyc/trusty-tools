//! Drawing for `tga tui`.
//!
//! Why: this file exists so `state.rs` never has to know about ratatui and
//! `event_loop.rs` never has to know about layout. Every decision it makes is
//! read from [`TuiState`]; the line builders it uses are pure functions that
//! `super::tests` asserts against directly, so the draw calls themselves carry
//! no untested logic.
//! What: [`render`] lays out a title bar, the active pane, and a status bar,
//! and overlays help on `?`.
//! Test: `super::tests::*_lines`.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use trusty_common::monitor::tui_common::{panel_block, render_help_overlay, truncate};

use super::state::{RepoSource, TuiState, View, WorkStatus};

/// Key reference shown in the status bar and the help overlay.
pub const KEY_HINT: &str =
    "[Tab] view  [↑↓] move  [Space] pick  [a] all  [r] pull+correlate  [c] correlate  \
     [f] filter  [g] reload  [?] help  [q] quit";

/// Full help text for the `?` overlay.
///
/// Why: the status bar can only carry the short form.
/// What: the key map plus the one thing an operator most needs to know — that
/// nothing here needs a model or an API key.
/// Test: `super::tests::help_text_states_the_zero_inference_default`.
pub fn help_text() -> String {
    "Tab / 1 2 3   switch view      Space  pick repo\n\
     ↑ ↓ / j k     move cursor      a      all / none\n\
     r             pull + correlate selected repos\n\
     c             correlate only (no network)\n\
     f             cycle results filter (all/linked/unlinked)\n\
     g             reload results   q / Esc  quit\n\
     \n\
     Every view is deterministic: no model and no API key are required."
        .to_string()
}

/// The title bar: the three view tabs, with the active one highlighted.
///
/// Why/What/Test: see `super::tests::title_line_marks_the_active_view`.
pub fn title_line(state: &TuiState) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "tga ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    for view in [View::Repos, View::Progress, View::Results] {
        let style = if view == state.view {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(format!(" {} ", view.label()), style));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// The status bar: worker state, then either a transient message or the keys.
///
/// Why/What/Test: see `super::tests::status_line_reports_worker_state`.
pub fn status_line(state: &TuiState) -> String {
    let work = match &state.status {
        WorkStatus::Idle => "idle".to_string(),
        WorkStatus::Running => "running…".to_string(),
        WorkStatus::Finished(summary) => summary.clone(),
    };
    let dropped = state.progress.dropped();
    let gap = if dropped > 0 {
        // #5197: the counter now covers both feeds the pane renders — the
        // progress bus and the diverted tracing log — so it no longer names
        // just one of them.
        format!("  (dropped {dropped} line(s) under load)")
    } else {
        String::new()
    };
    match state.message.as_deref() {
        Some(m) => format!("{work}{gap}  —  {m}"),
        None => format!("{work}{gap}  —  {KEY_HINT}"),
    }
}

/// One repo-picker line.
///
/// Why/What/Test: see `super::tests::repo_lines_mark_selection_and_source`.
pub fn repo_line(state: &TuiState, index: usize) -> String {
    let Some(entry) = state.repos.get(index) else {
        return String::new();
    };
    let mark = if !entry.is_selectable() {
        "  "
    } else if entry.selected {
        "[x]"
    } else {
        "[ ]"
    };
    format!(
        "{mark} {:<28} {:<7} {}",
        truncate(&entry.name, 28),
        entry.source.label(),
        truncate(&entry.detail, 60)
    )
}

/// One progress line for a stage's target row.
///
/// Why/What/Test: see `super::tests::progress_lines_show_counts_and_outcome`.
///
/// #5361: driven by the stages that actually emitted, not by
/// [`Stage::all`](tga::core::progress::Stage::all) — the audit sweep reports
/// under `Stage::Audit`, which `all()` excludes, so a fixed skeleton would drop
/// its rows on the floor. Unstarted stages were already skipped, so the three
/// pipeline stages render exactly as before.
pub fn progress_lines(state: &TuiState) -> Vec<String> {
    let mut out = Vec::new();
    for stage in state.progress.stages() {
        let summary = state.progress.summary(stage);
        if !summary.is_started() {
            continue;
        }
        out.push(format!(
            "{} — {} done, {} failed, {} skipped, {} running",
            stage.label(),
            summary.completed,
            summary.failed,
            summary.skipped,
            summary.running
        ));
        for row in state.progress.rows(stage) {
            let status = match &row.outcome {
                Some(o) => o.label().to_string(),
                None => "…".to_string(),
            };
            let counts = match row.total {
                Some(t) => format!("{}/{}", row.done, t),
                None => row.done.to_string(),
            };
            let detail = row.detail.as_deref().unwrap_or_default();
            out.push(format!(
                "    {:<32} {:>12}  {:<8} {}",
                truncate(&row.target, 32),
                counts,
                status,
                truncate(detail, 60)
            ));
        }
    }
    out
}

/// The results header: coverage in one line.
///
/// Why/What/Test: see `super::tests::results_header_states_coverage`.
pub fn results_header(state: &TuiState) -> String {
    let c = &state.counts;
    format!(
        "{} commits — {} linked ({:.0}%), {} unlinked ({} with a ticket, {} without) \
         | {} board items, {} referenced | filter: {}",
        c.commits,
        c.linked,
        c.linked_fraction() * 100.0,
        c.unlinked(),
        c.ticketed_unlinked,
        c.unticketed,
        c.work_items,
        c.work_items_linked,
        state.filter.label()
    )
}

/// One correlation-results line.
///
/// Why/What/Test: see `super::tests::result_lines_name_the_linked_items`.
pub fn result_line(state: &TuiState, index: usize) -> String {
    let Some(row) = state.rows.get(index) else {
        return String::new();
    };
    let sha: String = row.sha.chars().take(8).collect();
    let links = if row.work_items.is_empty() {
        match row.ticket_id.as_deref() {
            Some(t) => format!("— no board item for {t}"),
            None => "— no ticket reference".to_string(),
        }
    } else {
        row.work_items
            .iter()
            .map(|(source, id, title)| format!("{source}:{id} {}", truncate(title, 30)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{sha} {:<18} {:<44} {}",
        truncate(&row.repository, 18),
        truncate(&row.subject, 44),
        truncate(&links, 60)
    )
}

/// Draw one frame.
///
/// Why: the single entry point the event loop calls per tick.
/// What: title bar, active pane, status bar, plus the help overlay when up.
/// Test: side-effect-only ratatui drawing; every line it renders comes from a
/// pure builder above, each of which is unit-tested.
pub fn render(frame: &mut Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(frame.area());

    frame.render_widget(Paragraph::new(title_line(state)), chunks[0]);
    match state.view {
        View::Repos => render_repos(frame, state, chunks[1]),
        View::Progress => render_progress(frame, state, chunks[1]),
        View::Results => render_results(frame, state, chunks[1]),
    }
    frame.render_widget(
        Paragraph::new(status_line(state))
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: true }),
        chunks[2],
    );

    if state.show_help {
        render_help_overlay(frame, &help_text());
    }
}

/// Render the repo picker.
fn render_repos(frame: &mut Frame, state: &TuiState, area: Rect) {
    let items: Vec<ListItem> = (0..state.repos.len())
        .map(|i| {
            let style = if i == state.repo_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else if state.repos[i].source == RepoSource::Org {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(repo_line(state, i)).style(style)
        })
        .collect();
    let selected = state.repos.iter().filter(|r| r.selected).count();
    let title = format!("REPOSITORIES ({selected}/{} selected)", state.repos.len());
    frame.render_widget(List::new(items).block(panel_block(&title, true)), area);
}

/// Render the live progress pane: a gauge per running target, then the log.
fn render_progress(frame: &mut Frame, state: &TuiState, area: Rect) {
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    if state.progress.is_empty() {
        frame.render_widget(
            Paragraph::new("Nothing has run yet — press [r] to pull and correlate.")
                .block(panel_block("PROGRESS", true)),
            split[0],
        );
    } else {
        let lines: Vec<ListItem> = progress_lines(state)
            .into_iter()
            .map(ListItem::new)
            .collect();
        frame.render_widget(
            List::new(lines).block(panel_block("PROGRESS", true)),
            split[0],
        );
    }

    let log: Vec<ListItem> = state
        .progress
        .log()
        .rev()
        .take(split[1].height.saturating_sub(2) as usize)
        .map(|l| ListItem::new(l.clone()))
        .collect();
    frame.render_widget(
        List::new(log).block(panel_block("ACTIVITY", false)),
        split[1],
    );
}

/// Render the correlation results view.
fn render_results(frame: &mut Frame, state: &TuiState, area: Rect) {
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let ratio = state.counts.linked_fraction().clamp(0.0, 1.0);
    frame.render_widget(
        Gauge::default()
            .block(panel_block("CORRELATION", false))
            .gauge_style(Style::default().fg(Color::Green))
            .ratio(ratio)
            .label(results_header(state)),
        split[0],
    );

    let items: Vec<ListItem> = (0..state.rows.len())
        .map(|i| {
            let style = if i == state.row_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else if state.rows[i].is_linked() {
                Style::default()
            } else {
                Style::default().fg(Color::Yellow)
            };
            ListItem::new(result_line(state, i)).style(style)
        })
        .collect();
    let title = format!("COMMITS ({} loaded)", state.rows.len());
    frame.render_widget(List::new(items).block(panel_block(&title, true)), split[1]);
}
