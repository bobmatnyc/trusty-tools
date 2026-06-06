//! One-line status bar rendered below the REPL input area.
//!
//! Why: Users need at-a-glance visibility into which model/agent is active,
//! token consumption, and how long the session has been running — without
//! cluttering the prompt itself. Modeled on the Claude Code status line.
//! What: `StatusBar` holds session state (model, agent, token counters,
//! start time) and renders a single dim-styled line at the bottom of the
//! terminal via crossterm. `StatusBarConfig` controls which segments show.
//! Test: `status_line_format_includes_segments` verifies the rendered string
//! shape; visual verification on a TTY confirms placement.

// Why: The status-bar surface is partially wired — `StatusBar::new` is called
// in the REPL constructor, but the per-segment toggles, token accumulator,
// `format_line`, `render`, and `format_elapsed` helpers are staged for the
// streaming integration. Tests below exercise the helpers directly, so they
// must remain reachable. Suppress dead-code warnings module-wide rather than
// scattering individual `#[allow]`s.
#![allow(dead_code)]

use std::io::{IsTerminal, Write};
use std::time::Instant;

use crossterm::{
    cursor::{MoveTo, MoveToColumn, position as cursor_position},
    execute,
    style::{Attribute, Print, ResetColor, SetAttribute},
    terminal::{Clear, ClearType, size},
};

/// Toggles for which status segments are shown.
///
/// Why: Different users want different density. Tokens may be uninteresting
/// to a user running a quick local task; agent name is redundant if always
/// the same. Defaults show everything.
#[derive(Debug, Clone, Copy)]
pub struct StatusBarConfig {
    pub show_model: bool,
    pub show_agent: bool,
    pub show_tokens: bool,
    pub show_elapsed: bool,
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            show_model: true,
            show_agent: true,
            show_tokens: true,
            show_elapsed: true,
        }
    }
}

/// Status bar state and renderer.
///
/// Why: Centralizes the formatting/rendering so callers (REPL loop, future
/// event handlers) only need to mutate the data fields and call `render()`.
/// What: Holds model name, optional active agent, in/out token counts, and
/// the session start instant. Provides `render()` (write to stderr) and
/// `clear()` (blank the line).
pub struct StatusBar {
    pub model: String,
    pub agent: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub session_start: Instant,
    pub config: StatusBarConfig,
}

impl StatusBar {
    /// Create a new status bar pre-populated with the model name and start time.
    ///
    /// Why: A consistent constructor lets the REPL build the bar in `new()`
    /// without juggling Option fields.
    /// What: Returns a StatusBar with zero token counters and default config.
    /// Test: Construct, assert tokens_in/out == 0 and config equals default.
    pub fn new(model: impl Into<String>, session_start: Instant) -> Self {
        Self {
            model: model.into(),
            agent: None,
            tokens_in: 0,
            tokens_out: 0,
            session_start,
            config: StatusBarConfig::default(),
        }
    }

    /// Increment the token counters by `prompt` / `completion`.
    ///
    /// Why: Lets callers attribute every LLM round-trip to the running session
    /// without poking the public fields directly.
    /// What: Saturating add into `tokens_in`/`tokens_out`.
    /// Test: `add_tokens_accumulates`.
    pub fn add_tokens(&mut self, prompt: u32, completion: u32) {
        self.tokens_in = self.tokens_in.saturating_add(prompt as u64);
        self.tokens_out = self.tokens_out.saturating_add(completion as u64);
    }

    /// Set (or clear) the active agent label shown in the status bar.
    ///
    /// Why: The bar must reflect "we're now talking to python-engineer" while
    /// a delegation is in flight, then clear back to None when the task ends.
    /// What: Stores `agent` directly.
    /// Test: `set_agent_round_trips`.
    pub fn set_agent(&mut self, agent: Option<String>) {
        self.agent = agent;
    }

    /// Format the status segments as a single dimmed line.
    ///
    /// Why: Pulled out so we can unit-test the format without touching the
    /// terminal.
    /// What: Returns a string like "  claude-sonnet-4-6 | python-engineer | ↑1234 ↓5678 | 00:01:23  "
    /// honoring the active config flags.
    /// Test: `status_line_format_includes_segments` asserts segments appear
    /// when enabled and absent when disabled.
    pub fn format_line(&self) -> String {
        let mut segments: Vec<String> = Vec::new();

        if self.config.show_model && !self.model.is_empty() {
            segments.push(self.model.clone());
        }
        if self.config.show_agent
            && let Some(agent) = &self.agent
            && !agent.is_empty()
        {
            segments.push(agent.clone());
        }
        if self.config.show_tokens {
            segments.push(format!("↑{} ↓{}", self.tokens_in, self.tokens_out));
        }
        if self.config.show_elapsed {
            segments.push(format_elapsed(self.session_start.elapsed()));
        }

        if segments.is_empty() {
            String::new()
        } else {
            format!("  {}  ", segments.join(" | "))
        }
    }

    /// Render the status line beneath the cursor's current row.
    ///
    /// Why: Keeps the bar visible without disrupting the user's input area.
    /// What: If stderr is a TTY, save the cursor, drop to the bottom row,
    /// clear and write the dim status line, then restore. In non-TTY mode
    /// (CI, redirected output) this is a no-op so log output stays clean.
    /// Test: Manual on a TTY; non-TTY check is implicit (no panic, no write).
    pub fn render(&self) {
        let mut stderr = std::io::stderr();
        if !stderr.is_terminal() {
            return;
        }
        let line = self.format_line();
        if line.is_empty() {
            return;
        }
        let (_cols, rows) = match size() {
            Ok(s) => s,
            Err(_) => return,
        };
        // Query the cursor position via ESC[6n instead of using the terminal's
        // SavePosition/RestorePosition slot — reedline relies on that single
        // save slot, and clobbering it causes the cursor to jump after a task
        // completes. If the terminal doesn't support the query, bail out.
        let (cur_col, cur_row) = match cursor_position() {
            Ok(p) => p,
            Err(_) => return,
        };
        // Render at the last terminal row. Best-effort — ignore errors so a
        // misbehaving terminal never aborts the REPL.
        let bottom_row = rows.saturating_sub(1);
        let _ = execute!(
            stderr,
            MoveTo(0, bottom_row),
            Clear(ClearType::CurrentLine),
            SetAttribute(Attribute::Dim),
            Print(line),
            ResetColor,
            SetAttribute(Attribute::Reset),
            MoveTo(cur_col, cur_row)
        );
        let _ = stderr.flush();
    }

    /// Blank the status line. Useful before printing multi-line output that
    /// would otherwise overlap.
    ///
    /// Why: Prevents stale status text from bleeding into long results.
    /// What: Saves cursor, jumps to bottom row, clears it, restores cursor.
    /// Test: Manual.
    #[allow(dead_code)]
    pub fn clear(&self) {
        let mut stderr = std::io::stderr();
        if !stderr.is_terminal() {
            return;
        }
        let (_cols, rows) = match size() {
            Ok(s) => s,
            Err(_) => return,
        };
        let (cur_col, cur_row) = match cursor_position() {
            Ok(p) => p,
            Err(_) => return,
        };
        let bottom_row = rows.saturating_sub(1);
        let _ = execute!(
            stderr,
            MoveTo(0, bottom_row),
            Clear(ClearType::CurrentLine),
            MoveToColumn(0),
            MoveTo(cur_col, cur_row)
        );
        let _ = stderr.flush();
    }
}

/// Format a Duration as HH:MM:SS (or MM:SS if under an hour).
///
/// Why: The status bar needs a compact, human-friendly elapsed display.
/// What: Returns "MM:SS" for short sessions, "HH:MM:SS" for longer ones.
/// Test: `format_elapsed_short_session` and `format_elapsed_long_session`.
pub fn format_elapsed(d: std::time::Duration) -> String {
    let total = d.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn format_elapsed_short_session() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "00:00");
        assert_eq!(format_elapsed(Duration::from_secs(83)), "01:23");
        assert_eq!(format_elapsed(Duration::from_secs(3599)), "59:59");
    }

    #[test]
    fn format_elapsed_long_session() {
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "01:00:00");
        assert_eq!(format_elapsed(Duration::from_secs(3661)), "01:01:01");
    }

    #[test]
    fn status_line_format_includes_segments() {
        let mut bar = StatusBar::new("claude-sonnet-4-6", Instant::now());
        bar.agent = Some("python-engineer".to_string());
        bar.tokens_in = 1234;
        bar.tokens_out = 5678;
        let line = bar.format_line();
        assert!(line.contains("claude-sonnet-4-6"), "model missing: {line}");
        assert!(line.contains("python-engineer"), "agent missing: {line}");
        assert!(line.contains("↑1234"), "tokens-in missing: {line}");
        assert!(line.contains("↓5678"), "tokens-out missing: {line}");
    }

    #[test]
    fn status_line_respects_config_disable() {
        let mut bar = StatusBar::new("modelX", Instant::now());
        bar.agent = Some("agentX".to_string());
        bar.config = StatusBarConfig {
            show_model: false,
            show_agent: false,
            show_tokens: false,
            show_elapsed: true,
        };
        let line = bar.format_line();
        assert!(!line.contains("modelX"), "model should be hidden: {line}");
        assert!(!line.contains("agentX"), "agent should be hidden: {line}");
        assert!(!line.contains("↑"), "tokens should be hidden: {line}");
    }

    #[test]
    fn add_tokens_accumulates() {
        let mut bar = StatusBar::new("m", Instant::now());
        bar.add_tokens(100, 50);
        bar.add_tokens(200, 25);
        assert_eq!(bar.tokens_in, 300);
        assert_eq!(bar.tokens_out, 75);
    }

    #[test]
    fn set_agent_round_trips() {
        let mut bar = StatusBar::new("m", Instant::now());
        bar.set_agent(Some("python-engineer".into()));
        assert_eq!(bar.agent.as_deref(), Some("python-engineer"));
        bar.set_agent(None);
        assert!(bar.agent.is_none());
    }

    #[test]
    fn status_line_empty_when_all_disabled() {
        let mut bar = StatusBar::new("m", Instant::now());
        bar.config = StatusBarConfig {
            show_model: false,
            show_agent: false,
            show_tokens: false,
            show_elapsed: false,
        };
        assert_eq!(bar.format_line(), "");
    }
}
