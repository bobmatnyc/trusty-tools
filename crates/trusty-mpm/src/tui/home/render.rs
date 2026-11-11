//! ratatui drawing for the console home screen and the service view.
//!
//! Why (#10491): drawing reads `&HomeState` only — it never probes, spawns or
//! awaits — so a slow service can never stall a frame.
//! What: [`render`] dispatches on [`View`]: the console look
//! ([`super::console`], #10966), a service view, or the Architect view
//! (#10966); [`service_lines`] is the pure text builder of the service view.
//! Test: `home_render_draws_every_service_row`, `service_view_shows_unavailable_metrics`.

use crate::tui::theme::palette;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::Value;

use super::console::{model_from_state, render_body, render_header};
use super::services::ServiceId;
use super::state::{HomeState, MetricsState, TileHealth, View};
use crate::tui::VERSION_LABEL;
use crate::tui::architect::render::ARCHITECT_HINT;

/// Footer on the home screen.
pub const HOME_HINT: &str = "[↑↓] select  [Enter] open  [a] architect  [q] quit";
/// Footer on a service view.
pub const SERVICE_HINT: &str = "[Esc] home  [q] quit";
/// Most metrics lines a service view lists.
const MAX_METRIC_LINES: usize = 40;

/// Draw the current view.
pub fn render(frame: &mut Frame, state: &HomeState) {
    crate::tui::theme::paint_background(frame);
    let [header, body, footer] = split(frame.area());
    let hint = match state.view {
        View::Home => {
            render_header(frame, header);
            render_body(frame, body, &model_from_state(state));
            HOME_HINT
        }
        View::Service(id) => {
            render_service_header(frame, header, id);
            frame.render_widget(
                Paragraph::new(service_lines(id, state))
                    .block(Block::default().borders(Borders::ALL))
                    .wrap(Wrap { trim: false }),
                body,
            );
            SERVICE_HINT
        }
        View::Architect => {
            render_header(frame, header);
            crate::tui::architect::render::render(frame, body, &state.architect);
            ARCHITECT_HINT
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(hint)).style(Style::default().fg(palette().muted)),
        footer,
    );
}

/// `trusty console › <service>` with the tm version at the right edge.
fn render_service_header(frame: &mut Frame, area: Rect, id: ServiceId) {
    // #10476: version in tui header (Bob 11-05)
    let [title_area, version_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(VERSION_LABEL.chars().count() as u16),
        ])
        .areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(format!("trusty console › {}", id.id())))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        title_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(VERSION_LABEL)).style(Style::default().fg(palette().muted)),
        version_area,
    );
}

/// Header row, body, footer row.
fn split(area: Rect) -> [Rect; 3] {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
    [chunks[0], chunks[1], chunks[2]]
}

/// The text of one service's first-level view: health detail, then metrics.
pub fn service_lines(id: ServiceId, state: &HomeState) -> Vec<Line<'static>> {
    let health = state.health(id);
    let mut lines = vec![
        Line::from(format!("service   {}", id.id())),
        Line::from(format!("binary    {}", id.binary())),
        Line::from(format!("health    {}", health.label())),
    ];
    match health {
        TileHealth::Up {
            version: Some(version),
        } => lines.push(Line::from(format!("version   {version}"))),
        TileHealth::Degraded { reason } | TileHealth::Down { reason } => {
            lines.push(Line::from(format!("detail    {reason}")))
        }
        TileHealth::NoUdsEndpoint { detail } => {
            lines.push(Line::from(format!("detail    {detail}")))
        }
        _ => {}
    }
    // See #10899: a service with no UDS endpoint has no socket to show.
    if !matches!(health, TileHealth::NoUdsEndpoint { .. })
        && let Ok(socket) = id.socket()
    {
        lines.push(Line::from(format!("socket    {}", socket.display())));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "console_metrics",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    match state.metrics.get(&id) {
        None | Some(MetricsState::Loading) => lines.push(Line::from("loading…")),
        Some(MetricsState::Unavailable { reason, .. }) => {
            lines.push(Line::from(format!("unavailable: {reason}")))
        }
        Some(MetricsState::Ready { report, .. }) => {
            lines.push(Line::from(format!(
                "{} v{} — {:?} (schema {})",
                report.display_name, report.version, report.status, report.metrics_schema_version
            )));
            lines.extend(metric_lines(&report.metrics).into_iter().map(Line::from));
        }
    }
    lines
}

/// One line per top-level metrics field; nested values are summarised.
pub fn metric_lines(metrics: &Value) -> Vec<String> {
    let Some(object) = metrics.as_object() else {
        return vec![format!("  {metrics}")];
    };
    object
        .iter()
        .take(MAX_METRIC_LINES)
        .map(|(key, value)| match value {
            Value::Object(map) => format!("  {key}: {{{} fields}}", map.len()),
            Value::Array(items) => format!("  {key}: [{} items]", items.len()),
            Value::String(s) => format!("  {key}: {s}"),
            other => format!("  {key}: {other}"),
        })
        .collect()
}
