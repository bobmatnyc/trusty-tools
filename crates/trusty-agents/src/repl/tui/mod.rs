//! Ratatui-based REPL renderer (#268).
//!
//! Why: The crossterm-based REPL in `mod.rs`/`input.rs`/`banner.rs` relies on
//! manual cursor geometry (`bar_top = rows - 3`, `ScrollDown(pad)`,
//! `cursor::position()` queries) that races with background tokio writers and
//! breaks every time a new feature shifts the layout math. ratatui's
//! declarative model — describe the full screen, let the framework diff —
//! eliminates the entire bug class by making async output flow through an
//! `mpsc` channel instead of competing for the cursor.
//! What: `ReplApp` carries all mutable UI state. `ReplEvent` is the union of
//! everything that can mutate that state (key presses, LLM responses, status
//! lines, resizes). `run_tui` enters alt-screen + raw mode, spawns a key-event
//! reader, drives the existing `forward_task`-style dispatch via an injected
//! handler, and on each event repaints the whole frame. The banner, chat
//! scrollback, and input bar are all standard ratatui widgets.
//! Test: `repl_app_*` unit tests cover state transitions; full-screen
//! rendering is exercised via `scripts/tmux-repl-test.sh`.

// External crate imports, re-exported `pub(crate)` so the split
// submodules pick them up through `use super::*;` (#357).
#[allow(unused_imports)]
pub(crate) use std::io::{self, Stdout};
pub(crate) use std::path::PathBuf;
pub(crate) use std::sync::Arc;

pub(crate) use anyhow::{Context, Result};
pub(crate) use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
pub(crate) use crossterm::execute;
pub(crate) use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
pub(crate) use ratatui::Terminal;
pub(crate) use ratatui::backend::CrosstermBackend;
pub(crate) use ratatui::layout::{Constraint, Direction, Layout, Rect};
pub(crate) use ratatui::style::{Color, Modifier, Style};
pub(crate) use ratatui::text::{Line, Span};
pub(crate) use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
pub(crate) use tokio::sync::Mutex;
pub(crate) use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

pub(crate) use crate::repl::statusline::{StatuslineConfig, render_statusline};

mod app;
mod banner;
mod chat;
mod events;
mod helpers;
mod keys;
mod layout;
mod markdown;
mod pickers;
mod run;
mod status;
mod types;

#[cfg(test)]
mod tests_input;
#[cfg(test)]
mod tests_render;
#[cfg(test)]
mod tests_state;

// Flat re-export of every submodule surface so cross-submodule
// references resolve via `use super::*;` and external callers see
// the same public API the monolithic `tui.rs` exposed (#357).
//
// Note: `app` contributes only `impl ReplApp` (no free items), so it has no
// glob to re-export — the methods attach to `ReplApp` re-exported from `types`.
pub(crate) use banner::*;
pub(crate) use chat::*;
pub(crate) use events::*;
pub(crate) use helpers::*;
pub(crate) use keys::*;
pub(crate) use layout::*;
pub(crate) use markdown::*;
pub(crate) use pickers::*;
pub(crate) use run::*;
pub(crate) use status::*;
pub(crate) use types::*;
