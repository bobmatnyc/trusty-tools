//! Coordinator dashboard rendering.
//!
//! Why: the TUI's primary surface is the cross-session *coordinator chat* — a
//! conversational pane that has visibility into every active Claude Code
//! session, with a dismissable session sidebar beside it. Keeping the pure
//! layout/rendering logic here (separate from the event loop and HTTP polling)
//! makes the line-building unit-testable without a terminal.
//! What: [`DashboardState`] holds the polled session rows, the chat transcript,
//! and a CMD> input bar; [`render`] draws the ratatui frame; the `*_lines`
//! helpers build the text the test suite asserts on.
//! Test: `cargo test -p trusty-mpm-tui` checks chat/session formatting and the
//! empty state without a terminal.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};

use super::client::SessionRow;

/// One-line key hint shown in the status bar.
pub const KEY_HINT: &str =
    "keys: ↵ send | s sidebar | Tab focus | ↑↓ scroll/select | ? help | q quit";

/// Maximum number of executed messages kept in the input-bar history.
pub const COMMAND_HISTORY_LIMIT: usize = 20;

/// The CMD> input bar pinned to the bottom of the dashboard.
///
/// Why: the operator types coordinator messages into one always-visible
/// editable input line; this struct holds the edit buffer plus a ↑/↓ recall
/// ring so a recent message can be re-sent without retyping.
/// What: `input` is the current edit buffer; `history` is the bounded ring of
/// recently sent messages; `history_cursor` tracks ↑/↓ recall.
/// Test: `command_bar_*` unit tests cover editing and history.
#[derive(Debug, Clone, Default)]
pub struct CommandBar {
    /// The current edit buffer.
    pub input: String,
    /// Recently sent messages, newest last, capped at the history limit.
    pub history: Vec<String>,
    /// Index into [`Self::history`] while recalling with ↑/↓; `None` = live input.
    history_cursor: Option<usize>,
}

impl CommandBar {
    /// Append a character to the input buffer.
    ///
    /// Why: printable keystrokes build up the coordinator message.
    /// What: pushes `c` and resets the history cursor so the next ↑ starts fresh.
    /// Test: `command_bar_edits_buffer`.
    pub fn push(&mut self, c: char) {
        self.input.push(c);
        self.history_cursor = None;
    }

    /// Delete the last character of the input buffer (the Backspace key).
    ///
    /// Why: Backspace must edit a mistyped message.
    /// What: pops the trailing character; resets the recall cursor.
    /// Test: `command_bar_edits_buffer`.
    pub fn backspace(&mut self) {
        self.input.pop();
        self.history_cursor = None;
    }

    /// Clear the input buffer (the Esc key).
    ///
    /// Why: Esc abandons a half-typed message.
    /// What: empties [`Self::input`] and resets the recall cursor.
    /// Test: `command_bar_clear_empties_input`.
    pub fn clear(&mut self) {
        self.input.clear();
        self.history_cursor = None;
    }

    /// Recall the previous message from history (the ↑ key).
    ///
    /// Why: re-sending or editing a recent message should not require retyping.
    /// What: moves the history cursor one step toward the oldest entry and loads
    /// it into the buffer; a no-op when history is empty.
    /// Test: `command_bar_history_recall`.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_cursor = Some(next);
        self.input = self.history[next].clone();
    }

    /// Recall the next (newer) message from history (the ↓ key).
    ///
    /// Why: lets the operator step back down after recalling too far with ↑.
    /// What: advances the cursor toward the newest entry; stepping past the
    /// newest clears the cursor and empties the buffer.
    /// Test: `command_bar_history_recall`.
    pub fn history_next(&mut self) {
        let Some(i) = self.history_cursor else {
            return;
        };
        if i + 1 >= self.history.len() {
            self.history_cursor = None;
            self.input.clear();
        } else {
            self.history_cursor = Some(i + 1);
            self.input = self.history[i + 1].clone();
        }
    }

    /// Take the typed message for sending, recording it in history.
    ///
    /// Why: pressing Enter sends the message; the loop needs the buffer's text
    /// and the bar needs the message pushed onto its bounded history.
    /// What: returns the trimmed buffer, clears the input, appends a non-empty
    /// message to [`Self::history`] (dropping the oldest beyond the limit), and
    /// resets the recall cursor.
    /// Test: `command_bar_submit_records_history`.
    pub fn take_for_execution(&mut self) -> String {
        let typed = std::mem::take(&mut self.input);
        self.history_cursor = None;
        let trimmed = typed.trim().to_string();
        if !trimmed.is_empty() {
            self.history.push(trimmed.clone());
            if self.history.len() > COMMAND_HISTORY_LIMIT {
                let overflow = self.history.len() - COMMAND_HISTORY_LIMIT;
                self.history.drain(0..overflow);
            }
        }
        trimmed
    }
}

/// Who authored a coordinator-chat message.
///
/// Why: the chat pane renders user and coordinator turns differently (a
/// `[user]` / `[coord]` prefix and a distinct colour); a typed role keeps that
/// rendering exhaustive.
/// What: `User` for the operator's messages, `Coordinator` for replies.
/// Test: `chat_message_lines_prefix_role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// A message typed by the operator.
    User,
    /// A reply (or routed-command output) from the coordinator.
    Coordinator,
}

/// One message in the coordinator chat transcript.
///
/// Why: the chat pane is a scrollable list of past turns; each turn needs its
/// author and its (possibly multi-line) text.
/// What: a [`ChatRole`] and the message `content`.
/// Test: `chat_message_lines_prefix_role`.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Who authored this message.
    pub role: ChatRole,
    /// The message text (may contain newlines).
    pub content: String,
}

impl ChatMessage {
    /// A user-authored chat message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    /// A coordinator-authored chat message.
    pub fn coordinator(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Coordinator,
            content: content.into(),
        }
    }
}

/// Which pane currently has keyboard focus.
///
/// Why: `Tab` switches focus between the CMD input bar and the session sidebar;
/// arrow keys then either scroll chat / select a session depending on focus.
/// What: `Input` (the default) routes ↑/↓ to chat scroll; `Sidebar` routes
/// ↑/↓ to session selection.
/// Test: `tab_toggles_focus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The CMD> input bar has focus (typing, Enter send, ↑/↓ scroll chat).
    #[default]
    Input,
    /// The session sidebar has focus (↑/↓ select a session).
    Sidebar,
}

/// Snapshot of everything the dashboard renders this frame.
///
/// Why: the event loop polls the daemon, fills this struct, and hands it to
/// `render` — a clean data/render split.
/// What: the session list, the coordinator chat transcript, and a
/// daemon-reachable flag.
/// Test: `chat_message_lines_prefix_role`, `sidebar_starts_visible_with_sessions`.
#[derive(Debug, Clone, Default)]
pub struct DashboardState {
    /// Sessions reported by the daemon.
    pub sessions: Vec<SessionRow>,
    /// Whether the last daemon poll succeeded.
    pub daemon_reachable: bool,
    /// The coordinator chat transcript, oldest first.
    pub chat_history: Vec<ChatMessage>,
    /// Whether the session sidebar is visible (toggled with `s`).
    pub sidebar_visible: bool,
    /// Scroll offset (in transcript lines from the top) into the chat pane.
    pub chat_scroll: usize,
    /// Which pane currently holds keyboard focus.
    pub focus: Focus,
    /// Index into [`Self::sessions`] of the highlighted sidebar row.
    pub selected_session: usize,
    /// Human-readable result of the last user action, shown in the status bar.
    pub last_action: Option<String>,
    /// Whether the help overlay is currently visible (toggled with `?`).
    pub show_help: bool,
    /// The CMD> input bar.
    pub command_bar: CommandBar,
    /// Rolling LLM chat history threaded through the coordinator endpoint.
    ///
    /// Why: `POST /api/v1/coordinator/chat` is stateless; the TUI holds the
    /// conversation window so successive messages form one conversation.
    pub coord_history: Vec<crate::client::ChatMessage>,
    /// Whether the event loop should exit.
    pub should_exit: bool,
}

impl DashboardState {
    /// Clamp [`Self::selected_session`] into the current session bounds.
    ///
    /// Why: the session list shrinks between polls (sessions end); a stale
    /// selection index would index out of bounds.
    /// What: pins the index to `sessions.len() - 1`, or `0` when empty.
    /// Test: `selection_clamps_to_bounds`.
    pub fn clamp_selection(&mut self) {
        let max = self.sessions.len().saturating_sub(1);
        if self.selected_session > max {
            self.selected_session = max;
        }
    }

    /// Move the session selection up one row (saturating at the top).
    pub fn select_up(&mut self) {
        self.selected_session = self.selected_session.saturating_sub(1);
        self.clamp_selection();
    }

    /// Move the session selection down one row (saturating at the bottom).
    pub fn select_down(&mut self) {
        let max = self.sessions.len().saturating_sub(1);
        if self.selected_session < max {
            self.selected_session += 1;
        }
    }

    /// The friendly `tmux_name` of the currently-selected session, if any.
    ///
    /// Why: pressing Enter on the sidebar prefills the input with the session's
    /// routing prefix; callers need the selected row's name.
    /// What: returns `None` when there are no sessions.
    /// Test: `selected_target_returns_none_when_empty`.
    pub fn selected_target(&self) -> Option<String> {
        self.sessions
            .get(self.selected_session)
            .map(|s| s.tmux_name.clone())
    }

    /// Toggle the session sidebar's visibility (the `s` key).
    ///
    /// Why: when hidden, the coordinator chat pane gets the full terminal width.
    /// What: flips [`Self::sidebar_visible`].
    /// Test: `toggle_sidebar_flips_visibility`.
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
    }

    /// Switch keyboard focus between the input bar and the sidebar (`Tab`).
    ///
    /// Why: `Tab` cycles focus; with the sidebar hidden, focus stays on input.
    /// What: flips [`Self::focus`] — but only to `Sidebar` when the sidebar is
    /// visible, so a hidden sidebar can never silently capture the arrow keys.
    /// Test: `tab_toggles_focus`, `tab_noop_when_sidebar_hidden`.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Input if self.sidebar_visible => Focus::Sidebar,
            _ => Focus::Input,
        };
    }

    /// Append a chat message to the transcript and scroll to the bottom.
    ///
    /// Why: every coordinator turn (user message, then reply) is appended here;
    /// new content should be visible, so the scroll snaps to the end.
    /// What: pushes `msg` and sets [`Self::chat_scroll`] past the transcript so
    /// the render clamps it to the last page.
    /// Test: `chat_history_grows_on_send`.
    pub fn push_chat(&mut self, msg: ChatMessage) {
        self.chat_history.push(msg);
        // A large value; the renderer clamps it to the real last page.
        self.chat_scroll = usize::MAX;
    }

    /// Scroll the chat transcript up one line (toward older messages).
    pub fn scroll_up(&mut self) {
        self.chat_scroll = self.chat_scroll.saturating_sub(1);
    }

    /// Scroll the chat transcript down one line (toward newer messages).
    pub fn scroll_down(&mut self) {
        self.chat_scroll = self.chat_scroll.saturating_add(1);
    }
}

/// Render a [`SessionStatus`] as a status indicator glyph.
///
/// Why: the sidebar shows a compact coloured indicator per session so the
/// operator's eye jumps to trouble; centralising the mapping keeps the row
/// builder readable and unit-testable.
/// What: `● Active` (green), `○ Paused` (yellow), `✕ Stopped`/other (red).
/// Test: `status_indicator_maps_each_status`.
pub fn status_indicator(status: crate::core::session::SessionStatus) -> (char, Color) {
    use crate::core::session::SessionStatus;
    match status {
        SessionStatus::Active | SessionStatus::Starting => ('●', Color::Green),
        SessionStatus::AwaitingApproval | SessionStatus::Paused | SessionStatus::Detached => {
            ('○', Color::Yellow)
        }
        SessionStatus::Stopped => ('✕', Color::Red),
    }
}

/// Derive a session's short routing prefix from its tmux name.
///
/// Why: the sidebar and the Enter-to-prefill flow address a session by its
/// short prefix (`aipowerranking`), not its full `tmpm-aipowerranking` name.
/// What: strips a leading `tmpm-` when present.
/// Test: `session_prefix_strips_tmpm`.
pub fn session_prefix(name: &str) -> &str {
    name.strip_prefix("tmpm-").unwrap_or(name)
}

/// Build the list items for the session sidebar.
///
/// Why: separating row construction from the ratatui `List` lets tests assert
/// the formatted text without a terminal backend; the `selected` index drives
/// the visible navigation highlight.
/// What: one item per session — a coloured status glyph then the session name;
/// the item at `selected` gets a bold blue background.
/// Test: `sidebar_items_format_each_session`.
pub fn sidebar_items(state: &DashboardState) -> Vec<ListItem<'static>> {
    state
        .sessions
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            let (glyph, color) = status_indicator(s.status);
            let name = if s.tmux_name.is_empty() {
                short_session(&s.id)
            } else {
                s.tmux_name.clone()
            };
            let line = Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::raw(name),
            ]);
            let item = ListItem::new(line);
            if idx == state.selected_session && state.focus == Focus::Sidebar {
                item.style(
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                item
            }
        })
        .collect()
}

/// Render a [`SessionId`] into a short, human id.
///
/// Why: a session with no friendly name is shown by the first 8 UUID chars.
/// What: truncates the UUID string to its first 8 characters.
/// Test: `short_session_extracts_prefix`.
pub(crate) fn short_session(id: &crate::core::session::SessionId) -> String {
    id.0.to_string().chars().take(8).collect()
}

/// Build the flat, wrapped lines of the coordinator chat transcript.
///
/// Why: the chat pane is a scrollable `List`; each transcript message may span
/// several lines, so it is flattened into one line vec the scroll offset can
/// index. Keeping it pure lets a test assert the `[user]` / `[coord]` prefixes
/// without a terminal.
/// What: one `(text, role)` pair per rendered line — the first line of a
/// message carries its `[user]` / `[coord]` prefix, continuation lines are
/// indented; an empty transcript yields a single placeholder line.
/// Test: `chat_message_lines_prefix_role`, `chat_lines_empty_placeholder`.
pub fn chat_lines(state: &DashboardState) -> Vec<(String, ChatRole)> {
    if state.chat_history.is_empty() {
        return vec![(
            "(no messages yet — type a question, or @session: to route a command)".to_string(),
            ChatRole::Coordinator,
        )];
    }
    let mut lines = Vec::new();
    for msg in &state.chat_history {
        let prefix = match msg.role {
            ChatRole::User => "[user] ",
            ChatRole::Coordinator => "[coord] ",
        };
        for (i, raw) in msg.content.lines().enumerate() {
            let text = if i == 0 {
                format!("{prefix}{raw}")
            } else {
                format!("        {raw}")
            };
            lines.push((text, msg.role));
        }
    }
    lines
}

/// Clamp a chat scroll offset so the last page of the transcript is visible.
///
/// Why: [`DashboardState::push_chat`] sets the scroll to `usize::MAX` to mean
/// "snap to bottom"; the renderer must turn that into a real offset bounded by
/// the transcript length and the visible height.
/// What: returns `min(scroll, total.saturating_sub(height))`.
/// Test: `clamp_scroll_bounds_to_last_page`.
pub fn clamp_scroll(scroll: usize, total: usize, height: usize) -> usize {
    let max = total.saturating_sub(height);
    scroll.min(max)
}

/// Pick the header-title style from the daemon's reachability.
///
/// Why: the title doubles as a health indicator — a calm cyan when reachable,
/// a loud reverse-video red banner when not, unmissable on any terminal theme.
/// What: bold cyan when reachable; bold + reversed red when unreachable.
/// Test: `title_style_signals_daemon_health`.
fn title_style(daemon_reachable: bool) -> Style {
    let base = Style::default()
        .fg(if daemon_reachable {
            Color::Cyan
        } else {
            Color::Red
        })
        .add_modifier(Modifier::BOLD);
    if daemon_reachable {
        base
    } else {
        base.add_modifier(Modifier::REVERSED)
    }
}

/// Build the status-bar line (header line 2).
///
/// Why: gives the operator feedback on the last action, or the key hint when
/// nothing has happened yet.
/// What: returns `last_action` if set, otherwise [`KEY_HINT`].
/// Test: `status_line_falls_back_to_key_hint`, `status_line_shows_last_action`.
pub fn status_line(state: &DashboardState) -> String {
    state
        .last_action
        .clone()
        .unwrap_or_else(|| KEY_HINT.to_string())
}

/// Build a styled panel title line for a bordered block.
fn panel_title(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Compute a centred sub-rectangle for the help overlay.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// The body text for the help overlay, one binding per line.
///
/// Why: kept separate so a test can assert every binding is documented.
/// What: returns the multi-line help string for the coordinator layout.
/// Test: `help_text_lists_all_bindings`.
pub fn help_text() -> String {
    [
        "  Enter     send the typed message to the coordinator",
        "  s         toggle the session sidebar",
        "  Tab       switch focus: input bar ↔ session sidebar",
        "  ↑ / ↓     scroll chat (input focus) / select session (sidebar)",
        "  ?         toggle this help",
        "  Esc       clear input / close help",
        "  q         quit",
        "",
        "  Prefix a message with @session: to route a command directly.",
    ]
    .join("\n")
}

/// Render the help overlay listing every key binding.
fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(58, 13, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(help_text())
            .style(Style::default().fg(Color::White))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(panel_title("Help — press ? or Esc to close")),
            ),
        area,
    );
}

/// Build the CMD> input line shown at the bottom of the dashboard.
///
/// Why: kept separate so a test can assert the rendered prompt and cursor glyph
/// without a terminal frame.
/// What: returns `CMD> <input>_` — the trailing `_` is the cursor, shown when
/// the input bar holds focus.
/// Test: `command_input_line_shows_cursor`.
pub fn command_input_line(bar: &CommandBar, focused: bool) -> String {
    if focused {
        format!("CMD> {}_", bar.input)
    } else {
        format!("CMD> {}", bar.input)
    }
}

/// Sidebar width in columns when visible.
const SIDEBAR_WIDTH: u16 = 22;

/// Draw the dashboard frame.
///
/// Why: the single entry point the event loop calls each tick.
/// What: a vertical layout — a two-line header (title + status bar); a flexing
/// middle area split horizontally into the session sidebar (when visible) and
/// the coordinator chat pane; and a full-width CMD> input strip at the bottom.
/// When `show_help` is set, a centred help overlay floats over the layout.
/// Test: rendering is exercised by the integration smoke test; line content is
/// unit-tested via `chat_lines` and `sidebar_items`.
pub fn render(frame: &mut Frame, state: &DashboardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header (title + status bar)
            Constraint::Min(4),    // sidebar + coordinator chat
            Constraint::Length(3), // CMD> input strip
        ])
        .split(frame.area());

    // Header: title doubles as a daemon-health indicator.
    let title = if state.daemon_reachable {
        format!("trusty-mpm — {} session(s)", state.sessions.len())
    } else {
        "trusty-mpm — daemon unreachable".to_string()
    };
    let header = Paragraph::new(vec![
        Line::from(title).style(title_style(state.daemon_reachable)),
        Line::from(status_line(state)).style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED),
        ),
    ]);
    frame.render_widget(header, chunks[0]);

    // Middle: optional sidebar beside the coordinator chat pane.
    let chat_area = if state.sidebar_visible {
        let middle = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(20)])
            .split(chunks[1]);
        let sidebar = List::new(sidebar_items(state)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(panel_title("Sessions")),
        );
        frame.render_widget(sidebar, middle[0]);
        middle[1]
    } else {
        chunks[1]
    };

    // Coordinator chat pane: a scrollable transcript.
    let lines = chat_lines(state);
    // The visible height excludes the bordered block's two border rows.
    let height = chat_area.height.saturating_sub(2) as usize;
    let offset = clamp_scroll(state.chat_scroll, lines.len(), height);
    let items: Vec<ListItem> = lines
        .into_iter()
        .skip(offset)
        .map(|(text, role)| {
            let color = match role {
                ChatRole::User => Color::White,
                ChatRole::Coordinator => Color::Cyan,
            };
            ListItem::new(Line::from(text).style(Style::default().fg(color)))
        })
        .collect();
    let chat = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(panel_title("Coordinator Chat")),
    );
    frame.render_widget(chat, chat_area);

    // CMD> input strip.
    let input_focused = state.focus == Focus::Input;
    let input_style = if input_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let input = Paragraph::new(command_input_line(&state.command_bar, input_focused))
        .style(input_style)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(input, chunks[2]);

    if state.show_help {
        render_help_overlay(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::{SessionId, SessionStatus};

    /// A deterministic test [`SessionId`] derived from a short label.
    fn sid(label: &str) -> SessionId {
        let mut bytes = [0u8; 16];
        for (slot, b) in bytes.iter_mut().zip(label.bytes()) {
            *slot = b;
        }
        SessionId(uuid::Uuid::from_bytes(bytes))
    }

    /// Build a `SessionRow` for tests.
    fn session(id: &str, status: SessionStatus, name: &str) -> SessionRow {
        SessionRow {
            id: sid(id),
            workdir: "/tmp/proj".into(),
            status,
            active_delegations: 0,
            tmux_name: name.into(),
            last_seen: Default::default(),
        }
    }

    #[test]
    fn command_bar_edits_buffer() {
        let mut bar = CommandBar::default();
        bar.push('h');
        bar.push('i');
        assert_eq!(bar.input, "hi");
        bar.backspace();
        assert_eq!(bar.input, "h");
    }

    #[test]
    fn command_bar_clear_empties_input() {
        let mut bar = CommandBar::default();
        bar.push('x');
        bar.clear();
        assert!(bar.input.is_empty());
    }

    #[test]
    fn command_bar_submit_records_history() {
        let mut bar = CommandBar {
            input: "  hello  ".into(),
            ..Default::default()
        };
        assert_eq!(bar.take_for_execution(), "hello");
        assert!(bar.input.is_empty());
        assert_eq!(bar.history, vec!["hello".to_string()]);
        // An empty submit is not recorded.
        assert_eq!(bar.take_for_execution(), "");
        assert_eq!(bar.history.len(), 1);
    }

    #[test]
    fn command_bar_history_recall() {
        let mut bar = CommandBar {
            input: "first".into(),
            ..Default::default()
        };
        bar.take_for_execution();
        bar.input = "second".into();
        bar.take_for_execution();
        bar.history_prev();
        assert_eq!(bar.input, "second");
        bar.history_prev();
        assert_eq!(bar.input, "first");
        bar.history_next();
        assert_eq!(bar.input, "second");
        bar.history_next();
        assert!(bar.input.is_empty());
    }

    #[test]
    fn chat_message_lines_prefix_role() {
        let state = DashboardState {
            chat_history: vec![
                ChatMessage::user("hello"),
                ChatMessage::coordinator("two sessions are active\nrun /sessions"),
            ],
            ..DashboardState::default()
        };
        let lines = chat_lines(&state);
        assert_eq!(lines[0].0, "[user] hello");
        assert_eq!(lines[0].1, ChatRole::User);
        assert_eq!(lines[1].0, "[coord] two sessions are active");
        // Continuation lines are indented, not re-prefixed.
        assert_eq!(lines[2].0, "        run /sessions");
        assert_eq!(lines[2].1, ChatRole::Coordinator);
    }

    #[test]
    fn chat_lines_empty_placeholder() {
        let lines = chat_lines(&DashboardState::default());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].0.contains("no messages yet"));
    }

    #[test]
    fn chat_history_grows_on_send() {
        let mut state = DashboardState::default();
        state.push_chat(ChatMessage::user("hi"));
        state.push_chat(ChatMessage::coordinator("hello back"));
        assert_eq!(state.chat_history.len(), 2);
        // push_chat snaps the scroll to the bottom.
        assert_eq!(state.chat_scroll, usize::MAX);
    }

    #[test]
    fn toggle_sidebar_flips_visibility() {
        let mut state = DashboardState::default();
        assert!(!state.sidebar_visible);
        state.toggle_sidebar();
        assert!(state.sidebar_visible);
        state.toggle_sidebar();
        assert!(!state.sidebar_visible);
    }

    #[test]
    fn tab_toggles_focus() {
        let mut state = DashboardState {
            sidebar_visible: true,
            ..DashboardState::default()
        };
        assert_eq!(state.focus, Focus::Input);
        state.toggle_focus();
        assert_eq!(state.focus, Focus::Sidebar);
        state.toggle_focus();
        assert_eq!(state.focus, Focus::Input);
    }

    #[test]
    fn tab_noop_when_sidebar_hidden() {
        // With the sidebar hidden, Tab must keep focus on the input bar so the
        // arrow keys never get silently captured by an invisible pane.
        let mut state = DashboardState::default();
        state.toggle_focus();
        assert_eq!(state.focus, Focus::Input);
    }

    #[test]
    fn selection_clamps_to_bounds() {
        let mut state = DashboardState {
            sessions: vec![
                session("a", SessionStatus::Active, "tmpm-a"),
                session("b", SessionStatus::Active, "tmpm-b"),
            ],
            selected_session: 99,
            ..DashboardState::default()
        };
        state.clamp_selection();
        assert_eq!(state.selected_session, 1);
        state.sessions.clear();
        state.clamp_selection();
        assert_eq!(state.selected_session, 0);
    }

    #[test]
    fn select_up_down_saturate() {
        let mut state = DashboardState {
            sessions: vec![
                session("a", SessionStatus::Active, "tmpm-a"),
                session("b", SessionStatus::Active, "tmpm-b"),
            ],
            ..DashboardState::default()
        };
        state.select_down();
        assert_eq!(state.selected_session, 1);
        state.select_down();
        assert_eq!(state.selected_session, 1);
        state.select_up();
        assert_eq!(state.selected_session, 0);
        state.select_up();
        assert_eq!(state.selected_session, 0);
    }

    #[test]
    fn selected_target_returns_none_when_empty() {
        assert_eq!(DashboardState::default().selected_target(), None);
        let state = DashboardState {
            sessions: vec![session("a", SessionStatus::Active, "tmpm-quiet-falcon")],
            ..DashboardState::default()
        };
        assert_eq!(state.selected_target(), Some("tmpm-quiet-falcon".into()));
    }

    #[test]
    fn status_indicator_maps_each_status() {
        assert_eq!(status_indicator(SessionStatus::Active).0, '●');
        assert_eq!(status_indicator(SessionStatus::Paused).0, '○');
        assert_eq!(status_indicator(SessionStatus::Stopped).0, '✕');
        assert_eq!(status_indicator(SessionStatus::Stopped).1, Color::Red);
    }

    #[test]
    fn session_prefix_strips_tmpm() {
        assert_eq!(session_prefix("tmpm-aipowerranking"), "aipowerranking");
        assert_eq!(session_prefix("frontend"), "frontend");
    }

    #[test]
    fn sidebar_items_format_each_session() {
        let state = DashboardState {
            sessions: vec![
                session("a", SessionStatus::Active, "tmpm-a"),
                session("b", SessionStatus::Paused, "tmpm-b"),
            ],
            ..DashboardState::default()
        };
        assert_eq!(sidebar_items(&state).len(), 2);
    }

    #[test]
    fn sidebar_items_empty_when_no_sessions() {
        assert!(sidebar_items(&DashboardState::default()).is_empty());
    }

    #[test]
    fn clamp_scroll_bounds_to_last_page() {
        // A huge offset (the "snap to bottom" sentinel) is clamped so the last
        // `height` lines stay visible.
        assert_eq!(clamp_scroll(usize::MAX, 100, 10), 90);
        // An in-range offset is left untouched.
        assert_eq!(clamp_scroll(5, 100, 10), 5);
        // Fewer lines than the height: no scroll needed.
        assert_eq!(clamp_scroll(usize::MAX, 3, 10), 0);
    }

    #[test]
    fn title_style_signals_daemon_health() {
        let healthy = title_style(true);
        assert_eq!(healthy.fg, Some(Color::Cyan));
        assert!(!healthy.add_modifier.contains(Modifier::REVERSED));
        let down = title_style(false);
        assert_eq!(down.fg, Some(Color::Red));
        assert!(down.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn status_line_falls_back_to_key_hint() {
        assert_eq!(status_line(&DashboardState::default()), KEY_HINT);
    }

    #[test]
    fn status_line_shows_last_action() {
        let state = DashboardState {
            last_action: Some("sent to coordinator".into()),
            ..DashboardState::default()
        };
        assert_eq!(status_line(&state), "sent to coordinator");
    }

    #[test]
    fn command_input_line_shows_cursor() {
        let bar = CommandBar {
            input: "hello".into(),
            ..Default::default()
        };
        assert_eq!(command_input_line(&bar, true), "CMD> hello_");
        assert_eq!(command_input_line(&bar, false), "CMD> hello");
    }

    #[test]
    fn help_text_lists_all_bindings() {
        let text = help_text();
        for token in ["Enter", "s ", "Tab", "?", "Esc", "q ", "@session:"] {
            assert!(text.contains(token), "help text missing {token}");
        }
    }

    #[test]
    fn short_session_extracts_prefix() {
        let id = sid("abcdefghij");
        assert_eq!(short_session(&id).len(), 8);
    }

    #[test]
    fn scroll_up_down_adjust_offset() {
        let mut state = DashboardState {
            chat_scroll: 5,
            ..DashboardState::default()
        };
        state.scroll_up();
        assert_eq!(state.chat_scroll, 4);
        state.scroll_down();
        assert_eq!(state.chat_scroll, 5);
    }
}
