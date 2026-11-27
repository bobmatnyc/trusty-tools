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
use serde_json::{Map, Value};
use trusty_common::console_metrics::BY_PREFIX;

use super::console::{model_from_state, render_body, render_header};
use super::services::ServiceId;
use super::state::{HomeState, MetricsState, TileHealth, View};
use crate::tui::VERSION_LABEL;
use crate::tui::architect::render::ARCHITECT_HINT;

/// Footer on the home screen.
pub const HOME_HINT: &str =
    "[↑↓] select  [Enter] open  [i] info  [a] architect  [v] live  [q] quit";
/// Footer on a service view.
pub const SERVICE_HINT: &str = "[Esc] home  [q] quit";
/// Most metrics lines a service view lists.
const MAX_METRIC_LINES: usize = 40;
// #10908: breakdowns render as nested top-N tables, not "{N fields}".
/// Most entries one `by_*` table lists before its `… N more` row.
const MAX_BREAKDOWN_ROWS: usize = 5;
/// Deepest indent level an object expands at; deeper ones are summarised.
const MAX_METRIC_DEPTH: usize = 6;

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

/// The text lines of a report's `metrics` payload.
///
/// Why: #10908 — a `by_*` breakdown read "{N fields}", hiding the counts the
/// service view exists to show.
/// What: plain fields come first, then breakdowns, each group sorted by key so
/// the output does not depend on `serde_json`'s map order. A plain object
/// expands one indent deeper; a `by_*` object becomes a table (see
/// [`breakdown_lines`]). Objects deeper than [`MAX_METRIC_DEPTH`] are
/// summarised, and the whole list stops at [`MAX_METRIC_LINES`].
/// Test: `by_breakdowns_render_as_nested_tables_snapshot`,
/// `a_long_breakdown_keeps_the_top_entries_and_counts_the_rest`,
/// `service_view_draws_breakdown_rows`.
pub fn metric_lines(metrics: &Value) -> Vec<String> {
    let Some(object) = metrics.as_object() else {
        return vec![format!("  {metrics}")];
    };
    let mut lines = Vec::new();
    field_lines(object.iter().collect(), 1, &mut lines);
    lines.truncate(MAX_METRIC_LINES);
    lines
}

/// Whether `key: value` is a `by_*` breakdown (convention rule 1).
fn is_breakdown(key: &str, value: &Value) -> bool {
    key.starts_with(BY_PREFIX) && value.is_object()
}

/// A scalar as text: a string without its quotes, anything else as JSON.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Lines for a set of fields at `depth` (two spaces per level).
fn field_lines(mut fields: Vec<(&String, &Value)>, depth: usize, out: &mut Vec<String>) {
    fields.sort_by_key(|(key, value)| (is_breakdown(key, value), key.as_str()));
    let indent = "  ".repeat(depth);
    for (key, value) in fields {
        match value {
            Value::Object(map) if depth > MAX_METRIC_DEPTH => {
                out.push(format!("{indent}{key}: {{{} fields}}", map.len()))
            }
            Value::Object(map) if is_breakdown(key, value) => breakdown_lines(key, map, depth, out),
            Value::Object(map) if map.is_empty() => out.push(format!("{indent}{key}: {{}}")),
            Value::Object(map) => {
                out.push(format!("{indent}{key}:"));
                field_lines(map.iter().collect(), depth + 1, out);
            }
            Value::Array(items) => out.push(format!("{indent}{key}: [{} items]", items.len())),
            other => out.push(format!("{indent}{key}: {}", scalar_text(other))),
        }
    }
}

/// An entry's rank in its breakdown: its count, or a body's largest count.
fn entry_rank(value: &Value) -> u64 {
    match value {
        Value::Object(body) => body.values().filter_map(Value::as_u64).max().unwrap_or(0),
        other => other.as_u64().unwrap_or(0),
    }
}

/// One `by_*` breakdown as a table of its top [`MAX_BREAKDOWN_ROWS`] entries.
///
/// What: a `key (N):` heading, then one row per entry ranked by
/// [`entry_rank`] high to low, ties by key. A count row right-aligns the
/// count; a body row lists the body's scalars as `name=value`, and its object
/// fields indent under it. Entries past the limit are counted on a `… N more`
/// row; an empty breakdown reads `(none)`.
/// Test: `by_breakdowns_render_as_nested_tables_snapshot`,
/// `a_long_breakdown_keeps_the_top_entries_and_counts_the_rest`.
fn breakdown_lines(key: &str, map: &Map<String, Value>, depth: usize, out: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    if map.is_empty() {
        out.push(format!("{indent}{key}: (none)"));
        return;
    }
    out.push(format!("{indent}{key} ({}):", map.len()));
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by(|(ka, va), (kb, vb)| entry_rank(vb).cmp(&entry_rank(va)).then(ka.cmp(kb)));
    let shown = &entries[..entries.len().min(MAX_BREAKDOWN_ROWS)];
    let key_width = shown
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let count_width = shown
        .iter()
        .filter(|(_, v)| !v.is_object())
        .map(|(_, v)| scalar_text(v).chars().count())
        .max()
        .unwrap_or(0);
    for (name, value) in shown {
        let Value::Object(body) = value else {
            let count = scalar_text(value);
            out.push(format!(
                "{indent}  {name:<key_width$}  {count:>count_width$}"
            ));
            continue;
        };
        let mut scalars: Vec<(&String, &Value)> = body
            .iter()
            .filter(|(_, v)| !v.is_object() && !v.is_array())
            .collect();
        scalars.sort_by_key(|(k, _)| k.as_str());
        let row: Vec<String> = scalars
            .iter()
            .map(|(k, v)| format!("{k}={}", scalar_text(v)))
            .collect();
        let line = format!("{indent}  {name:<key_width$}  {}", row.join("  "));
        out.push(line.trim_end().to_string());
        let nested = body
            .iter()
            .filter(|(_, v)| v.is_object() || v.is_array())
            .collect();
        field_lines(nested, depth + 2, out);
    }
    if entries.len() > shown.len() {
        out.push(format!("{indent}  … {} more", entries.len() - shown.len()));
    }
}
