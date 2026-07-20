//! The engine-agnostic render/reducer model: [`ReplApp`].
//!
//! Why: DOC-50 §3.2/§5 Slice 4 generalizes tagent's `ReplApp`
//! (`crates/trusty-agents/src/repl/tui/types.rs`/`app.rs`) into the shape
//! both `AgentEngine` (tagent) and `CodeEngine` (tcode) can render against
//! without either product's semantics leaking into the shared crate. The
//! original interleaves ~15 tagent-specific fields (OpenRouter cost
//! tracking, TM/claude-mpm session counts, model/provider pickers, agent
//! scope) with the genuinely generic chat/input/history/scroll state DOC-50
//! §3.2 calls out for extraction — this module keeps only the latter.
//! Product-specific data (statusline content, picker items, the
//! slash-command table) flows in through the Slice-1/1.5 seam
//! ([`crate::event::ReplEvent::StatuslineUpdate`], [`crate::model`]) instead
//! of being hardcoded here, per the generalization mandate.
//!
//! What: [`ReplApp`] is the `M` the shared [`crate::run::event_loop`] drives
//! (it implements [`crate::run::TuiModel`]). [`reduce::apply`] is the
//! `apply: FnMut(&mut M, ReplEvent)` reducer half of that contract; this
//! module owns the state and its primitive mutators (`insert_char`,
//! `backspace`, `push_user`, `scroll`, …). Most are direct ports; a few
//! (`Up`/`Down`/Ctrl-E key handling) are ports of tagent's REAL production
//! behavior rather than its dead-code helpers — see the disclosure list
//! below, which spells out every point (not just those) where this module's
//! behavior differs from tagent's, since Slice 10's cutover plans against
//! exactly this list. A product wiring `run()` (Slice 5+, tagent's eventual
//! `AgentEngine`/tcode's `CodeEngine`) supplies its own product-specific
//! fields (splash art, banner title, command table) via the public setters
//! below rather than this crate special-casing either product.
//!
//! ## Disclosed behavioral differences from tagent (DOC-50 §5 Slice 4)
//!
//! Ported **faithfully** (same production behavior, verified against
//! tagent's actual `keys.rs`/`app.rs`, not just its doc comments — one round
//! of review on this slice caught a doc/reality drift in tagent itself, see
//! history below):
//! - Up-arrow recalls [`Self::last_prompt`] (the single most recent
//!   submission) and, when [`Self::busy`], ALSO sets
//!   [`Self::pending_cancel`] — mirrors
//!   `crates/trusty-agents/src/repl/tui/keys.rs`'s real `KeyCode::Up` arm,
//!   not the multi-level `history_prev`/`history_next` `#[allow(dead_code)]`
//!   helpers tagent's own doc comment (misleadingly) attributes to
//!   production Up-handling.
//! - Down-arrow calls [`Self::history_next`] — tagent's real `KeyCode::Down`
//!   arm does exactly this, even though nothing (including this crate's Up
//!   handler) ever sets `history_idx`, making it a functional no-op today in
//!   both tagent and here. Ported for fidelity, not because it currently
//!   does anything.
//! - Ctrl-E, with an empty input buffer and a non-`None`
//!   [`Self::last_bash_block`], pastes only that block's first non-blank
//!   line (not the whole block) — matches
//!   `crates/trusty-agents/src/repl/tui/keys.rs`'s `KeyCode::Char('e')` arm
//!   exactly (the REPL input is single-line; pasting a multi-line block
//!   would silently truncate at the first `\n` on submit).
//! - The input composer's right-aligned `[thinking...]` label renders
//!   whenever [`Self::busy`] is true, REGARDLESS of whether the input
//!   buffer is empty — mirrors
//!   `crates/trusty-agents/src/repl/tui/chat.rs::draw_input`. `busy` is set
//!   `true` at submit time (not on the first streamed chunk), so the
//!   pre-first-token latency window after Enter also shows the indicator.
//!
//! Deliberately **NOT** ported (Phase 2 / later-slice / no-shared-crate-
//! formula scope — not oversights):
//! - The three-row animated activity panel (spinner cycling, elapsed timer,
//!   rust-rainbow shimmer, latest thinking-step echo —
//!   `repl/tui/layout.rs::draw_activity`, `repl/tui/status.rs`'s
//!   `SPINNER_FRAMES`/`rainbow_spans`/`hsl_to_rgb`). `busy: bool` is this
//!   crate's entire generalization of tagent's two-field
//!   `thinking: bool` + `busy_since: Option<Instant>` — there is no elapsed-
//!   time derivation available here (no stored start `Instant`).
//! - Inline token counters in the input row and ALL cost/usage tracking
//!   (`tokens_in`/`tokens_out`, `daily_cost_start`, usage-file persistence,
//!   the OpenRouter pricing formula) — DOC-50 Q9: no cost formula belongs in
//!   the shared crate; cost is engine-supplied, pre-formatted, via
//!   [`crate::model::StatuslineSegment::Cost`].
//! - Model/provider pickers (`PickerState`/`PickerKind`) and the inline
//!   LLM-offered/slash-completion choice list (`choices`/`choices_context`/
//!   `update_slash_completions`) — Phase 2 (DOC-50 §5 Slice 7 and later);
//!   [`crate::model::PickerItem`]/[`crate::model::PickerRequest`] exist
//!   (Slice 1.5) but no widget consumes them yet.
//! - `AgentScope`'s User/Project cyan/yellow semantic — replaced by a plain
//!   [`Self::accent_color`] field with no "scope" concept; the engine
//!   decides what it means and when to change it.
//! - The shared `trusty_common::banner` splash art, tagent's hardcoded
//!   banner title/identity text, and its fixed `/help`/`/connect`/`/clear`/
//!   `/status` command list — all now engine-supplied fields
//!   ([`Self::banner_art`], [`Self::banner_title`], [`Self::commands`],
//!   [`Self::recent_activity`]) rather than constants (this crate must not
//!   depend on `trusty_common`, see DOC-50 §2.2's dependency direction).
//! - The literal `"[trusty-agents] "` status prefix and its exact idle-hint
//!   copy (`"Ask ctrl anything, or /connect <path> for project work"`) —
//!   generalized to [`Self::status_prefix`] (defaulted from `label`) and a
//!   generic [`crate::widgets::input_composer`] hint string respectively.
//!
//! # Spec References
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — §3.2, the generalization layer.
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): `ReplApp` state.

mod reduce;

pub use reduce::apply;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use ratatui::style::Color;
use ratatui::text::Line;

use crate::event::WorkstreamSummary;
use crate::model::{CommandDescriptor, StatuslineSegment};
use crate::run::TuiModel;
use crate::text::{strip_interior_blank_lines, trim_surrounding_blank_lines};

/// One rendered chat entry in the scrollback.
///
/// Why: a direct, unmodified port of tagent's `ChatLine`
/// (`crates/trusty-agents/src/repl/tui/types.rs`) — already fully
/// engine-agnostic (a role + a string), so no generalization was needed.
#[derive(Debug, Clone)]
pub struct ChatLine {
    pub role: ChatRole,
    pub text: String,
}

/// Source/role of a chat line — drives the leader glyph and color chosen by
/// [`crate::widgets::scrollback::build_chat_lines`].
///
/// Why: a direct port of tagent's `ChatRole` — the four roles (who's
/// speaking, or a system status line) are universal to any chat-shaped TUI,
/// not specific to tagent's personas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// User input.
    User,
    /// Assistant/agent response.
    Assistant,
    /// An error response (rendered in the scrollback's error color).
    Error,
    /// An informational status line (e.g. "Connection lost", a tool
    /// invocation notice).
    Status,
}

/// All mutable state the shared render/event loop needs to draw one frame.
///
/// Why: kept separate from the render/event-loop machinery (mirroring
/// tagent's original rationale) so unit tests can drive state transitions
/// without a `Terminal`. See the module doc comment for what was
/// deliberately left out of the generalized port.
/// What: every field is `pub` (matching tagent's style — this is a plain
/// data/mutator struct, not an encapsulated type) so a product binary can
/// seed banner/branding fields at construction without a builder API.
#[derive(Debug, Clone)]
pub struct ReplApp {
    /// Scrollback history of user/assistant/status exchanges.
    pub chat: Vec<ChatLine>,
    /// Current input line being edited.
    pub input_buf: String,
    /// Byte offset within `input_buf`.
    pub cursor_pos: usize,
    /// Number of lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll_offset: usize,
    /// Last-rendered maximum scroll offset (published by the scrollback
    /// widget each frame; `scroll()` clamps against it). `Arc` so a render
    /// snapshot and the authoritative instance behind a caller's mutex (if
    /// any) share the same cell — mirrors tagent's `last_max_scroll`.
    pub last_max_scroll: Arc<AtomicUsize>,
    /// In-memory input history (Up-arrow recall).
    pub history: Vec<String>,
    /// Index into `history` while navigating. `None` when not navigating.
    pub history_idx: Option<usize>,
    /// Saved current input while navigating history.
    pub saved_input: Option<String>,
    /// Whether to show the welcome banner in the scrollback.
    pub show_banner: bool,
    /// The name shown in the chat leader (`⏺ <label> · `) and the input
    /// prompt (`<label>> `). Generic replacement for tagent's
    /// `project_name` — any single display label a product wants there.
    pub label: String,
    /// The user's display name, shown in the banner's identity line.
    pub user_label: String,
    /// Accent color for the chat leader glyph and label. Generic replacement
    /// for tagent's `AgentScope`-driven cyan/yellow — a product sets this to
    /// whatever distinguishes its own contexts (or leaves the default).
    pub accent_color: Color,
    /// Prefix rendered before every [`ChatRole::Status`] line (e.g.
    /// `"[tagent] "`). Defaults to `"[{label}] "` in [`ReplApp::new`];
    /// override if a product wants different bracketed text than its
    /// `label`.
    pub status_prefix: String,
    /// Banner title text (defaults to `label`).
    pub banner_title: String,
    /// Version string shown in the banner (defaults to this crate's own
    /// version — a product should override with its own).
    pub version: String,
    /// Recent-activity lines shown in the banner's right column. Generic
    /// replacement for tagent's `git_commits` — a product decides what
    /// "recent activity" means for it (or supplies none).
    pub recent_activity: Vec<String>,
    /// Optional pre-rendered left-column art (e.g. a splash logo). Empty by
    /// default — this crate has no branding assets of its own (see the
    /// dependency-direction constraint in the crate-level doc comment: this
    /// crate must not depend on a product-specific art asset).
    pub banner_art: Vec<Line<'static>>,
    /// Slash commands shown in the banner's "Commands" section, sourced from
    /// [`crate::engine::TuiEngine::commands`] plus any client-side built-ins.
    pub commands: Vec<CommandDescriptor>,
    /// Whether a response is currently streaming in. Drives the input
    /// composer's placeholder text.
    pub busy: bool,
    /// Index into `chat` of the in-progress streaming assistant entry, if
    /// any. `None` when idle.
    pub streaming_idx: Option<usize>,
    /// Engine-supplied statusline segments, most recently pushed via
    /// [`crate::event::ReplEvent::StatuslineUpdate`].
    pub statusline: Vec<StatuslineSegment>,
    /// The active workstream, most recently pushed via
    /// [`crate::event::ReplEvent::WorkstreamUpdated`].
    pub active_workstream: Option<WorkstreamSummary>,
    /// Quit signal — set on Ctrl-D. Backs [`TuiModel::should_quit`].
    pub quit: bool,
    /// A line the user just submitted (typed Enter, or a synthesized
    /// `ReplEvent::Submit`), staged for the outer driver to forward to
    /// `TuiEngine::handle_input`. `apply` cannot reach the engine or the
    /// event channel itself (see [`reduce`]'s doc comment), so — mirroring
    /// tagent's `pending_picker_selection`/`pending_submit` pattern — it
    /// stashes the line here instead. Drained (`Option::take`) by the
    /// caller after each `apply` call.
    pub pending_submit: Option<String>,
    /// Set when the user pressed Ctrl-C. Drained by the caller, which should
    /// call `TuiEngine::cancel_session` (thin-client axiom C-2 — cancellation
    /// is the backend's job, not just clearing local UI state).
    pub pending_cancel: bool,
    /// The most recently submitted line, restored to `input_buf` on
    /// Up-arrow. Direct port of tagent's `last_prompt` (single-level recall,
    /// not the multi-level `history`/`history_idx` browser — see the module
    /// doc comment's disclosure list for why Up uses this field and not
    /// `history_prev`).
    pub last_prompt: String,
    /// The most recently seen executable-shell (bash/sh/zsh/fish) fenced
    /// code block across all assistant messages, updated by
    /// [`Self::update_last_bash_block`]. Ctrl-E pastes this block's first
    /// non-blank line into an empty input buffer — direct port of tagent's
    /// `last_bash_block` (`crates/trusty-agents/src/repl/tui/types.rs`).
    pub last_bash_block: Option<String>,
}

impl ReplApp {
    /// Construct a fresh app with sensible generic defaults.
    ///
    /// Why: every field needs *some* value; the defaults chosen here (empty
    /// scrollback, banner visible, cyan accent, no commands/art yet) mirror
    /// tagent's `ReplApp::new` starting state minus the fields that didn't
    /// survive generalization.
    /// What: `label`/`user_label` seed both the derived `status_prefix` and
    /// `banner_title` — override those two afterward if a product wants
    /// different text than a straight derivation from `label`.
    pub fn new(label: impl Into<String>, user_label: impl Into<String>) -> Self {
        let label = label.into();
        let status_prefix = format!("[{label}] ");
        let banner_title = label.clone();
        Self {
            chat: Vec::new(),
            input_buf: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            last_max_scroll: Arc::new(AtomicUsize::new(0)),
            history: Vec::new(),
            history_idx: None,
            saved_input: None,
            show_banner: true,
            label,
            user_label: user_label.into(),
            accent_color: Color::Cyan,
            status_prefix,
            banner_title,
            version: env!("CARGO_PKG_VERSION").to_string(),
            recent_activity: Vec::new(),
            banner_art: Vec::new(),
            commands: Vec::new(),
            busy: false,
            streaming_idx: None,
            statusline: Vec::new(),
            active_workstream: None,
            quit: false,
            pending_submit: None,
            pending_cancel: false,
            last_prompt: String::new(),
            last_bash_block: None,
        }
    }

    /// Append a user prompt line to the chat scrollback.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.chat.push(ChatLine {
            role: ChatRole::User,
            text: text.into(),
        });
        self.scroll_offset = 0;
    }

    /// Append an assistant response to the chat scrollback, trimming
    /// surrounding and interior blank lines first.
    ///
    /// Why: assistant/agent responses regularly arrive with leading/trailing
    /// blank lines and markdown-style `\n\n` paragraph breaks; rendering
    /// them verbatim leaves visible dead space. See [`crate::text`] for the
    /// two trimming passes.
    /// Test: [`reduce::tests::push_assistant_trims_surrounding_blanks`].
    pub fn push_assistant(&mut self, text: impl Into<String>, is_error: bool) {
        let role = if is_error {
            ChatRole::Error
        } else {
            ChatRole::Assistant
        };
        let raw: String = text.into();
        let trimmed = trim_surrounding_blank_lines(&raw);
        let collapsed = strip_interior_blank_lines(&trimmed);
        self.chat.push(ChatLine {
            role,
            text: collapsed,
        });
        self.scroll_offset = 0;
        self.update_last_bash_block();
    }

    /// Rescan `self.chat` and refresh [`Self::last_bash_block`] with the
    /// last executable-shell fenced block seen across all assistant
    /// messages, newest-first. Direct port of tagent's
    /// `ReplApp::update_last_bash_block`
    /// (`crates/trusty-agents/src/repl/tui/app.rs`).
    ///
    /// Why: called after every chat mutation that could introduce or
    /// obsolete a shell block ([`Self::push_assistant`], and the streaming
    /// finalize path in [`reduce`]) so Ctrl-E's paste buffer never goes
    /// stale relative to `chat`.
    /// What: walks `chat` newest-to-oldest; the first [`ChatRole::Assistant`]
    /// entry containing an executable shell fence wins (errors and user/
    /// status entries are skipped — an error entry is never a paste source).
    /// `None` when no assistant entry has one.
    /// Test: [`reduce::tests::push_assistant_updates_last_bash_block`],
    /// [`reduce::tests::push_assistant_skips_error_entries_for_bash_block`].
    pub fn update_last_bash_block(&mut self) {
        for entry in self.chat.iter().rev() {
            if entry.role != ChatRole::Assistant {
                continue;
            }
            if let Some(block) = crate::render::markdown::extract_last_shell_block(&entry.text) {
                self.last_bash_block = Some(block);
                return;
            }
        }
        self.last_bash_block = None;
    }

    /// Append a status line (rendered with [`Self::status_prefix`]).
    pub fn push_status(&mut self, text: impl Into<String>) {
        self.chat.push(ChatLine {
            role: ChatRole::Status,
            text: text.into(),
        });
        self.scroll_offset = 0;
    }

    /// Clear the scrollback (backs `ReplEvent::ClearScrollback`, the shared
    /// `/clear` built-in per DOC-50 §5 Slice 7).
    pub fn clear_scrollback(&mut self) {
        self.chat.clear();
        self.streaming_idx = None;
        self.scroll_offset = 0;
    }

    /// Push the current input onto history (with adjacent-duplicate dedup),
    /// record it as [`Self::last_prompt`] (Up-arrow recall), mark the app
    /// [`Self::busy`], echo it to the scrollback, and stage it in
    /// [`Self::pending_submit`].
    ///
    /// Why: shared by the Enter-key path and a synthesized
    /// `ReplEvent::Submit` (e.g. a future picker confirmation) so both go
    /// through one echo+stage code path rather than duplicating it — see
    /// [`reduce`]. Setting `busy` here (not on the first streamed
    /// `AssistantOutput` chunk) closes the pre-first-token latency window
    /// where tagent's `[thinking...]` indicator is already lit but this
    /// crate's used to still show the idle placeholder — see the module doc
    /// comment's disclosure list.
    pub(crate) fn submit_line(&mut self, line: String) {
        self.remember_input(&line);
        self.last_prompt = line.clone();
        self.busy = true;
        self.push_user(line.clone());
        self.pending_submit = Some(line);
    }

    /// Push a line onto `history`, deduping an immediate repeat.
    pub fn remember_input(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.history.last().map(|s| s.as_str()) == Some(line) {
            return;
        }
        self.history.push(line.to_string());
    }

    /// Set the input buffer + cursor in one shot (cursor moves to the end).
    pub fn set_input(&mut self, s: String) {
        self.cursor_pos = s.len();
        self.input_buf = s;
    }

    /// Insert a single character at the cursor.
    pub fn insert_char(&mut self, c: char) {
        self.input_buf.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
        self.history_idx = None;
    }

    /// Backspace: delete the char before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let mut prev = self.cursor_pos - 1;
        while prev > 0 && !self.input_buf.is_char_boundary(prev) {
            prev -= 1;
        }
        self.input_buf.replace_range(prev..self.cursor_pos, "");
        self.cursor_pos = prev;
    }

    /// Move the cursor left one char-boundary.
    pub fn cursor_left(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let mut prev = self.cursor_pos - 1;
        while prev > 0 && !self.input_buf.is_char_boundary(prev) {
            prev -= 1;
        }
        self.cursor_pos = prev;
    }

    /// Move the cursor right one char-boundary.
    pub fn cursor_right(&mut self) {
        if self.cursor_pos >= self.input_buf.len() {
            return;
        }
        let mut next = self.cursor_pos + 1;
        while next < self.input_buf.len() && !self.input_buf.is_char_boundary(next) {
            next += 1;
        }
        self.cursor_pos = next;
    }

    /// Up-arrow history navigation.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let new_idx = match self.history_idx {
            None => {
                self.saved_input = Some(self.input_buf.clone());
                self.history.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.history_idx = Some(new_idx);
        let entry = self.history[new_idx].clone();
        self.set_input(entry);
    }

    /// Down-arrow history navigation.
    pub fn history_next(&mut self) {
        let Some(i) = self.history_idx else { return };
        if i + 1 >= self.history.len() {
            let restore = self.saved_input.take().unwrap_or_default();
            self.history_idx = None;
            self.set_input(restore);
        } else {
            self.history_idx = Some(i + 1);
            let entry = self.history[i + 1].clone();
            self.set_input(entry);
        }
    }

    /// Take the current input buffer and reset the editor state. Returns
    /// `None` if the trimmed buffer was empty (nothing to submit).
    pub fn take_input(&mut self) -> Option<String> {
        let trimmed = self.input_buf.trim();
        if trimmed.is_empty() {
            return None;
        }
        let out = std::mem::take(&mut self.input_buf);
        self.cursor_pos = 0;
        self.history_idx = None;
        self.saved_input = None;
        Some(out)
    }

    /// Apply a scroll delta. Negative = older (up), positive = newer (down).
    /// Clamps to `[0, last_max_scroll]` so mouse-wheel scroll-up can't
    /// accumulate past the actual scrollback height.
    pub fn scroll(&mut self, delta: isize) {
        if delta < 0 {
            self.scroll_offset = self.scroll_offset.saturating_add((-delta) as usize);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(delta as usize);
        }
        let cap = self
            .last_max_scroll
            .load(std::sync::atomic::Ordering::Relaxed);
        if self.scroll_offset > cap {
            self.scroll_offset = cap;
        }
    }
}

impl TuiModel for ReplApp {
    fn should_quit(&self) -> bool {
        self.quit
    }
}
