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
//! - Up-arrow, when `Self::busy`, sets `Self::pending_cancel` — mirrors
//!   `crates/trusty-agents/src/repl/tui/keys.rs`'s real `KeyCode::Up` arm.
//!   What Up RECALLS diverges (#8181, see the divergence list below).
//! - Ctrl-E, with an empty input buffer and a non-`None`
//!   `Self::last_bash_block`, pastes only that block's first non-blank
//!   line (not the whole block) — matches
//!   `crates/trusty-agents/src/repl/tui/keys.rs`'s `KeyCode::Char('e')` arm
//!   exactly (the REPL input is single-line; pasting a multi-line block
//!   would silently truncate at the first `\n` on submit).
//! - The input composer's right-aligned `[thinking...]` label renders
//!   whenever `Self::busy` is true, REGARDLESS of whether the input
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
//!   `Self::accent_color` field with no "scope" concept; the engine
//!   decides what it means and when to change it.
//! - The shared `trusty_common::banner` splash art, tagent's hardcoded
//!   banner title/identity text, and its fixed `/help`/`/connect`/`/clear`/
//!   `/status` command list — all now engine-supplied fields
//!   (`Self::banner_art`, `Self::banner_title`, [`Self::commands`](crate::commands),
//!   `Self::recent_activity`) rather than constants (this crate must not
//!   depend on `trusty_common`, see DOC-50 §2.2's dependency direction).
//! - The literal `"[trusty-agents] "` status prefix and its exact idle-hint
//!   copy (`"Ask ctrl anything, or /connect <path> for project work"`) —
//!   generalized to `Self::status_prefix` (defaulted from `label`) and a
//!   generic [`crate::widgets::input_composer`] hint string respectively.
//!
//! Deliberately **DIVERGES** from tagent (Slice 5, DOC-50 §5 — an intentional
//! improvement per spec, not an oversight or a parity gap):
//! - **Ctrl-C cancels the in-flight request** (see `reduce::apply_key`'s
//!   `'c'` arm, staging `Self::pending_cancel` for `crate::run::run`'s
//!   dispatch step to relay as a real `TuiEngine::cancel_session` RPC).
//!   Tagent's actual Ctrl-c (`crates/trusty-agents/src/repl/tui/keys.rs`)
//!   only clears the input buffer — tagent's cancel trigger is Up-arrow
//!   while `thinking`. DOC-50 §5 Slice 5 specifies Ctrl-C as the cancel key
//!   (the conventional terminal interrupt), which this crate implements as
//!   written; Up-arrow-while-busy ALSO still signals cancel here (ported
//!   faithfully from tagent, see above), so both triggers work.
//! - **Up/Down walk the whole session's prompt history** (#8181). Tagent's
//!   real `KeyCode::Up` arm recalls only `last_prompt` — one snapshot,
//!   overwritten every submit — and its `KeyCode::Down` arm calls
//!   `history_next` against a `history_idx` nothing ever sets, so Down is a
//!   no-op there. This crate wires Up to `Self::history_prev` instead, which
//!   makes both arms live: Up steps older through `Self::history`, Down
//!   steps newer and restores the in-progress draft at the end. The
//!   busy-cancel half of Up is unchanged.
//!
//! # Spec References
//! - [`SPEC-TTUI-03~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — §3.2, the generalization layer.
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): `ReplApp` state.

mod reduce;

pub use reduce::apply;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use ratatui::style::Color;
use ratatui::text::Line;

use crate::event::WorkstreamSummary;
use crate::model::{
    CommandDescriptor, PendingPermission, PermissionAnswer, PermissionResponse, PickerRequest,
    StatuslineSegment,
};
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
    /// The tool call this entry renders as a card, if any (#4596). When
    /// `Some`, the scrollback draws the card and ignores `text`; `role` still
    /// places it — [`ChatRole::Status`] at top level, [`ChatRole::Delegated`]
    /// inside a delegation block.
    pub tool: Option<ToolCard>,
}

/// One tool call and its result, rendered as a single scrollback card
/// (#4596).
///
/// Why: [`crate::event::ReplEvent::ToolInvocation`] arrives twice per call
/// (start, then completion) sharing an `id`. Holding both halves in one value
/// lets the reducer fill in one card instead of pushing two entries.
/// What: `result` is `None` while the call is in flight. `args` keeps the
/// start event's payload, because a completion event may carry `Null` args.
/// Test: `reduce::tests::tool_invocation_result_merges_into_its_call_card`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCard {
    /// The engine-minted call id shared by the start and completion events.
    pub id: String,
    /// The invoked tool's name.
    pub tool_name: String,
    /// The call's arguments, as the start event carried them.
    pub args: serde_json::Value,
    /// The tool's output; `None` while the call is still running.
    pub result: Option<String>,
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
    /// An informational status line (e.g. "Connection lost"), or a
    /// top-level tool card when the entry carries [`ChatLine::tool`].
    Status,
    /// The header or footer line framing one delegated sub-agent's block
    /// (#7940). Rendered flush-left with its own glyph so the block's start
    /// and end are scannable.
    Delegation,
    /// Output or a tool notice produced by a delegated sub-agent (#7940).
    /// Rendered indented inside the [`ChatRole::Delegation`] frame above it.
    Delegated,
}

/// One delegated sub-agent whose block is currently open in the scrollback
/// (#7940).
///
/// Why: the reducer needs two things after a delegation starts — whether an
/// arriving `(agent_id, turn_id)` stream belongs inside a delegation block,
/// and which agent name the status line should show. Both are answered by
/// this pair, kept in a `Vec` so the first-seen order is the display order.
/// What: `agent_id` is the seam's opaque id (empty only for a producer that
/// announced the delegation before the sub-agent existed — see
/// [`crate::event::ReplEvent::DelegationStarted`]); `agent` is the display
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delegation {
    /// The opaque per-spawn agent id this block is keyed on.
    pub agent_id: String,
    /// The delegated agent's display name.
    pub agent: String,
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
    /// Every prompt submitted this session, oldest first, with adjacent
    /// duplicates collapsed ([`Self::remember_input`]). Up/Down walk it
    /// (#8181).
    ///
    /// Why: scoped to one TUI process deliberately. Persisting across
    /// launches would need a store this crate cannot reach — it must not
    /// depend on `trusty_common` (see the crate-level doc comment's
    /// dependency-direction constraint) and has no product-specific state
    /// directory of its own — so cross-launch history stays with whichever
    /// product wants it, seeding this field at construction.
    pub history: Vec<String>,
    /// Index into `history` while navigating. `None` when not navigating.
    pub history_idx: Option<usize>,
    /// The in-progress draft stashed by [`Self::history_prev`]'s first step
    /// and restored by [`Self::history_next`] at the newest end (#8181).
    /// `None` when not navigating.
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
    /// An inline picker currently open for selection (DOC-50 §3.2/§6 Q6),
    /// staged by [`crate::commands::dispatch_forward`] when a bare `/name`
    /// submission matches `TuiEngine::picker(name)`. `None` when no picker
    /// is open. The picker WIDGET itself (navigation/rendering) is a
    /// follow-up slice's concern (DOC-50 §5 Slice 7's deliverable scopes
    /// "routing correctness", not the visual overlay); this field is the
    /// data half of that contract.
    pub active_picker: Option<PickerRequest>,
    /// The permission request currently blocking the turn (#3422), or `None`
    /// when nothing is pending.
    ///
    /// Why: a suspended tool call is a second, independent reason input must
    /// not be accepted — [`Self::busy`] cannot express it, because the
    /// backend is stopped rather than working. `Self::submit_line` refuses
    /// while this is `Some`, exactly as it refuses while `busy`, and
    /// `reduce::apply_key` routes every key to the decision bindings.
    /// What: set by [`crate::event::ReplEvent::PermissionRequested`], cleared
    /// by a matching `PermissionResolved` or by the user answering.
    pub pending_permission: Option<PendingPermission>,
    /// An answer staged for the outer driver to relay to
    /// [`crate::engine::TuiEngine::respond_permission`] (#3422). Drained by
    /// [`TuiModel::take_pending_permission_response`], same as
    /// [`Self::pending_submit`].
    pub pending_permission_response: Option<PermissionResponse>,
    /// Why the last answer to [`Self::pending_permission`] never reached the
    /// backend (#3422), rendered as an inline retry line on the reopened
    /// prompt; `None` on a prompt that has not been answered yet.
    ///
    /// Why: reopening the prompt alone would look like the backend asked
    /// twice. The operator needs to know their answer was lost, not
    /// re-asked, before choosing again.
    /// What: set by [`crate::event::ReplEvent::PermissionAnswerFailed`],
    /// cleared whenever the prompt is opened, answered, or resolved.
    pub permission_error: Option<String>,
    /// Whether a response is currently streaming in. Drives the input
    /// composer's placeholder text.
    pub busy: bool,
    /// Index into `chat` of the in-progress streaming assistant entry, if
    /// any. `None` when idle.
    pub streaming_idx: Option<usize>,
    /// Delegated sub-agents whose scrollback block is currently open, in
    /// first-seen order (#7940). Pushed by
    /// [`crate::event::ReplEvent::DelegationStarted`], removed by
    /// `DelegationFinished`. The last entry names the agent the status line
    /// shows — see [`Self::active_agent`].
    pub delegations: Vec<Delegation>,
    /// Index into `chat` of the in-progress bubble for one `(agent_id,
    /// turn_id)` stream (#7940). Separate from [`Self::streaming_idx`] —
    /// which is the single unkeyed slot
    /// [`crate::event::ReplEvent::AssistantOutput`] uses — precisely so two
    /// agents streaming at once cannot land in one bubble.
    pub agent_streams: HashMap<(String, String), usize>,
    /// Index into `chat` of the card for each in-flight tool call, keyed by
    /// the call's `id` (#4596). Inserted when a call starts; removed when its
    /// result lands in that card.
    pub tool_cards: HashMap<String, usize>,
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
    /// The most recently submitted line. Direct port of tagent's
    /// `last_prompt`, kept as a plain snapshot a product can read.
    ///
    /// Why: Up-arrow no longer reads it (#8181 moved recall to
    /// [`Self::history`]/[`Self::history_idx`]); the field stays because it
    /// is published API and costs one `String` clone per submit.
    pub last_prompt: String,
    /// The most recently seen executable-shell (bash/sh/zsh/fish) fenced
    /// code block across all assistant messages, updated by
    /// [`Self::update_last_bash_block`]. Ctrl-E pastes this block's first
    /// non-blank line into an empty input buffer — direct port of tagent's
    /// `last_bash_block` (`crates/trusty-agents/src/repl/tui/types.rs`).
    pub last_bash_block: Option<String>,
    /// The generation number of the turn currently considered "live" —
    /// mirrors `crate::run::dispatch_pending`'s `AtomicU64` counter, updated
    /// via [`TuiModel::set_current_generation`] on the SAME serial
    /// event-loop task that owns that counter (never from a spawned task).
    /// A `ReplEvent::TurnFinished { generation }` whose `generation` no
    /// longer matches this field is a stale signal from a superseded turn
    /// and is ignored — see that variant's doc comment for the TOCTOU race
    /// this by-construction design closes (the comparison happens here, in
    /// the reducer, serially — never as a spawned task's load-then-branch
    /// against the shared counter).
    pub current_generation: u64,
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
            active_picker: None,
            pending_permission: None,
            pending_permission_response: None,
            permission_error: None,
            busy: false,
            streaming_idx: None,
            delegations: Vec::new(),
            agent_streams: HashMap::new(),
            tool_cards: HashMap::new(),
            statusline: Vec::new(),
            active_workstream: None,
            quit: false,
            pending_submit: None,
            pending_cancel: false,
            last_prompt: String::new(),
            last_bash_block: None,
            current_generation: 0,
        }
    }

    /// Append a user prompt line to the chat scrollback.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.chat.push(ChatLine {
            role: ChatRole::User,
            text: text.into(),
            tool: None,
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
    /// Test: `reduce::tests::push_assistant_trims_surrounding_blanks`.
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
            tool: None,
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
    /// Test: `reduce::tests::push_assistant_updates_last_bash_block`,
    /// `reduce::tests::push_assistant_skips_error_entries_for_bash_block`.
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
            tool: None,
        });
        self.scroll_offset = 0;
    }

    /// Clear the scrollback (backs `ReplEvent::ClearScrollback`, the shared
    /// `/clear` built-in per DOC-50 §5 Slice 7).
    pub fn clear_scrollback(&mut self) {
        self.chat.clear();
        self.streaming_idx = None;
        // #7940: both keyed-stream maps index INTO `chat`, so clearing it
        // without clearing them would leave every key pointing at a stale
        // (or out-of-range) row.
        self.agent_streams.clear();
        self.delegations.clear();
        // #4596: same for the tool-call index map.
        self.tool_cards.clear();
        self.scroll_offset = 0;
    }

    /// The delegated agent whose work is currently in flight, if any
    /// (#7940).
    ///
    /// Why: the status line shows "who is working right now"; that is the
    /// most recently opened, not-yet-closed delegation, and it must revert
    /// on its own when the block closes rather than needing the engine to
    /// push a statusline update.
    /// What: the last entry of [`Self::delegations`]; `None` when no
    /// delegation is open.
    /// Test: `delegation_started_pushes_header_and_sets_active_agent`,
    /// `delegation_finished_pushes_footer_and_clears_active_agent`,
    /// `build_statusline_appends_active_agent_while_delegating`.
    pub fn active_agent(&self) -> Option<&str> {
        self.delegations.last().map(|d| d.agent.as_str())
    }

    /// Open an inline picker (DOC-50 §3.2/§6 Q6), staged by
    /// [`crate::commands::dispatch_forward`] for a bare command matching
    /// `TuiEngine::picker(name)`.
    pub fn open_picker(&mut self, request: PickerRequest) {
        self.active_picker = Some(request);
    }

    /// Close the active picker without a selection (e.g. Esc). No-op if
    /// none is open.
    pub fn close_picker(&mut self) {
        self.active_picker = None;
    }

    /// Confirm the picker item at `index`, closing the picker and returning
    /// the composed `"{dispatch_command} {selected.id}"` line
    /// ([`crate::commands::compose_selection`]) to resubmit — mirrors
    /// [`crate::model::PickerRequest`]'s documented contract. Returns `None`
    /// (leaving the picker untouched) when no picker is open or `index` is
    /// out of range, so a stale/invalid confirmation is a no-op rather than
    /// a panic.
    /// Test: `crate::commands::tests::confirm_picker_selection_composes_and_closes`,
    /// `crate::commands::tests::confirm_picker_selection_out_of_range_is_noop`.
    pub fn confirm_picker_selection(&mut self, index: usize) -> Option<String> {
        let request = self.active_picker.as_ref()?;
        let selected = request.items.get(index)?;
        let composed = crate::commands::compose_selection(request, selected);
        self.active_picker = None;
        Some(composed)
    }

    /// Push the current input onto history (with adjacent-duplicate dedup),
    /// record it as [`Self::last_prompt`] (Up-arrow recall), echo it to the
    /// scrollback, then route it (DOC-50 §5 Slice 7, §6 Q4): a recognized
    /// built-in command ([`crate::commands::route`]) is applied inline and
    /// never reaches an engine; anything else marks the app [`Self::busy`]
    /// and stages it in [`Self::pending_submit`] for the caller to forward.
    ///
    /// Why: shared by the Enter-key path and a synthesized
    /// `ReplEvent::Submit` (e.g. a picker confirmation, DOC-50 §3.2/§6 Q6)
    /// so both go through one echo+route code path rather than duplicating
    /// it — see [`reduce`]. Routing lives here rather than in `reduce`'s
    /// `apply` because [`crate::commands::route`] is pure and needs no
    /// `TuiEngine` handle (`apply` has none — see that module's doc
    /// comment), so both submission paths get built-in short-circuiting for
    /// free. `busy`/`pending_submit` are set ONLY for the non-built-in
    /// (`Route::Forward`) case — a built-in never calls
    /// `TuiEngine::handle_input`, so there is nothing to wait on and no
    /// `AssistantOutput` will ever arrive to clear `busy` again. Setting
    /// `busy` before any streamed chunk arrives (not on the first
    /// `AssistantOutput` chunk) closes the pre-first-token latency window —
    /// see the module doc comment's disclosure list.
    ///
    /// **A second turn cannot start while one is in flight**: if
    /// [`Self::busy`] is already `true`, this whole function (routing
    /// included) is a no-op — DOC-50 §5 Slice 5's "blocks user input until
    /// cancel completes" requirement, made literal rather than reasoned
    /// away. This is the authoritative guard (not just a UI nicety at the
    /// call site): without it, a second `handle_input` task starts while the
    /// first is still streaming, and with `streaming_idx` shared per-app
    /// rather than per-task, the second task's chunks splice into the first
    /// task's (now orphaned) chat entry — a real corruption bug this guard
    /// closes at the source. Callers that want to preserve the user's typed
    /// text when busy (the Enter-key path, [`reduce::apply_key`]) must check
    /// `!app.busy` themselves BEFORE consuming the input buffer, since by
    /// the time this function is reached the line is already taken out of
    /// `input_buf` and would otherwise be silently lost, not just silently
    /// ignored.
    /// What: [`crate::commands::BuiltIn::Clear`] reuses
    /// [`Self::clear_scrollback`] (the same effect
    /// `ReplEvent::ClearScrollback` produces); [`crate::commands::BuiltIn::Quit`]
    /// sets [`Self::quit`]; [`crate::commands::BuiltIn::Help`] renders
    /// [`crate::commands::render_help`] (built-ins + [`Self::commands`]) as
    /// a status line.
    /// Test: [`crate::commands::tests::submit_line_builtin_clear_clears_scrollback`],
    /// [`crate::commands::tests::submit_line_builtin_quit_sets_quit`],
    /// [`crate::commands::tests::submit_line_builtin_help_lists_commands`],
    /// [`crate::commands::tests::submit_line_forwards_non_builtin_and_marks_busy`],
    /// [`reduce::tests::apply_enter_is_noop_while_busy_and_preserves_buffer`],
    /// [`reduce::tests::apply_submit_event_is_noop_while_busy`],
    /// [`reduce::tests::submit_line_is_noop_while_a_permission_prompt_is_pending`].
    pub(crate) fn submit_line(&mut self, line: String) {
        // #3422: a suspended permission request blocks the turn for the same
        // reason `busy` does — the backend will not accept new work until it
        // is answered — so the guard is the same shape, and authoritative
        // here rather than only at the key-handling call site.
        if self.busy || self.pending_permission.is_some() {
            return;
        }
        self.remember_input(&line);
        self.last_prompt = line.clone();
        self.push_user(line.clone());
        match crate::commands::route(&line) {
            crate::commands::Route::BuiltIn(crate::commands::BuiltIn::Clear) => {
                self.clear_scrollback();
            }
            crate::commands::Route::BuiltIn(crate::commands::BuiltIn::Quit) => {
                self.quit = true;
            }
            crate::commands::Route::BuiltIn(crate::commands::BuiltIn::Help) => {
                self.push_status(crate::commands::render_help(&self.commands));
            }
            crate::commands::Route::Forward(forwarded) => {
                self.busy = true;
                self.pending_submit = Some(forwarded);
            }
        }
    }

    /// Answer the open permission prompt (#3422), staging the answer for the
    /// outer driver and releasing the input block.
    ///
    /// Why: the prompt is modal, so it must close the moment the user
    /// answers rather than waiting on the RPC — the same reasoning
    /// [`TuiModel::on_cancelled`] documents for clearing `busy` ahead of
    /// `cancel_session`. A backend that is slow to confirm must not leave
    /// the keyboard dead. The backend's own
    /// [`crate::event::ReplEvent::PermissionResolved`] is what records the
    /// outcome in the scrollback; this method records nothing, because the
    /// TUI's belief about the decision is not evidence that it was applied.
    /// What: no-op when no prompt is open. Otherwise takes the prompt and
    /// stages [`Self::pending_permission_response`] carrying it whole, so a
    /// relay that fails can hand the same request back (#3422 — see
    /// [`crate::event::ReplEvent::PermissionAnswerFailed`]). Also clears
    /// [`Self::permission_error`]: this attempt has not failed yet, and a
    /// stale retry line must not survive onto it.
    /// Test: `reduce::tests::permission_key_y_answers_allow_once`,
    /// `reduce::tests::permission_key_a_answers_allow_for_session`,
    /// `reduce::tests::permission_key_n_answers_deny`,
    /// `reduce::tests::permission_unbound_key_leaves_the_prompt_pending`,
    /// `crate::run::tests::dispatch_pending_permission_answer_failure_reopens_the_prompt`.
    pub fn answer_permission(&mut self, answer: PermissionAnswer) {
        let Some(pending) = self.pending_permission.take() else {
            return;
        };
        self.permission_error = None;
        self.pending_permission_response = Some(PermissionResponse { pending, answer });
    }

    /// Push a line onto [`Self::history`], deduping an immediate repeat
    /// (readline's `ignoredups` convention, #8181).
    ///
    /// Why: submitting the same prompt twice in a row should cost one Up
    /// press to recall, not two.
    /// What: blank/whitespace-only lines are never recorded; a line equal to
    /// the current newest entry is dropped. A repeat that is NOT adjacent is
    /// kept, so the walk order still reflects what was actually run.
    /// Test: `reduce::tests::consecutive_duplicate_submissions_are_stored_once`,
    /// `reduce::tests::submitting_appends_to_history_and_resets_the_cursor`.
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

    /// Up-arrow history navigation: step one entry older, saving the
    /// in-progress draft on the first step (#8181).
    ///
    /// Why: the draft the user was typing must survive a walk through
    /// history and come back when they walk forward past the newest entry —
    /// losing it is the failure readline's `saved_input` slot exists to
    /// prevent.
    /// What: no-op on an empty history. The first step (`history_idx ==
    /// None`) stashes `input_buf` into [`Self::saved_input`] and lands on the
    /// newest entry; later steps move one older and clamp at index 0 rather
    /// than wrapping. The cursor moves to the end of the recalled line
    /// ([`Self::set_input`]).
    /// Test: `reduce::tests::apply_up_walks_back_through_submitted_prompts`,
    /// `reduce::tests::apply_down_walks_forward_and_restores_the_draft`,
    /// `reduce::tests::apply_up_clamps_at_the_oldest_entry`.
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

    /// Down-arrow history navigation: step one entry newer, restoring the
    /// saved draft once past the newest entry (#8181).
    ///
    /// Why: the other half of [`Self::history_prev`]'s contract — walking
    /// forward off the newest entry returns the user to what they were
    /// typing, not to an empty line.
    /// What: no-op when not navigating (`history_idx == None`), so Down on a
    /// freshly typed line never clobbers it. Stepping past the newest entry
    /// clears `history_idx` and restores [`Self::saved_input`] (empty string
    /// when the draft was empty).
    /// Test: `reduce::tests::apply_down_walks_forward_and_restores_the_draft`,
    /// `reduce::tests::apply_down_is_noop_when_not_navigating_history`.
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

    /// Drain [`Self::pending_submit`] — see `crate::run::dispatch_pending`
    /// (private to `crate::run`, called from [`crate::run::run`]) for the
    /// caller that reads this after every `apply` call.
    fn take_pending_submit(&mut self) -> Option<String> {
        self.pending_submit.take()
    }

    /// Drain-and-clear [`Self::pending_cancel`].
    fn take_pending_cancel(&mut self) -> bool {
        std::mem::replace(&mut self.pending_cancel, false)
    }

    /// Drain [`Self::pending_permission_response`] (#3422) — see
    /// `crate::run::dispatch_pending` for the caller.
    fn take_pending_permission_response(&mut self) -> Option<PermissionResponse> {
        self.pending_permission_response.take()
    }

    /// Reset the in-flight-request UI state the moment a cancel is
    /// dispatched (before the `TuiEngine::cancel_session` RPC even starts) —
    /// direct parity with tagent's real cancel path, which resets
    /// `thinking`/`busy_since` synchronously ahead of `h.abort()`
    /// (`crates/trusty-agents/src/repl/tui/events.rs::process_event`). See
    /// [`crate::run::TuiModel::on_cancelled`]'s doc comment for why this is
    /// synchronous rather than waiting on the RPC.
    /// What: clears [`Self::busy`] and abandons the in-progress streaming
    /// entry index (a future response starts a fresh chat entry rather than
    /// appending to one no more chunks will ever arrive for), then pushes a
    /// "cancelled" status line.
    /// Test: `reduce::tests::apply_ctrl_c_signals_pending_cancel` covers
    /// the reducer half; `crate::run::tests` covers this method being
    /// invoked from the dispatch step.
    fn on_cancelled(&mut self) {
        self.busy = false;
        self.streaming_idx = None;
        self.push_status("cancelled");
    }

    /// Record `generation` into [`Self::current_generation`] — see
    /// [`crate::run::TuiModel::set_current_generation`]'s doc comment for
    /// why this is called only from `dispatch_pending` on the serial
    /// event-loop task, and [`ReplEvent::TurnFinished`](crate::ReplEvent::TurnFinished)'s doc comment for
    /// the race this by-construction design closes.
    /// Test: `reduce::tests::apply_turn_finished_with_stale_generation_is_ignored`,
    /// `reduce::tests::apply_turn_finished_clears_busy_and_streaming_idx_without_touching_chat`.
    fn set_current_generation(&mut self, generation: u64) {
        self.current_generation = generation;
    }
}
