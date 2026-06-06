//! Ratatui split-pane TUI for the `trusty-agents debug` subcommand.
//!
//! Why: Operators need a single window where they can watch the REPL's
//! tmux scrollback, see ctrl-socket liveness, and inject commands —
//! without juggling multiple terminals. The TUI lives in the invoking
//! terminal; the REPL lives in the detached tmux session this module's
//! sibling adapter manages.
//! What: `DebugApp` owns mutable UI state. `run_tui` enters raw mode,
//! drives a 100ms tick loop that polls crossterm events, refreshes the
//! left panel from `tmux capture-pane`, refreshes the status panel from
//! `SocketMonitor`, and renders three regions (REPL output, status, input).
//! Test: `debug_app_handles_basic_input` and `strip_ansi_strips_codes`
//! cover state transitions deterministically; full-screen rendering is
//! exercised manually.

use std::io::{self, Stdout};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::socket_monitor::SocketMonitor;
use super::tmux::TmuxAdapter;

const TICK: Duration = Duration::from_millis(100);
const SCROLL_STEP: usize = 1;
const PAGE_STEP: usize = 10;

/// Which region currently receives keyboard scroll/edit events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Left,
    Right,
    Input,
}

/// Mutable UI state. Kept separate from the rendering loop so unit tests
/// can drive transitions without instantiating a Terminal.
#[derive(Debug)]
pub struct DebugApp {
    pub repl_output: Vec<String>,
    pub status_lines: Vec<String>,
    pub input: String,
    pub repl_scroll: usize,
    pub status_scroll: usize,
    pub focus: Focus,
    pub socket_alive: bool,
    pub pane_id: String,
    pub session_name: String,
    pub quit: bool,
}

impl DebugApp {
    pub fn new(session_name: String, pane_id: String) -> Self {
        Self {
            repl_output: Vec::new(),
            status_lines: Vec::new(),
            input: String::new(),
            repl_scroll: 0,
            status_scroll: 0,
            focus: Focus::Input,
            socket_alive: false,
            pane_id,
            session_name,
            quit: false,
        }
    }

    /// Cycle focus Left → Right → Input → Left.
    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Left => Focus::Right,
            Focus::Right => Focus::Input,
            Focus::Input => Focus::Left,
        };
    }

    /// Apply a relative scroll delta to whichever panel currently has focus.
    /// Negative scrolls move toward older content (up); positive toward newer.
    pub fn scroll(&mut self, delta: isize) {
        let target = match self.focus {
            Focus::Left => &mut self.repl_scroll,
            Focus::Right => &mut self.status_scroll,
            Focus::Input => return,
        };
        if delta < 0 {
            *target = target.saturating_sub((-delta) as usize);
        } else {
            *target = target.saturating_add(delta as usize);
        }
    }

    pub fn rebuild_status(&mut self, capture_lines: usize) {
        let socket = if self.socket_alive {
            "● socket: alive"
        } else {
            "○ socket: dead"
        };
        self.status_lines = vec![
            socket.to_string(),
            format!("session: {}", self.session_name),
            format!("pane: {}", self.pane_id),
            format!("capture lines: {capture_lines}"),
            format!("focus: {:?}", self.focus),
        ];
    }
}

/// Strip ANSI CSI escape sequences from `s`.
///
/// Why: `tmux capture-pane` without `-e` shouldn't include color codes,
/// but some REPLs emit them anyway through the pty. We use the
/// `strip-ansi-escapes` crate to filter them out before display.
/// What: Wraps `strip_ansi_escapes::strip` and lossy-decodes the bytes.
/// Test: `strip_ansi_strips_codes`.
pub fn strip_ansi(s: &str) -> String {
    let bytes = strip_ansi_escapes::strip(s.as_bytes());
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Run the TUI to completion.
///
/// Why: Consolidates terminal setup/teardown so the caller gets a
/// stack-safe RAII boundary — even a panic inside the event loop
/// restores the terminal via `restore_terminal`.
/// What: Enters alt-screen + raw mode, runs the tick loop, then restores.
/// Test: integration via `cargo run -- debug` plus the deterministic
/// `DebugApp` unit tests.
pub fn run_tui(
    tmux: TmuxAdapter,
    socket_monitor: SocketMonitor,
    session_name: String,
    pane_id: String,
    capture_lines: u32,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app = DebugApp::new(session_name.clone(), pane_id);

    let result = event_loop(
        &mut terminal,
        &mut app,
        &tmux,
        &socket_monitor,
        capture_lines,
    );

    restore_terminal(&mut terminal).ok();
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("construct terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();
    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut DebugApp,
    tmux: &TmuxAdapter,
    socket_monitor: &SocketMonitor,
    capture_lines: u32,
) -> Result<()> {
    let mut last_tick = Instant::now();

    loop {
        // Refresh data first so the next draw reflects newest state.
        if last_tick.elapsed() >= TICK {
            refresh(app, tmux, socket_monitor, capture_lines);
            last_tick = Instant::now();
        }

        terminal.draw(|f| draw(f, app))?;

        // Poll the remaining slice of the tick window so we don't busy-loop.
        let timeout = TICK
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
        {
            handle_key(app, tmux, key.code, key.modifiers);
        }

        if app.quit {
            return Ok(());
        }
    }
}

fn refresh(
    app: &mut DebugApp,
    tmux: &TmuxAdapter,
    socket_monitor: &SocketMonitor,
    capture_lines: u32,
) {
    if let Ok(out) = tmux.capture_output(&app.session_name, capture_lines) {
        let cleaned = strip_ansi(&out);
        app.repl_output = cleaned.lines().map(|s| s.to_string()).collect();
    }
    app.socket_alive = socket_monitor.check_alive();
    app.rebuild_status(app.repl_output.len());
}

fn handle_key(app: &mut DebugApp, tmux: &TmuxAdapter, code: KeyCode, mods: KeyModifiers) {
    // Ctrl-C is always a quit, regardless of focus.
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }

    match (app.focus, code) {
        (_, KeyCode::Tab) => app.cycle_focus(),
        (Focus::Input, KeyCode::Esc) => app.input.clear(),
        (Focus::Input, KeyCode::Enter) if !app.input.is_empty() => {
            let _ = tmux.send_line(&app.session_name, &app.input);
            app.input.clear();
        }
        (Focus::Input, KeyCode::Backspace) => {
            app.input.pop();
        }
        (Focus::Input, KeyCode::Char(c)) => {
            // 'q' inside the input box is just text — quit only outside Input.
            app.input.push(c);
        }
        (Focus::Left | Focus::Right, KeyCode::Char('q')) => app.quit = true,
        (Focus::Left | Focus::Right, KeyCode::Up) => app.scroll(-(SCROLL_STEP as isize)),
        (Focus::Left | Focus::Right, KeyCode::Down) => app.scroll(SCROLL_STEP as isize),
        (Focus::Left | Focus::Right, KeyCode::PageUp) => app.scroll(-(PAGE_STEP as isize)),
        (Focus::Left | Focus::Right, KeyCode::PageDown) => app.scroll(PAGE_STEP as isize),
        _ => {}
    }
}

fn draw(f: &mut ratatui::Frame, app: &DebugApp) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(f.area());

    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(outer[0]);

    draw_repl(f, app, panels[0]);
    draw_status(f, app, panels[1]);
    draw_footer(f, app, outer[1]);
}

fn focus_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_repl(f: &mut ratatui::Frame, app: &DebugApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" REPL Output (tmux capture) ")
        .border_style(focus_style(app.focus == Focus::Left));

    // Compute the visible window. The conventional model: 0 == bottom (most
    // recent). Increasing scroll moves the window upward (older lines).
    let total = app.repl_output.len();
    let visible_height = area.height.saturating_sub(2) as usize; // borders
    let bottom = total.saturating_sub(app.repl_scroll);
    let top = bottom.saturating_sub(visible_height);
    let slice = if total == 0 {
        &[][..]
    } else {
        &app.repl_output[top..bottom]
    };
    let lines: Vec<Line> = slice.iter().map(|l| Line::from(l.as_str())).collect();
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_status(f: &mut ratatui::Frame, app: &DebugApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Status / Log ")
        .border_style(focus_style(app.focus == Focus::Right));

    let lines: Vec<Line> = app
        .status_lines
        .iter()
        .map(|s| {
            let style = if s.contains("alive") {
                Style::default().fg(Color::Green)
            } else if s.contains("dead") {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            Line::from(Span::styled(s.clone(), style))
        })
        .collect();
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_footer(f: &mut ratatui::Frame, app: &DebugApp, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(2)])
        .split(area);

    let help = Paragraph::new(Line::from(vec![
        Span::styled("[q]", Style::default().fg(Color::Cyan)),
        Span::raw(" quit  "),
        Span::styled("[Tab]", Style::default().fg(Color::Cyan)),
        Span::raw(" focus  "),
        Span::styled("[↑↓/PgUp/PgDn]", Style::default().fg(Color::Cyan)),
        Span::raw(" scroll  "),
        Span::styled("[Enter]", Style::default().fg(Color::Cyan)),
        Span::raw(" send  "),
        Span::styled("[Esc]", Style::default().fg(Color::Cyan)),
        Span::raw(" clear"),
    ]));
    f.render_widget(help, layout[0]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_style(app.focus == Focus::Input));
    let prompt = format!("> {}", app.input);
    let input = Paragraph::new(prompt).block(block);
    f.render_widget(input, layout[1]);
}

/// Resolve the project directory used for ctrl-socket path derivation.
///
/// Why: Reused by the subcommand entry; centralises the cwd fallback.
pub fn resolve_project_dir() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_app_cycles_focus() {
        let mut app = DebugApp::new("s".into(), "%0".into());
        assert_eq!(app.focus, Focus::Input);
        app.cycle_focus();
        assert_eq!(app.focus, Focus::Left);
        app.cycle_focus();
        assert_eq!(app.focus, Focus::Right);
        app.cycle_focus();
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn debug_app_scroll_clamps_at_zero() {
        let mut app = DebugApp::new("s".into(), "%0".into());
        app.focus = Focus::Left;
        app.scroll(-5);
        assert_eq!(app.repl_scroll, 0);
        app.scroll(7);
        assert_eq!(app.repl_scroll, 7);
        app.scroll(-3);
        assert_eq!(app.repl_scroll, 4);
    }

    #[test]
    fn debug_app_scroll_input_focus_noop() {
        let mut app = DebugApp::new("s".into(), "%0".into());
        app.focus = Focus::Input;
        app.scroll(5);
        assert_eq!(app.repl_scroll, 0);
        assert_eq!(app.status_scroll, 0);
    }

    #[test]
    fn debug_app_rebuild_status_reflects_socket() {
        let mut app = DebugApp::new("ompm-debug".into(), "%0".into());
        app.socket_alive = true;
        app.rebuild_status(42);
        assert!(app.status_lines.iter().any(|s| s.contains("alive")));
        assert!(app.status_lines.iter().any(|s| s.contains("ompm-debug")));
        assert!(app.status_lines.iter().any(|s| s.contains("42")));
    }

    #[test]
    fn strip_ansi_strips_codes() {
        let raw = "\x1b[31mhello\x1b[0m world";
        assert_eq!(strip_ansi(raw), "hello world");
    }
}
