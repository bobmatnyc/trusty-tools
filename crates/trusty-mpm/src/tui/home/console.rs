//! The console look of the home screen: header, MACHINE STATUS, SERVICES.
//!
//! Why (#10966, goal ar-G01): `tm tui` should read like trusty-console's
//! Overview tab. Drawing takes a plain view model, so the live data and a
//! filled model go through the same code.
//! What: [`ConsoleModel`] holds what the screen shows; [`model_from_state`]
//! builds it from the live [`HomeState`]: probe results for the rows, and the
//! `console.machine_status` / `console.history` feeds for the machine cards
//! and the per-service history (#10942, #10943, via `machine_cards`).
//! [`render_header`] and [`render_body`] draw it. Layout:
//! `docs/design/UI/tm-tui-console-look.md` section 3.
//! Test: `console_home_shows_no_data_yet_for_machine_and_history`,
//! `a_filled_model_draws_cards_and_sparklines`.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Sparkline, Table, TableState},
};

use super::machine_cards::{TITLES, history_note, machine_model, service_figures};
use super::state::{HomeState, TileHealth};
use crate::tui::VERSION_LABEL;
use crate::tui::theme::{Tone, badge, palette};

/// The console's identity line (`consoleVersion.js::CONSOLE_DESCRIPTOR`).
pub const DESCRIPTOR: &str = "UNIT-05 · SERVICE CONSOLE";
/// What a panel with no source shows.
pub const NO_DATA: &str = "no data yet";
/// Height of one machine card, borders included.
const CARD_HEIGHT: u16 = 7;
/// Below this width the two history columns drop.
const HISTORY_MIN_WIDTH: u16 = 100;

/// One machine-status card.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// `CPU`, `MEMORY`, `DISK` or `NETWORK`.
    pub title: &'static str,
    /// The big value line.
    pub value: String,
    /// The muted line under it.
    pub meta: String,
    /// The badge word and tone.
    pub badge: (String, Tone),
    /// The sparkline samples; `None` is a gap, never zero (console rule).
    pub samples: Vec<Option<u64>>,
    /// The sparkline's ceiling; `None` scales to the window's own maximum.
    pub max: Option<u64>,
    /// What the graph row says when `samples` is empty (#10942).
    pub graph_note: String,
}

/// MACHINE STATUS: the section badge and four cards.
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    /// The section badge (`NOMINAL`, …) and tone.
    pub badge: (String, Tone),
    /// CPU, MEMORY, DISK, NETWORK.
    pub cards: Vec<Card>,
}

/// One SERVICES row.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceRow {
    /// Display name.
    pub name: String,
    /// Version, or `-`.
    pub version: String,
    /// Status word and tone.
    pub status: (String, Tone),
    /// `%CPU` now, when a source exists.
    pub cpu: Option<String>,
    /// `MEM` now, when a source exists.
    pub mem: Option<String>,
    /// CPU history (last 10 min), when a source exists.
    pub cpu_history: Option<Vec<Option<u64>>>,
    /// MEM history (last 10 min), when a source exists.
    pub mem_history: Option<Vec<Option<u64>>>,
}

/// Everything the console home body draws.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleModel {
    /// `None` until `console.machine_status` has been asked (#10942).
    pub machine: Option<Machine>,
    /// One row per service, in roster order.
    pub rows: Vec<ServiceRow>,
    /// What a history cell with no series says (#10943).
    pub history_note: String,
    /// The selected row.
    pub selected: usize,
}

/// The status word and tone a probe result shows as.
///
/// Test: `no_uds_endpoint_rows_are_muted_never_down`.
pub fn status_of(health: &TileHealth) -> (String, Tone) {
    let tone = match health {
        TileHealth::Up { .. } => Tone::Ok,
        TileHealth::OnDemand => Tone::Info,
        TileHealth::Degraded { .. } => Tone::Degraded,
        TileHealth::Timeout => Tone::Warn,
        TileHealth::Down { .. } => Tone::Err,
        // See #10899: not UDS-reachable is neutral, never the red of Down.
        TileHealth::Absent | TileHealth::Pending | TileHealth::NoUdsEndpoint { .. } => Tone::Muted,
    };
    let word = match health {
        TileHealth::Up { .. } => "running",
        other => other.label(),
    };
    (word.to_string(), tone)
}

/// The live model: probe results for the rows, the console feeds for the
/// machine cards and the per-service history.
///
/// Test: `console_home_shows_no_data_yet_for_machine_and_history`,
/// `a_ready_feed_fills_every_card_with_live_values`.
pub fn model_from_state(state: &HomeState) -> ConsoleModel {
    let history = &state.machine.history;
    let rows = state
        .tiles
        .iter()
        .map(|(id, health)| {
            // #10943: %CPU, MEM and their history from `service_samples`.
            let figures = service_figures(history, id.id());
            ServiceRow {
                name: id.display_name().to_string(),
                version: match health {
                    TileHealth::Up { version: Some(v) } => v.clone(),
                    _ => "-".to_string(),
                },
                status: status_of(health),
                cpu: figures.cpu,
                mem: figures.mem,
                cpu_history: figures.cpu_history,
                mem_history: figures.mem_history,
            }
        })
        .collect();
    ConsoleModel {
        // #10942: the cards come from `console.machine_status` over UDS.
        machine: machine_model(&state.machine),
        rows,
        selected: state.selected,
        history_note: history_note(history),
    }
}

/// `TRUSTY CONSOLE  UNIT-05 · SERVICE CONSOLE · tm X`.
pub fn render_header(frame: &mut Frame, area: Rect) {
    let p = palette();
    let line = Line::from(vec![
        Span::styled(
            "TRUSTY CONSOLE",
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{DESCRIPTOR} · {VERSION_LABEL}"),
            Style::default().fg(p.muted),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// A section title on the left and an optional badge on the right.
fn section_title(frame: &mut Frame, area: Rect, title: &str, badge_of: Option<(String, Tone)>) {
    let p = palette();
    frame.render_widget(
        Paragraph::new(Span::styled(
            title.to_string(),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )),
        area,
    );
    if let Some((word, tone)) = badge_of {
        let width = word.chars().count() as u16 + 2;
        let x = area.right().saturating_sub(width);
        frame.render_widget(
            Paragraph::new(Line::from(badge(&word, tone))),
            Rect::new(x, area.y, width.min(area.width), 1),
        );
    }
}

/// MACHINE STATUS, then SERVICES, into `area`.
///
/// Test: `console_home_shows_no_data_yet_for_machine_and_history`.
pub fn render_body(frame: &mut Frame, area: Rect, model: &ConsoleModel) {
    let [machine_title, cards, _gap, services_title, table] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(CARD_HEIGHT),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(2),
        ])
        .areas(area);
    let section_badge = match &model.machine {
        Some(m) => m.badge.clone(),
        None => ("no data".to_string(), Tone::Muted),
    };
    section_title(frame, machine_title, "MACHINE STATUS", Some(section_badge));
    render_cards(frame, cards, model.machine.as_ref());
    section_title(frame, services_title, "SERVICES", None);
    render_services(frame, table, model);
}

/// The four cards; with no source each says so.
fn render_cards(frame: &mut Frame, area: Rect, machine: Option<&Machine>) {
    let placeholder: Vec<Card> = TITLES
        .into_iter()
        .map(|title| Card {
            title,
            value: NO_DATA.to_string(),
            meta: "waiting for console.machine_status".to_string(),
            badge: ("no data".to_string(), Tone::Muted),
            samples: Vec::new(),
            max: None,
            graph_note: String::new(),
        })
        .collect();
    let cards = machine.map(|m| m.cards.as_slice()).unwrap_or(&placeholder);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, 4); 4])
        .split(area);
    for (card, col) in cards.iter().zip(cols.iter()) {
        render_card(frame, *col, card);
    }
}

fn render_card(frame: &mut Frame, area: Rect, card: &Card) {
    let p = palette();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.muted))
        .style(Style::default().bg(p.surface))
        .title(Span::styled(
            format!(" {} ", card.title),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))
        .title(Line::from(badge(&card.badge.0, card.badge.1)).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [value, meta, graph] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(inner);
    let empty = card.samples.is_empty();
    let value_style = if empty {
        Style::default().fg(p.muted)
    } else {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    };
    frame.render_widget(Paragraph::new(card.value.clone()).style(value_style), value);
    frame.render_widget(
        Paragraph::new(card.meta.clone()).style(Style::default().fg(p.muted)),
        meta,
    );
    if empty {
        frame.render_widget(
            Paragraph::new(card.graph_note.clone()).style(Style::default().fg(p.muted)),
            graph,
        );
    } else {
        frame.render_widget(
            sparkline(&card.samples, card.max, graph, p.sparkline),
            graph,
        );
    }
}

/// A sparkline in `color` over the newest samples that fit `area`; `None`
/// samples draw as gaps. `max` fixes the ceiling (a percentage's 100%).
fn sparkline(
    samples: &[Option<u64>],
    max: Option<u64>,
    area: Rect,
    color: ratatui::style::Color,
) -> Sparkline<'static> {
    // #10942: ratatui draws from the left, so keep the newest `width` samples.
    let tail = samples.len().saturating_sub(usize::from(area.width));
    let spark = Sparkline::default()
        .data(samples[tail..].to_vec())
        .style(Style::default().fg(color))
        .absent_value_symbol(" ");
    match max {
        Some(max) => spark.max(max),
        None => spark,
    }
}

/// The SERVICES table; history sparklines are drawn into their cells after.
fn render_services(frame: &mut Frame, area: Rect, model: &ConsoleModel) {
    let p = palette();
    let history = area.width >= HISTORY_MIN_WIDTH;
    let mut widths = vec![
        Constraint::Length(12),
        Constraint::Length(10),
        // Fits the widest badge, ` NO UDS ENDPOINT `.
        Constraint::Length(17),
        // #10943: fits `unknown`.
        Constraint::Length(7),
        Constraint::Length(9),
    ];
    let mut header = vec!["Service", "Version", "Status", "%CPU", "MEM"];
    if history {
        widths.extend([Constraint::Fill(1), Constraint::Fill(1)]);
        header.extend(["CPU · last 10 min", "MEM · last 10 min"]);
    }
    let dash = || "—".to_string();
    let rows = model.rows.iter().map(|r| {
        let mut cells = vec![
            Cell::from(r.name.clone()),
            Cell::from(r.version.clone()),
            Cell::from(Line::from(badge(&r.status.0, r.status.1))),
            Cell::from(r.cpu.clone().unwrap_or_else(dash)),
            Cell::from(r.mem.clone().unwrap_or_else(dash)),
        ];
        if history {
            cells.extend([Cell::from(""), Cell::from("")]);
        }
        Row::new(cells)
    });
    let table = Table::new(rows, widths.clone())
        .header(Row::new(header).style(Style::default().fg(p.muted).add_modifier(Modifier::BOLD)))
        .column_spacing(2)
        .row_highlight_style(p.selection());
    let mut table_state = TableState::default().with_selected(Some(model.selected));
    frame.render_stateful_widget(table, area, &mut table_state);
    if history {
        render_history_cells(frame, area, &widths, model, table_state.offset());
    }
}

/// One sparkline (or a note saying why there is none) per row in each history column.
fn render_history_cells(
    frame: &mut Frame,
    area: Rect,
    widths: &[Constraint],
    model: &ConsoleModel,
    offset: usize,
) {
    let p = palette();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(widths.to_vec())
        .spacing(2)
        .split(area);
    let (cpu_col, mem_col) = (cols[5], cols[6]);
    for (i, row) in model.rows.iter().enumerate().skip(offset) {
        let y = area.y + 1 + (i - offset) as u16;
        if y >= area.bottom() {
            break;
        }
        let selected = i == model.selected;
        let ink = if selected {
            p.selection().fg.unwrap_or(p.sparkline)
        } else {
            p.sparkline
        };
        for (col, hist, max) in [
            (cpu_col, &row.cpu_history, Some(1000)),
            (mem_col, &row.mem_history, None),
        ] {
            let cell = Rect::new(col.x, y, col.width, 1);
            // #10943: a series draws; no series or no samples says why.
            let note = match hist {
                Some(samples) if !samples.is_empty() => {
                    frame.render_widget(sparkline(samples, max, cell, ink), cell);
                    continue;
                }
                Some(_) => "no samples yet",
                None => model.history_note.as_str(),
            };
            let style = if selected {
                p.selection()
            } else {
                Style::default().fg(p.muted)
            };
            frame.render_widget(Paragraph::new(note.to_string()).style(style), cell);
        }
    }
}
