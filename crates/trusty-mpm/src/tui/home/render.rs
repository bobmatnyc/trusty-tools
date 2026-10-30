//! ratatui drawing for the console home screen and the service view.
//!
//! Why (#10491): drawing reads `&HomeState` only — it never probes, spawns or
//! awaits — so a slow service can never stall a frame.
//! What: [`render`] dispatches on [`View`]; [`tile_lines`] and
//! [`service_lines`] are the pure text builders.
//! Test: `home_render_draws_every_tile`, `service_view_shows_unavailable_metrics`.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::Value;

use super::services::ServiceId;
use super::state::{COLUMNS, HomeState, MetricsState, TileHealth, View};

/// Footer on the tile grid.
pub const HOME_HINT: &str = "[←→↑↓] select  [Enter] open  [q] quit";
/// Footer on a service view.
pub const SERVICE_HINT: &str = "[Esc] home  [q] quit";
/// Most metrics lines a service view lists.
const MAX_METRIC_LINES: usize = 40;

/// Draw the current view.
pub fn render(frame: &mut Frame, state: &HomeState) {
    let [header, body, footer] = split(frame.area());
    let (title, hint) = match state.view {
        View::Home => ("trusty console — services".to_string(), HOME_HINT),
        View::Service(id) => (format!("trusty console › {}", id.id()), SERVICE_HINT),
    };
    frame.render_widget(
        Paragraph::new(Line::from(title)).style(Style::default().add_modifier(Modifier::BOLD)),
        header,
    );
    match state.view {
        View::Home => render_grid(frame, body, state),
        View::Service(id) => frame.render_widget(
            Paragraph::new(service_lines(id, state))
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            body,
        ),
    }
    frame.render_widget(
        Paragraph::new(Line::from(hint)).style(Style::default().add_modifier(Modifier::REVERSED)),
        footer,
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

/// The tile grid, [`COLUMNS`] wide.
fn render_grid(frame: &mut Frame, area: Rect, state: &HomeState) {
    let rows = state.tiles.len().div_ceil(COLUMNS).max(1);
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Ratio(1, rows as u32); rows])
        .split(area);
    for (index, (id, health)) in state.tiles.iter().enumerate() {
        let Some(row) = row_areas.get(index / COLUMNS) else {
            continue;
        };
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Ratio(1, COLUMNS as u32); COLUMNS])
            .split(*row);
        let border = if index == state.selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(tile_lines(*id, health))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border)
                        .title(id.display_name()),
                )
                .wrap(Wrap { trim: true }),
            cols[index % COLUMNS],
        );
    }
}

/// The colour a tile's status word takes.
fn health_color(health: &TileHealth) -> Color {
    match health {
        TileHealth::Up { .. } => Color::Green,
        TileHealth::OnDemand => Color::Blue,
        TileHealth::Degraded { .. } | TileHealth::Timeout => Color::Yellow,
        TileHealth::Down { .. } => Color::Red,
        TileHealth::Absent | TileHealth::Pending => Color::DarkGray,
    }
}

/// The text inside one tile.
pub fn tile_lines(id: ServiceId, health: &TileHealth) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(
            format!("● {}", health.label()),
            Style::default().fg(health_color(health)),
        ),
        Line::from(id.id()),
    ];
    match health {
        TileHealth::Up {
            version: Some(version),
        } => lines.push(Line::from(format!("v{version}"))),
        TileHealth::Degraded { reason } | TileHealth::Down { reason } => {
            lines.push(Line::from(reason.clone()))
        }
        _ => {}
    }
    if id == ServiceId::Mpm {
        lines.push(Line::from("Enter: projects"));
    }
    lines
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
        _ => {}
    }
    if let Ok(socket) = id.socket() {
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
