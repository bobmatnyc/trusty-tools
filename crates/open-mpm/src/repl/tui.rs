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

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
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
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::statusline::{StatuslineConfig, render_statusline};

/// Whether the active agent is user-scoped (ctrl, personas) or project-scoped (pm + project).
///
/// Why: User-level agents (ctrl, izzie, cto-assistant) respond as themselves — cyan label.
///   Project-level agents (PM connected to a project) respond on behalf of the project —
///   yellow/amber label to signal project scope vs. personal scope at a glance.
/// What: Carried in `ReplApp`; updated via `ReplEvent::AgentScopeChanged`.
/// Test: `repl_app_agent_scope_default`, `agent_scope_label_color_user`, `agent_scope_label_color_project`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AgentScope {
    /// User-level agent (ctrl, izzie, cto-assistant). Label color = cyan.
    #[default]
    User,
    /// PM connected to a specific project. Label color = yellow.
    Project,
}

/// Which slash command opened the picker — drives selection routing on Enter.
///
/// Why: A single overlay handles both `/model` and `/provider`; the `kind`
/// disambiguates so `handle_picker_key` can synthesize the right slash command
/// when the user confirms a selection.
/// What: `Model` -> Enter dispatches `/model <selected>`. `Provider` ->
/// Enter dispatches `/provider <selected>`.
/// Test: `repl_app_picker_*` unit tests.
#[derive(Debug, Clone, PartialEq)]
pub enum PickerKind {
    Model,
    Provider,
}

/// State of an open picker overlay.
///
/// Why: Keeping picker state in `ReplApp` (vs. a top-level enum) lets the
/// existing event loop drive it without refactoring the dispatch.
/// What: `items` is the list of choices; `selected` is the highlighted index;
/// `title` renders in the popup border.
/// Test: `repl_app_picker_navigation_wraps`, `repl_app_picker_enter_clears_picker`.
#[derive(Debug, Clone)]
pub struct PickerState {
    pub items: Vec<String>,
    pub selected: usize,
    pub title: String,
    pub kind: PickerKind,
}

/// One rendered chat entry in the scrollback.
#[derive(Debug, Clone)]
pub struct ChatLine {
    pub role: ChatRole,
    pub text: String,
}

/// Source/role of a chat line — drives prefix glyph + colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// User input — green `❯` prefix.
    User,
    /// Assistant response — orange `⏺` prefix.
    Assistant,
    /// Error response — orange `⏺` prefix, red body.
    Error,
    /// `[open-mpm] …` informational status line — green `[open-mpm]` prefix.
    Status,
}

/// All mutable state the render loop needs.
///
/// Why: Keeping this separate from the rendering loop lets unit tests drive
/// transitions without instantiating a Terminal, mirroring the pattern in
/// `src/debugger/tui.rs::DebugApp`.
#[derive(Debug, Clone)]
pub struct ReplApp {
    /// Scrollback history of user/assistant exchanges.
    pub chat: Vec<ChatLine>,
    /// Current input line being edited.
    pub input_buf: String,
    /// Byte offset within `input_buf`.
    pub cursor_pos: usize,
    /// Number of lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll_offset: usize,
    /// Show `[thinking...]` indicator while the LLM is busy.
    pub thinking: bool,
    /// Curated status lines rendered below the last user prompt while the
    /// LLM is busy (e.g. "Delegating to engineer…", "engineer · running…").
    /// Cleared when the assistant response arrives.
    pub thinking_lines: Vec<String>,
    /// Project label drawn in the prompt (e.g. `ctrl`, `izzie`).
    pub project_name: String,
    /// One-line startup status (e.g. "All systems go").
    pub status_line: Option<String>,
    /// Recent git commit summaries shown in the banner panel.
    pub git_commits: Vec<String>,
    /// User label shown in the banner left panel.
    pub user_label: String,
    /// Whether to show the welcome banner block. Hidden once chat is non-empty
    /// to maximise scrollback room.
    pub show_banner: bool,
    /// Scope of the active agent — User (ctrl/personas) or Project (PM + project path).
    /// Drives label color in the chat: cyan for user-level, yellow for project-level.
    pub agent_scope: AgentScope,
    /// Quit signal — set on Ctrl-D or `/exit`.
    pub quit: bool,
    /// In-memory history (up arrow recall).
    pub history: Vec<String>,
    /// Index into `history` while up-arrow scrolling. `None` when not in
    /// history navigation mode.
    pub history_idx: Option<usize>,
    /// Saved current input while navigating history.
    pub saved_input: Option<String>,
    /// Cumulative prompt (input) tokens consumed during this REPL session.
    /// Why: Live-tracks token usage in the input bar so users can see the
    /// running cost without checking logs.
    /// What: Incremented by `ReplEvent::TokenUpdate`; reset on `/clear`.
    /// Test: `repl_app_token_update_accumulates`.
    pub tokens_in: u64,
    /// Cumulative completion (output) tokens produced during this REPL session.
    pub tokens_out: u64,
    /// Effective model id rendered by the statusline `model` segment.
    /// Updated by `/model` and `/provider local` (which echoes selected ollama model).
    pub model_name: String,
    /// Effective provider label rendered by the statusline `provider` segment.
    /// Updated by `/provider`.
    pub provider_name: String,
    /// Working directory shown by the `workdir` segment (last component used).
    pub working_dir: String,
    /// Current git branch (probed once at startup); None outside a git repo.
    pub git_branch: Option<String>,
    /// Whether the git working tree has uncommitted changes.
    pub git_dirty: bool,
    /// Statusline segment configuration loaded from `.open-mpm/config.toml`.
    pub statusline_config: StatuslineConfig,
    /// Wall-clock anchor for the `elapsed` segment.
    pub session_start: std::time::Instant,
    /// Active picker overlay (model/provider). `None` when no picker is open.
    ///
    /// Why: When `Some`, `handle_key` routes ALL key events to the picker
    /// modal handler so navigation/dismiss don't accidentally edit the input
    /// buffer underneath.
    /// What: Carries items, selection index, title, and kind.
    /// Test: `repl_app_picker_*`.
    pub picker: Option<PickerState>,
    /// Selection captured when the user pressed Enter on a picker. The event
    /// loop drains this AFTER `handle_key` returns and synthesizes a
    /// `Submit("/model …")` / `Submit("/provider …")` so the existing slash
    /// handler does the actual override mutation.
    ///
    /// Why: `handle_key` only has access to `&mut ReplApp`; it can't reach
    /// the `tx` channel or the `OpenMpmRepl` handler. Stashing the choice
    /// here keeps the transition lock-free and trivially testable.
    /// What: `(kind, selected_string)`. Cleared by the event loop after
    /// emitting the synthetic Submit.
    /// Test: `repl_app_picker_enter_sets_pending_selection`.
    pub pending_picker_selection: Option<(PickerKind, String)>,
    /// Cached ollama model list from the most recent `/provider local` probe.
    ///
    /// Why: When the user opens the model picker after `/provider local`,
    /// we want to show the actual locally-pulled models, not the hardcoded
    /// Anthropic list. Cached so the picker opens instantly without re-probing.
    /// What: Populated by `ReplEvent::OllamaModelsLoaded`.
    /// Test: Manual via `/provider local` + `/model`.
    pub ollama_models: Vec<String>,
    /// Last submitted prompt — restored to the input buffer when the user
    /// presses Up-arrow (idle: simple recall; busy: cancel + restore).
    ///
    /// Why: Single-level history recall lets users edit/resubmit an
    /// accidentally-fired prompt without trawling the full history.
    /// What: Set by `process_event` before `push_user` runs. Restored to
    /// `input_buf` by `handle_key` on KeyCode::Up.
    /// Test: `repl_app_up_arrow_recalls_last_prompt`,
    ///   `repl_app_up_arrow_when_busy_signals_cancel`.
    pub last_prompt: String,
    /// Set by `handle_key` when the user presses Up-arrow while the LLM is
    /// busy. The event loop drains this AFTER `handle_key` returns and
    /// aborts the in-flight JoinHandle (which lives outside `ReplApp`).
    ///
    /// Why: `handle_key` only has `&mut ReplApp`; it can't reach the
    /// JoinHandle. Stashing a flag here keeps the cancel transition
    /// lock-free and trivially testable.
    /// What: Set to `true` on Up-arrow when `thinking == true`. Cleared
    /// by `process_event` after issuing `JoinHandle::abort`.
    /// Test: `repl_app_up_arrow_when_busy_signals_cancel`.
    pub pending_cancel: bool,
    /// Wall-clock anchor for the activity-area `Xs` elapsed timer.
    ///
    /// Why: Users want immediate feedback on how long the current LLM call
    /// has been running so they can tell the difference between a fast turn
    /// and a hang. Pinned `Some(Instant::now())` whenever a Submit lands and
    /// cleared on `LlmResponse` / `LlmThinking(false)`.
    /// What: `None` when idle. Driven by `process_event`; never mutated by
    /// `handle_key`.
    /// Test: `repl_app_busy_since_set_on_submit`,
    /// `repl_app_busy_since_cleared_on_response`.
    pub busy_since: Option<std::time::Instant>,
    /// Streaming response preview shown above the input bar while the LLM
    /// works. Populated from `ThinkingStep` events (closest thing to
    /// streaming this harness has today); cleared on `LlmResponse` /
    /// `LlmThinking(false)`.
    ///
    /// Why: Surfaces the in-progress response *near the input* so users see
    /// the assistant taking shape without scrolling up to the chat history.
    /// What: Plain string; the renderer truncates to 1-2 lines.
    /// Test: `repl_app_streaming_preview_clears_on_response`.
    pub streaming_preview: String,
    /// Cost (USD) accumulated by *prior* sessions today, loaded once at
    /// startup from `.open-mpm/state/usage.json`. The session's own running
    /// cost is computed from `tokens_in`/`tokens_out`; daily total = this
    /// plus session cost.
    ///
    /// Why: Lets the statusline show "today" alongside "session" without
    /// double-counting the in-flight session.
    /// What: 0.0 on a fresh day; carried forward from disk otherwise.
    /// Survives `/clear` (which only zeroes session tokens).
    /// Test: `repl_app_token_reset_preserves_daily_cost_start`.
    pub daily_cost_start: f64,
    /// Project dir whose `.open-mpm/state/usage.json` we read/write.
    ///
    /// Why: Daily usage is per-project (matches every other state file).
    /// What: Resolved at startup from the harness's project_dir.
    pub usage_project_dir: PathBuf,
    /// Last time we flushed the daily usage file. Used to throttle writes
    /// to at most once per `USAGE_WRITE_INTERVAL`.
    ///
    /// Why: Token updates can arrive in tight bursts (one per chunk); we
    /// don't want to fsync on every event.
    /// What: `None` until first write attempt.
    pub last_usage_write: Option<std::time::Instant>,
    /// Inline multiple-choice picker options.
    ///
    /// Why: When the LLM responds with a numbered/bulleted list of choices,
    /// we surface them inline below the input row so the user can arrow-
    /// navigate + Enter to pick instead of retyping. Empty vec = no picker
    /// active (the layout collapses to zero rows in that case).
    /// What: Each entry is the raw choice text. Last entry is conventionally
    /// "Other (type your own)" so the user can opt out and free-type.
    /// Test: `inline_choice_picker_*` unit tests below.
    pub choices: Vec<String>,
    /// Currently highlighted index in `choices`.
    pub choice_cursor: usize,
    /// Optional context tag for `choices`. When set, the Enter handler
    /// dispatches a context-specific action instead of placing the
    /// selected text into the input buffer.
    ///
    /// Why: `/switch` (no arg) populates `choices` with persona display
    /// names and wants Enter to directly submit `/switch <name>` rather
    /// than requiring a second Enter to send the inserted text. The
    /// context tag lets the Enter handler distinguish this case from a
    /// generic LLM-offered choice list (which still uses the
    /// "insert into input" behavior).
    /// What: `Some("switch")` → Enter emits a synthetic Submit
    /// `/switch <selected>`. `None` → Enter inserts selection into the
    /// input buffer (legacy free-type behavior).
    /// Test: `inline_choices_switch_context_dispatches_submit`.
    pub choices_context: Option<String>,
    /// Synthetic submission queued by `handle_key` (e.g. when Enter on a
    /// `choices_context = "switch"` list selects a persona). Drained by
    /// the event loop after `handle_key` returns and translated into a
    /// `ReplEvent::Submit`.
    ///
    /// Why: `handle_key` only has `&mut ReplApp`; it can't reach the
    /// `tx` channel. Mirrors the `pending_picker_selection` pattern.
    /// What: `Some(line)` → event loop emits `Submit(line)` and clears
    /// this field. `None` → no-op.
    /// Test: `inline_choices_switch_context_dispatches_submit`.
    pub pending_submit: Option<String>,
    /// Monotonic frame counter incremented on every render tick (~10/sec).
    ///
    /// Why: Drives the activity spinner glyph cycling — without a tick-based
    /// index the `✻` glyph stays static and the activity strip looks frozen
    /// even when work is happening. Mirrors Claude Code's braille animation.
    /// What: `u64`, incremented in `event_loop` on each tick. Wraps freely.
    /// Test: `activity_row1_spinner_animates_with_tick`.
    pub tick_count: u64,
    /// Monotonic palette offset advanced each tick to flow the rust-rainbow
    /// shimmer across the activity spinner line while busy.
    ///
    /// Why: A static palette would just colorize the line; advancing the
    /// offset each tick makes the colors slide left-to-right like a neon
    /// sign, signaling "actively working" without distracting motion.
    /// What: `usize`, incremented in `event_loop` on each tick. Wraps freely.
    /// Test: `rainbow_spans_advances_with_tick`.
    pub rainbow_tick: usize,
    /// Live count of TM (tmux) sessions, rendered in the statusline as
    /// `TM: <n> sessions`. Updated by `ReplEvent::TmSessionCount` (emitted
    /// after the startup reconcile and by the background monitor).
    /// Why: TM is always-on infrastructure (#319); surfacing the count gives
    /// users instant awareness of how many sessions the harness manages.
    /// What: Plain `usize`. 0 → render as `TM: 0`.
    /// Test: `rich_statusline_includes_tm_segment`.
    pub tm_session_count: usize,
    /// Live count of TM sessions whose adapter is `claude-mpm` (#331).
    ///
    /// Why: Surfaces how many of the managed TM sessions are running the
    /// claude-mpm PM orchestrator specifically — distinct from the overall
    /// `TM:` count. Renders as a separate styled segment when > 0.
    /// What: Plain `usize`. 0 → segment is suppressed.
    /// Test: `rich_statusline_includes_claude_mpm_segment_when_present`.
    pub claude_mpm_session_count: usize,
    /// Last-known maximum scroll offset, captured during render (#329).
    ///
    /// Why: `scroll()` needs to clamp the offset so it can't accumulate
    /// beyond the actual scrollback height. The true max depends on layout
    /// (visible area + line count), which is only known at render time. We
    /// stash it on each draw and consult it here.
    /// What: `Arc<AtomicUsize>`; updated in `draw_chat` (which has only an
    /// immutable snapshot reference). The `Arc` keeps the underlying atomic
    /// shared between `ReplApp` clones (the render snapshot) and the
    /// authoritative instance behind the runtime mutex, so the snapshot's
    /// write is visible the next time `scroll()` runs. 0 means no scrollback.
    /// Test: `repl_app_scroll_clamps_at_max_offset`.
    pub last_max_scroll: Arc<std::sync::atomic::AtomicUsize>,
    /// Local inference model name for the statusline (#319).
    ///
    /// Why: When local Ollama inference is enabled and reachable, the
    /// statusline should surface the active model name so users know at a
    /// glance that local inference is live, without needing to run `/local`.
    /// What: `Some("qwen3:30b")` (vendor prefix stripped) when enabled+available;
    /// `None` when disabled or Ollama is unreachable (remote fallback active).
    /// Test: `rich_statusline_includes_local_model_when_set`.
    pub local_model: Option<String>,
    /// Content of the most recently rendered bash/sh code block (#321).
    ///
    /// Why: Lets users grab a suggested shell command into their input buffer
    /// with a single keystroke (Ctrl+E on empty input) instead of mouse-
    /// selecting and copy-pasting from the chat scrollback.
    /// What: Updated by `update_last_bash_block` whenever a new assistant
    /// message lands in `chat`. Holds the body lines of the *last*
    /// executable-shell fenced block (` ```bash `, ` ```sh `, ` ```zsh `,
    /// ` ```fish `) seen across all assistant messages. `None` when no such
    /// block has been seen yet.
    /// Test: `repl_app_last_bash_block_*` unit tests.
    pub last_bash_block: Option<String>,
}

/// Throttle window for daily-usage disk writes.
pub const USAGE_WRITE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Events that can mutate `ReplApp`.
///
/// Why: A single channel funnel for both terminal input AND background LLM
/// progress eliminates the ordering bugs that plagued the crossterm path
/// (sub-agent stderr beating the response print, `[thinking...]` ghost
/// frames in scrollback, etc.).
#[derive(Debug, Clone)]
pub enum ReplEvent {
    /// Completed assistant response text (markdown-stripped, ready to render).
    LlmResponse { text: String, is_error: bool },
    /// Toggle the `[thinking...]` indicator.
    LlmThinking(bool),
    /// Append a curated thinking-step line below the last user prompt
    /// (e.g. "Delegating to engineer…"). Replaced by `LlmResponse` when the
    /// final response arrives.
    ThinkingStep(String),
    /// Append a status line above the input bar (e.g. "Switched to: izzie").
    StatusMessage(String),
    /// Update the prompt label (after `/agent` or `/connect`).
    LabelChanged(String),
    /// Update the agent scope when switching between user-level and project-level agents.
    /// Drives label color: cyan for User, yellow for Project.
    AgentScopeChanged(AgentScope),
    /// Raw terminal key event from the input thread.
    Key(KeyEvent),
    /// Terminal was resized — repaint will pick up new dims.
    ///
    /// Why: The repaint loop only needs the signal that a resize happened to
    /// refresh terminal dimensions via `crossterm::terminal::size()`; the
    /// carried width/height are kept for future selective-redraw paths but
    /// are not currently read at the match site.
    #[allow(dead_code)]
    Resize(u16, u16),
    /// User submitted a line. Dispatched by the input handler so the outer
    /// task can forward to the LLM. The state update (echoing the user line
    /// to the chat buffer) is performed BEFORE this is emitted, so handlers
    /// don't have to.
    Submit(String),
    /// Live token usage update from an LLM call. Accumulated into
    /// `ReplApp.tokens_in` / `tokens_out` and rendered on the input bar.
    ///
    /// Why: Users want to see token spend climb in real time as agents run,
    /// without scraping logs or `usage.jsonl`.
    /// What: `prompt` and `completion` are deltas added to the running totals.
    /// Test: `repl_app_token_update_accumulates`.
    TokenUpdate { prompt: u64, completion: u64 },
    /// Reset cumulative token counters to zero (used by `/clear`).
    TokenReset,
    /// Update the statusline's effective model+provider after a slash override.
    ///
    /// Why: `/model` and `/provider` mutate routing state inside `OpenMpmRepl`;
    /// the statusline must refresh to reflect what the next dispatch will use.
    /// What: Both fields are replaced verbatim.
    /// Test: `repl_app_statusline_update_event` integration via process_event.
    StatuslineUpdate { model: String, provider: String },
    /// Open a picker overlay with the given items.
    ///
    /// Why: `/model` and `/provider` (no arg) emit this so the user gets an
    /// interactive list instead of plain text. Funneled through `ReplEvent`
    /// so `try_handle_slash` (which lacks `tx`) can be skipped while the
    /// dispatcher (which has `tx`) drives the modal.
    /// What: `items` are the menu rows; `title` renders in the border;
    /// `kind` decides which slash command Enter dispatches.
    /// Test: `process_event_open_picker_sets_state`.
    OpenPicker {
        items: Vec<String>,
        title: String,
        kind: PickerKind,
    },
    /// Cache an ollama model list (sent after a successful `/provider local`
    /// probe). The next `/model` picker will show these instead of the
    /// hardcoded Anthropic list.
    OllamaModelsLoaded(Vec<String>),
    /// Populate the inline (flat-list) choice picker shown below the input
    /// row. Replaces the previous modal-overlay path for `/switch` (no arg).
    ///
    /// Why: Modal overlays for short fixed lists feel heavy and break the
    /// "everything stays near the input" UX. The inline list is just a
    /// thin row of arrow-navigable items right under the prompt.
    /// What: `items` are the visible choices; `context` tags the list so
    /// the Enter handler knows whether to insert into the input buffer
    /// (None) or directly dispatch a slash command (e.g. `Some("switch")`
    /// → `/switch <selected>`).
    /// Test: `process_event_set_choices_populates_inline_picker`.
    SetChoices {
        items: Vec<String>,
        context: Option<String>,
    },
    /// Update the statusline's TM session count (#319). Emitted on startup
    /// after the initial reconcile and by background monitor ticks when the
    /// session count changes.
    ///
    /// Why: The constructor sites currently live behind feature-gated monitor
    /// hooks that may not compile into every build profile; keep the variant
    /// (and its match arm) so the wiring is ready when the hooks are enabled.
    #[allow(dead_code)]
    TmSessionCount(usize),
    /// Update the statusline's claude-mpm session count (#331). Emitted at
    /// the same points as `TmSessionCount` — counts only sessions whose
    /// `adapter_type == AdapterType::ClaudeMpm`.
    #[allow(dead_code)]
    ClaudeMpmSessionCount(usize),
    /// Mouse-wheel scroll delta (#329). Negative = older (up), positive =
    /// newer (down). Forwarded from the key reader thread which sees
    /// `MouseEventKind::ScrollUp` / `ScrollDown` events arriving via
    /// `EnableMouseCapture`.
    Scroll(isize),
}

impl ReplApp {
    pub fn new(project_name: String, user_label: String) -> Self {
        Self {
            chat: Vec::new(),
            input_buf: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            thinking: false,
            thinking_lines: Vec::new(),
            project_name,
            status_line: None,
            git_commits: Vec::new(),
            user_label,
            show_banner: true,
            agent_scope: AgentScope::default(),
            quit: false,
            history: Vec::new(),
            history_idx: None,
            saved_input: None,
            tokens_in: 0,
            tokens_out: 0,
            model_name: String::new(),
            provider_name: String::new(),
            working_dir: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            git_branch: None,
            git_dirty: false,
            statusline_config: StatuslineConfig::default(),
            session_start: std::time::Instant::now(),
            picker: None,
            pending_picker_selection: None,
            ollama_models: Vec::new(),
            last_prompt: String::new(),
            pending_cancel: false,
            busy_since: None,
            streaming_preview: String::new(),
            daily_cost_start: 0.0,
            usage_project_dir: std::env::current_dir().unwrap_or_default(),
            last_usage_write: None,
            choices: Vec::new(),
            choice_cursor: 0,
            choices_context: None,
            pending_submit: None,
            tick_count: 0,
            rainbow_tick: 0,
            tm_session_count: 0,
            claude_mpm_session_count: 0,
            last_max_scroll: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            local_model: None,
            last_bash_block: None,
        }
    }

    /// Append a user prompt line to the chat scrollback.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.chat.push(ChatLine {
            role: ChatRole::User,
            text: text.into(),
        });
        // Banner stays visible — it now lives in the chat scroll buffer
        // (see `banner_lines` + `draw_chat`) and scrolls off the top
        // naturally as content grows.
        // Always pin to the newest line on a new entry.
        self.scroll_offset = 0;
    }

    /// Append an assistant response line to the chat scrollback.
    ///
    /// Why: LLM responses frequently arrive with leading/trailing blank lines
    /// (extra `\n\n` from the model). Rendering them verbatim leaves visible
    /// dead space below the response in the chat scrollback, which the user
    /// reads as a UI bug. Strip those blanks once at the boundary so every
    /// downstream renderer sees clean content.
    /// What: Trim leading and trailing whitespace-only lines (and trailing
    /// whitespace) from `text` before appending; preserve interior blank lines.
    /// Test: `push_assistant_trims_surrounding_blanks` asserts that leading
    /// and trailing `\n` characters and whitespace-only lines are removed.
    pub fn push_assistant(&mut self, text: impl Into<String>, is_error: bool) {
        let role = if is_error {
            ChatRole::Error
        } else {
            ChatRole::Assistant
        };
        let raw: String = text.into();
        let trimmed = trim_surrounding_blank_lines(&raw);
        // Drop ALL interior blank lines so paragraphs flow without gaps in
        // the chat panel. Keeps non-tui consumers (logs, exports) consistent
        // with what the user sees on screen.
        let collapsed = strip_interior_blank_lines(&trimmed);
        // Inline-choice detection: if the response looks like a numbered or
        // bulleted list with ≥2 items, surface an inline picker. Errors are
        // never offered as choices (they're not menus). Only updates state
        // for non-error assistant messages so previous picker state is reset
        // even when no new choices appear.
        if !is_error {
            match detect_choices(&collapsed) {
                Some(mut items) => {
                    items.push("Other (type your own)".to_string());
                    self.choices = items;
                    self.choice_cursor = 0;
                    // LLM-offered list — generic context (insert into input).
                    self.choices_context = None;
                }
                None => {
                    self.choices.clear();
                    self.choice_cursor = 0;
                    self.choices_context = None;
                }
            }
        }
        self.chat.push(ChatLine {
            role,
            text: collapsed,
        });
        // Banner stays visible — see `push_user` for the rationale.
        self.scroll_offset = 0;
        // Refresh the Ctrl+E paste buffer (#321) — scan all assistant
        // messages and remember the most recently rendered shell block.
        self.update_last_bash_block();
    }

    /// Scan `self.chat` and update `self.last_bash_block` with the last
    /// executable-shell fenced block seen across all assistant messages (#321).
    ///
    /// Why: Ctrl+E (with empty input) pastes that block back into the input
    /// buffer. Recomputing on every assistant push keeps the source of truth
    /// (chat) and the cached buffer in sync without bookkeeping at multiple
    /// call sites.
    /// What: Iterates assistant entries newest→oldest; on the first one that
    /// contains an executable shell fence, stores its last block and stops.
    /// Errors and user/status entries are skipped.
    /// Test: `repl_app_last_bash_block_updates_on_push`.
    pub fn update_last_bash_block(&mut self) {
        for entry in self.chat.iter().rev() {
            if entry.role != ChatRole::Assistant {
                continue;
            }
            if let Some(block) = extract_last_shell_block(&entry.text) {
                self.last_bash_block = Some(block);
                return;
            }
        }
        self.last_bash_block = None;
    }

    /// Append a `[open-mpm]`-style status line.
    pub fn push_status(&mut self, text: impl Into<String>) {
        self.chat.push(ChatLine {
            role: ChatRole::Status,
            text: text.into(),
        });
        self.scroll_offset = 0;
    }

    /// Push the current input onto the in-memory history (with dedup).
    pub fn remember_input(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.history.last().map(|s| s.as_str()) == Some(line) {
            return;
        }
        self.history.push(line.to_string());
    }

    /// Set the input buffer + cursor in one shot. Used by history navigation
    /// to replace the buffer with a recalled entry.
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

    /// Move cursor left one char-boundary.
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

    /// Move cursor right one char-boundary.
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
    ///
    /// Why: Exercised by `repl_app_history_prev_next`; the production key-handler
    /// currently routes Up directly to the underlying `history` index, but the
    /// helper is kept (and tested) so the navigation logic stays unit-coverable.
    #[allow(dead_code)]
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

    /// Take the current input buffer and reset the editor state.
    /// Returns the submitted line or `None` if the buffer was empty.
    pub fn take_input(&mut self) -> Option<String> {
        let trimmed = self.input_buf.trim();
        if trimmed.is_empty() {
            return None;
        }
        let out = std::mem::take(&mut self.input_buf);
        self.cursor_pos = 0;
        self.history_idx = None;
        self.saved_input = None;
        // Submitting any message dismisses the inline choice picker — the
        // user has either accepted a choice (which already cleared it) or
        // chosen to free-type, so no stale picker should outlive the send.
        self.choices.clear();
        self.choice_cursor = 0;
        self.choices_context = None;
        Some(out)
    }

    /// Apply a scroll delta. Negative = older (up), positive = newer (down).
    ///
    /// Why (#329): unbounded accumulation made mouse-wheel scroll-up "stick"
    /// past the top of the scrollback — every subsequent scroll-down had to
    /// burn off the phantom offset before any visible movement. Clamping
    /// against the last-rendered max keeps the scroll position honest.
    /// What: After applying delta with saturating arithmetic, clamp to
    /// `[0, last_max_scroll]`. The cap is updated by `draw_chat` each frame.
    /// Test: `repl_app_scroll_clamps_at_max_offset`.
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

/// Handler trait for slash commands and chat dispatch.
///
/// Why: Keeps the ratatui loop ignorant of REPL business logic (slash command
/// table, persona switching, ctrl socket forwarding). The outer `OpenMpmRepl`
/// owns those concerns and implements this trait — the TUI just routes
/// submissions and surfaces results back through the event channel.
#[async_trait::async_trait]
pub trait ReplHandler: Send + Sync {
    /// Process a submitted line. Returns `Ok(true)` to keep looping,
    /// `Ok(false)` to quit. Pushes assistant/status lines through `tx`.
    async fn handle_input(&self, line: String, tx: UnboundedSender<ReplEvent>) -> Result<bool>;
}

/// Run the ratatui-based REPL to completion.
///
/// Why: Stack-safe RAII boundary — even a panic inside the event loop restores
/// the terminal via `restore_terminal`.
/// What: Enters alt-screen + raw mode, spawns the key reader task, spawns the
/// handler dispatcher, runs the render loop until `app.quit` is true.
/// Test: Integration via `scripts/tmux-repl-test.sh`.
pub struct ReplStartup {
    pub project_name: String,
    pub user_label: String,
    pub git_commits: Vec<String>,
    pub initial_status: Option<String>,
    pub initial_history: Vec<String>,
    pub initial_scope: AgentScope,
    pub model_name: String,
    pub provider_name: String,
    pub working_dir: String,
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    pub statusline_config: StatuslineConfig,
    /// Project dir whose `.open-mpm/state/usage.json` we'll read/write for
    /// daily cost accumulation.
    pub project_dir: PathBuf,
    /// System messages to push into the chat scrollback before the first
    /// frame renders (#319). Used by the TM startup-reconcile to surface
    /// "discovered N session(s)" without a separate event flush.
    pub initial_chat_messages: Vec<String>,
    /// Initial TM session count for the statusline `TM:` segment (#319).
    /// Updated by `ReplEvent::TmSessionCount` after subsequent reconciles.
    pub tm_session_count: usize,
    /// Initial count of TM sessions whose adapter is `claude-mpm` (#331).
    /// Updated by `ReplEvent::ClaudeMpmSessionCount` after subsequent reconciles.
    pub claude_mpm_session_count: usize,
    /// Local inference model name for the statusline (#319).
    /// `Some("qwen3:30b")` (vendor prefix stripped) when enabled+available;
    /// `None` when disabled or Ollama not reachable.
    pub local_model: Option<String>,
}

pub async fn run_tui<H: ReplHandler + 'static>(
    startup: ReplStartup,
    handler: Arc<H>,
) -> Result<()> {
    let mut terminal = setup_terminal()?;

    let mut app = ReplApp::new(startup.project_name, startup.user_label);
    app.git_commits = startup.git_commits;
    app.status_line = startup.initial_status;
    app.history = startup.initial_history;
    app.agent_scope = startup.initial_scope;
    app.model_name = startup.model_name;
    app.provider_name = startup.provider_name;
    app.working_dir = startup.working_dir;
    app.git_branch = startup.git_branch;
    app.git_dirty = startup.git_dirty;
    app.statusline_config = startup.statusline_config;
    app.usage_project_dir = startup.project_dir.clone();
    app.tm_session_count = startup.tm_session_count;
    app.claude_mpm_session_count = startup.claude_mpm_session_count;
    app.local_model = startup.local_model;
    // #319: surface any startup chat messages (e.g. TM reconcile result)
    // before the first frame renders so users see them immediately.
    for msg in startup.initial_chat_messages {
        app.push_status(msg);
    }
    // Load any cost already accumulated today (from prior sessions). When
    // the file is missing or dated to a previous day, this returns 0.0.
    let initial = crate::usage::daily::load(&startup.project_dir);
    app.daily_cost_start = initial.cost_usd;

    let app = Arc::new(Mutex::new(app));
    // Currently in-flight handler task (if any). Stored OUTSIDE `ReplApp`
    // because `JoinHandle` is not `Clone` and `ReplApp` derives Clone for
    // the snapshot-on-render pattern. Aborted by Up-arrow when busy (see
    // #XXX); cleared by the spawned task when it completes.
    let current_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::unbounded_channel::<ReplEvent>();

    // #368: Non-blocking update check. Spawn a task that hits GitHub
    // releases; when a newer version is found, surface it as a status
    // message in the chat (and the user can run `/update` to upgrade).
    {
        let update_tx = tx.clone();
        tokio::spawn(async move {
            if let Some(info) = crate::update::check_for_update().await {
                let msg = format!(
                    "Update available: open-mpm v{} (you have v{}) — run /update to upgrade",
                    info.latest_version,
                    env!("CARGO_PKG_VERSION")
                );
                let _ = update_tx.send(ReplEvent::StatusMessage(msg));
            }
        });
    }

    // Spawn key event reader thread. Crossterm's blocking `event::read` is
    // happiest on a dedicated OS thread so it doesn't park the tokio runtime.
    let key_tx = tx.clone();
    let key_thread = std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(CtEvent::Key(k)) => {
                    if k.kind != KeyEventKind::Press && k.kind != KeyEventKind::Repeat {
                        continue;
                    }
                    if key_tx.send(ReplEvent::Key(k)).is_err() {
                        break;
                    }
                }
                Ok(CtEvent::Resize(c, r)) => {
                    if key_tx.send(ReplEvent::Resize(c, r)).is_err() {
                        break;
                    }
                }
                // #329: Mouse-wheel scroll. EnableMouseCapture is on at startup
                // so these events arrive — previously they were dropped by the
                // catch-all `Ok(_) => continue` arm. -3/+3 mirrors a typical
                // 3-line wheel notch (PageUp/Down use 10).
                Ok(CtEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    ..
                })) => {
                    if key_tx.send(ReplEvent::Scroll(-3)).is_err() {
                        break;
                    }
                }
                Ok(CtEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    ..
                })) => {
                    if key_tx.send(ReplEvent::Scroll(3)).is_err() {
                        break;
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });

    let result = event_loop(
        &mut terminal,
        app.clone(),
        current_task.clone(),
        tx.clone(),
        rx,
        handler,
    )
    .await;

    restore_terminal(&mut terminal).ok();
    drop(tx); // drop the sender so the key reader exits cleanly
    let _ = key_thread.join();

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

async fn event_loop<H: ReplHandler + 'static>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: Arc<Mutex<ReplApp>>,
    current_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    tx: UnboundedSender<ReplEvent>,
    mut rx: UnboundedReceiver<ReplEvent>,
    handler: Arc<H>,
) -> Result<()> {
    // Initial paint.
    {
        let snap = app.lock().await.clone();
        terminal.draw(|f| draw(f, &snap))?;
    }

    // Periodic tick keeps the frame fresh even when nothing is happening,
    // so stray stderr writes from background tasks (logging, MCP init) get
    // overwritten on the next paint instead of permanently corrupting the
    // alt-screen.
    // 100ms tick keeps the activity-area spinner animating smoothly and the
    // `Xs` elapsed timer ticking in real time. Idle frames are cheap (full
    // diff) so this doesn't burn meaningful CPU.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                // Bump the tick counter BEFORE snapshotting so the spinner
                // index advances every frame (10fps). u64 wraps to 0 after
                // ~5.8B years at 10Hz — practically infinite.
                let snap = {
                    let mut a = app.lock().await;
                    a.tick_count = a.tick_count.wrapping_add(1);
                    a.rainbow_tick = a.rainbow_tick.wrapping_add(1);
                    a.clone()
                };
                terminal.draw(|f| draw(f, &snap))?;
                if snap.quit {
                    return Ok(());
                }
                continue;
            }
            ev = rx.recv() => {
                let Some(ev) = ev else { return Ok(()); };
                process_event(ev, &app, &current_task, &tx, &handler).await;
                let snap = app.lock().await.clone();
                terminal.draw(|f| draw(f, &snap))?;
                if snap.quit {
                    return Ok(());
                }
            }
        }
    }
}

async fn process_event<H: ReplHandler + 'static>(
    ev: ReplEvent,
    app: &Arc<Mutex<ReplApp>>,
    current_task: &Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    tx: &UnboundedSender<ReplEvent>,
    handler: &Arc<H>,
) {
    match ev {
        ReplEvent::Key(k) => {
            // Run the key through the editor, then drain any picker
            // selection that Enter just produced (so the synthetic Submit
            // is dispatched in this same event tick) and any pending
            // cancel signal (Up-arrow while busy → abort the in-flight
            // handler task).
            let (submit, picker_choice, cancel, pending_submit) = {
                let mut a = app.lock().await;
                let s = handle_key(&mut a, k);
                let pc = a.pending_picker_selection.take();
                let cancel = std::mem::replace(&mut a.pending_cancel, false);
                let ps = a.pending_submit.take();
                (s, pc, cancel, ps)
            };
            if cancel {
                // Up-arrow while LLM was busy: abort the in-flight task,
                // surface a status line so the user knows the cancel
                // landed, and clear the busy flag so the input bar / chat
                // hint stops showing thinking. The Up-arrow handler in
                // `handle_key` already restored `last_prompt` into the
                // input buffer.
                let mut slot = current_task.lock().await;
                if let Some(h) = slot.take() {
                    h.abort();
                }
                drop(slot);
                let mut a = app.lock().await;
                a.thinking = false;
                a.thinking_lines.clear();
                a.busy_since = None;
                a.streaming_preview.clear();
                a.push_status("cancelled");
            }
            if let Some((kind, selected)) = picker_choice {
                let cmd = match kind {
                    PickerKind::Model => format!("/model {}", selected),
                    PickerKind::Provider => format!("/provider {}", selected),
                };
                let _ = tx.send(ReplEvent::Submit(cmd));
            }
            // Inline-choice dispatch (e.g. `/switch <persona>` chosen from
            // the flat list shown after `/switch` with no arg). Synthesizes
            // a Submit so the slash handler runs in the existing pipeline.
            if let Some(cmd) = pending_submit {
                let _ = tx.send(ReplEvent::Submit(cmd));
            }
            if let Some(line) = submit {
                // Echo user line + remember + clear thinking immediately.
                // Also stash the line into `last_prompt` so Up-arrow can
                // recall it.
                {
                    let mut a = app.lock().await;
                    a.push_user(&line);
                    a.remember_input(&line);
                    a.last_prompt = line.clone();
                    a.thinking = true;
                    a.thinking_lines.clear();
                    // Activity panel: mark busy timestamp + clear any leftover
                    // preview from the prior turn.
                    a.busy_since = Some(std::time::Instant::now());
                    a.streaming_preview.clear();
                }
                // Dispatch handler in a background task so the render
                // loop keeps responding to scroll/resize while the LLM
                // call is in flight. Store the JoinHandle in
                // `current_task` so Up-arrow can abort it.
                let h = handler.clone();
                let dtx = tx.clone();
                let app_for_quit = app.clone();
                let _task_slot = current_task.clone();
                let handle = tokio::spawn(async move {
                    let res = h.handle_input(line, dtx.clone()).await;
                    match res {
                        Ok(true) => {}
                        Ok(false) => {
                            let mut a = app_for_quit.lock().await;
                            a.quit = true;
                        }
                        Err(e) => {
                            let _ = dtx.send(ReplEvent::LlmResponse {
                                text: format!("error: {e:#}"),
                                is_error: true,
                            });
                        }
                    }
                    let _ = dtx.send(ReplEvent::LlmThinking(false));
                });
                let mut slot = current_task.lock().await;
                // Drop any orphaned previous handle (shouldn't happen in
                // practice — busy gating prevents concurrent submits — but
                // be defensive).
                if let Some(prev) = slot.take() {
                    prev.abort();
                }
                *slot = Some(handle);
            }
        }
        ReplEvent::Resize(_, _) => {
            // Repaint will pick up new dims naturally.
        }
        ReplEvent::LlmResponse { text, is_error } => {
            {
                let mut a = app.lock().await;
                a.push_assistant(text, is_error);
                a.thinking = false;
                a.thinking_lines.clear();
                a.busy_since = None;
                a.streaming_preview.clear();
            }
            // Task done — drop the JoinHandle so a stale slot can't shadow
            // a future cancel target.
            let mut slot = current_task.lock().await;
            *slot = None;
        }
        ReplEvent::LlmThinking(b) => {
            {
                let mut a = app.lock().await;
                a.thinking = b;
                if !b {
                    a.thinking_lines.clear();
                    a.busy_since = None;
                    a.streaming_preview.clear();
                }
            }
            if !b {
                let mut slot = current_task.lock().await;
                *slot = None;
            }
        }
        ReplEvent::ThinkingStep(s) => {
            let mut a = app.lock().await;
            // #298: Real-time token feel during streaming. We don't yet have
            // per-token usage events on the bus (only LlmRequested at start
            // and LlmResponded at end), so each ThinkingStep nudges
            // `tokens_out` by a small estimate. When the real completion
            // count lands via TokenUpdate it overrides cleanly because the
            // event bus delivers it via accumulation either way. If a step
            // text contains an explicit `↓ N tokens` (or `↓N tokens`)
            // pattern, we use that exact value instead of the estimate.
            if let Some(parsed) = parse_token_count_from_step(&s) {
                // Replace, not increment — explicit counts are absolute.
                if parsed > a.tokens_out {
                    a.tokens_out = parsed;
                }
            } else {
                a.tokens_out = a.tokens_out.saturating_add(8);
            }
            // Dedup consecutive identical lines so a chatty event bus
            // doesn't flood the chat area with repeats.
            if a.thinking_lines.last().map(|x| x.as_str()) != Some(s.as_str()) {
                // Mirror the latest step into the preview area as a
                // best-effort "in-progress response" surface — until proper
                // token streaming is wired through, this is the closest
                // signal we have.
                a.streaming_preview = s.clone();
                a.thinking_lines.push(s);
            }
        }
        ReplEvent::StatusMessage(s) => {
            let mut a = app.lock().await;
            a.push_status(s);
        }
        ReplEvent::LabelChanged(s) => {
            let mut a = app.lock().await;
            a.project_name = s;
        }
        ReplEvent::AgentScopeChanged(scope) => {
            let mut a = app.lock().await;
            a.agent_scope = scope;
        }
        ReplEvent::Submit(line) => {
            // Synthetic submission (currently used by the picker overlay
            // when the user presses Enter on a /model or /provider choice).
            // Mirrors the Key(Enter) -> handler dispatch path so the slash
            // command flows through `try_handle_slash` exactly as a typed
            // command would.
            let h = handler.clone();
            let dtx = tx.clone();
            let app_for_quit = app.clone();
            let task_slot = current_task.clone();
            let handle = tokio::spawn(async move {
                let res = h.handle_input(line, dtx.clone()).await;
                match res {
                    Ok(true) => {}
                    Ok(false) => {
                        let mut a = app_for_quit.lock().await;
                        a.quit = true;
                    }
                    Err(e) => {
                        let _ = dtx.send(ReplEvent::LlmResponse {
                            text: format!("error: {e:#}"),
                            is_error: true,
                        });
                    }
                }
                let _ = dtx.send(ReplEvent::LlmThinking(false));
            });
            let mut slot = task_slot.lock().await;
            if let Some(prev) = slot.take() {
                prev.abort();
            }
            *slot = Some(handle);
        }
        ReplEvent::OpenPicker { items, title, kind } => {
            let mut a = app.lock().await;
            a.picker = Some(PickerState {
                items,
                selected: 0,
                title,
                kind,
            });
        }
        ReplEvent::OllamaModelsLoaded(models) => {
            let mut a = app.lock().await;
            a.ollama_models = models;
        }
        ReplEvent::SetChoices { items, context } => {
            let mut a = app.lock().await;
            a.choices = items;
            a.choice_cursor = 0;
            a.choices_context = context;
        }
        ReplEvent::TmSessionCount(n) => {
            let mut a = app.lock().await;
            a.tm_session_count = n;
        }
        ReplEvent::ClaudeMpmSessionCount(n) => {
            let mut a = app.lock().await;
            a.claude_mpm_session_count = n;
        }
        ReplEvent::Scroll(delta) => {
            let mut a = app.lock().await;
            a.scroll(delta);
        }
        ReplEvent::TokenUpdate { prompt, completion } => {
            let mut a = app.lock().await;
            a.tokens_in = a.tokens_in.saturating_add(prompt);
            a.tokens_out = a.tokens_out.saturating_add(completion);
            persist_daily_usage_if_due(&mut a);
        }
        ReplEvent::TokenReset => {
            let mut a = app.lock().await;
            a.tokens_in = 0;
            a.tokens_out = 0;
        }
        ReplEvent::StatuslineUpdate { model, provider } => {
            let mut a = app.lock().await;
            // Regenerate `status_line` so the rich statusline picks up the
            // new model/provider (the rich renderer reads `status_line`, not
            // `model_name`/`provider_name`). Format mirrors the startup
            // string built in `repl/mod.rs::OpenMpmRepl::run` (#296):
            //   "✓ LLM: provider:model · All systems go."
            a.status_line = Some(format!(
                "✓ LLM: {}:{} · All systems go.",
                provider,
                strip_vendor_prefix(&model)
            ));
            a.model_name = model;
            a.provider_name = provider;
        }
    }
}

/// Handle one key press. Returns `Some(line)` if the user submitted.
fn handle_key(app: &mut ReplApp, key: KeyEvent) -> Option<String> {
    // Picker modal: when an overlay is open, capture all keys here so
    // arrow / Enter / Esc don't leak through to the input editor.
    if app.picker.is_some() {
        return handle_picker_key(app, key);
    }
    // Inline choice picker: when the LLM offered a list and we surfaced it,
    // arrow keys navigate the choices, Enter commits the selection into the
    // input buffer (or clears for free-type on the "Other…" row), Esc
    // dismisses. Other keys fall through so the user can keep typing.
    if !app.choices.is_empty() {
        match key.code {
            KeyCode::Up => {
                app.choice_cursor = app.choice_cursor.saturating_sub(1);
                return None;
            }
            KeyCode::Down => {
                if app.choice_cursor + 1 < app.choices.len() {
                    app.choice_cursor += 1;
                }
                return None;
            }
            KeyCode::Enter => {
                let idx = app.choice_cursor;
                let last = app.choices.len().saturating_sub(1);
                let is_other = idx == last
                    && app
                        .choices
                        .get(idx)
                        .map(|s| s.starts_with("Other"))
                        .unwrap_or(false);
                if is_other {
                    // Free-type path: leave input empty for the user.
                    app.choices.clear();
                    app.choice_cursor = 0;
                    app.choices_context = None;
                    return None;
                }
                let pick = app.choices[idx].clone();
                let ctx = app.choices_context.take();
                app.choices.clear();
                app.choice_cursor = 0;
                match ctx.as_deref() {
                    Some("switch") => {
                        // Direct dispatch — synthesize `/switch <name>`
                        // so the user doesn't need a second Enter.
                        app.pending_submit = Some(format!("/switch {}", pick));
                    }
                    _ => {
                        // Default: insert selection into input buffer for
                        // the user to edit/submit themselves.
                        app.set_input(pick);
                    }
                }
                return None;
            }
            KeyCode::Esc => {
                app.choices.clear();
                app.choice_cursor = 0;
                app.choices_context = None;
                return None;
            }
            _ => { /* fall through to normal input editing */ }
        }
    }
    // Ctrl combos.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                // Cancel current input but stay in REPL.
                app.input_buf.clear();
                app.cursor_pos = 0;
                return None;
            }
            KeyCode::Char('d') => {
                if app.input_buf.is_empty() {
                    app.quit = true;
                }
                return None;
            }
            KeyCode::Char('a') => {
                app.cursor_pos = 0;
                return None;
            }
            KeyCode::Char('e') => {
                // #321: When the input is empty, paste the most recent
                // bash/sh fenced block from chat into the buffer so the
                // user can edit-and-run a suggested shell command without
                // mouse selection. Falls through to "cursor to end" when
                // input is non-empty (preserves the readline End-of-line
                // muscle memory).
                if app.input_buf.is_empty()
                    && let Some(block) = &app.last_bash_block
                {
                    // #323: Only paste the first non-empty line — the
                    // REPL input is single-line. Multi-line blocks are
                    // common (sequential commands like `git add -A` then
                    // `git commit -m "msg"`); pasting the whole block
                    // would silently truncate at the first `\n` on submit.
                    let first_line = block
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .to_string();
                    if !first_line.is_empty() {
                        app.input_buf = first_line;
                        app.cursor_pos = app.input_buf.len();
                        return None;
                    }
                }
                app.cursor_pos = app.input_buf.len();
                return None;
            }
            KeyCode::Char('u') => {
                app.input_buf.clear();
                app.cursor_pos = 0;
                return None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => app.take_input(),
        KeyCode::Tab => {
            // Slash-command autocomplete: when the inline picker is showing
            // matches, Tab completes the highlighted command into the input
            // buffer with a trailing space (so the user can immediately type
            // arguments). Falls through to no-op when no choices are active.
            if !app.choices.is_empty()
                && app.choices_context.is_none()
                && app.input_buf.starts_with('/')
            {
                let selected = app.choices[app.choice_cursor].clone();
                app.input_buf = format!("{selected} ");
                app.cursor_pos = app.input_buf.len();
                app.choices.clear();
                app.choice_cursor = 0;
            }
            None
        }
        KeyCode::Char(c) => {
            app.insert_char(c);
            update_slash_completions(app);
            None
        }
        KeyCode::Backspace => {
            app.backspace();
            update_slash_completions(app);
            None
        }
        KeyCode::Left => {
            app.cursor_left();
            None
        }
        KeyCode::Right => {
            app.cursor_right();
            None
        }
        KeyCode::Home => {
            app.cursor_pos = 0;
            None
        }
        KeyCode::End => {
            app.cursor_pos = app.input_buf.len();
            None
        }
        KeyCode::Up => {
            // New semantics (#XXX): Up-arrow recalls the last submitted
            // prompt into the input buffer. While the LLM is busy, it ALSO
            // signals the event loop to cancel the in-flight task via
            // `pending_cancel` — the user can then edit and resubmit.
            if app.thinking {
                app.pending_cancel = true;
            }
            if !app.last_prompt.is_empty() {
                let lp = app.last_prompt.clone();
                app.set_input(lp);
            }
            None
        }
        KeyCode::Down => {
            app.history_next();
            None
        }
        KeyCode::PageUp => {
            app.scroll(-10);
            None
        }
        KeyCode::PageDown => {
            app.scroll(10);
            None
        }
        _ => None,
    }
}

/// Handle a key while a picker overlay is open.
///
/// Why: Centralizes the modal-state key routing so `handle_key`'s normal path
/// can stay focused on the input editor. Up/Down navigate (with wrap-around),
/// Enter confirms (stashing the choice in `pending_picker_selection` for the
/// event loop to translate into a Submit), Esc cancels.
/// What: Mutates `app.picker` directly. Returns `None` always — picker keys
/// never produce a submitted line directly; the event loop synthesizes a
/// `Submit("/model …")` after observing `pending_picker_selection`.
/// Test: `repl_app_picker_navigation_wraps`, `repl_app_picker_enter_sets_pending_selection`,
/// `repl_app_picker_esc_dismisses`.
fn handle_picker_key(app: &mut ReplApp, key: KeyEvent) -> Option<String> {
    let picker = app.picker.as_mut().expect("picker present");
    match key.code {
        KeyCode::Up => {
            if picker.items.is_empty() {
                return None;
            }
            if picker.selected == 0 {
                picker.selected = picker.items.len() - 1;
            } else {
                picker.selected -= 1;
            }
        }
        KeyCode::Down => {
            if picker.items.is_empty() {
                return None;
            }
            picker.selected = (picker.selected + 1) % picker.items.len();
        }
        KeyCode::Enter if !picker.items.is_empty() => {
            let selected = picker.items[picker.selected].clone();
            let kind = picker.kind.clone();
            app.picker = None;
            app.pending_picker_selection = Some((kind, selected));
        }
        KeyCode::Esc => {
            app.picker = None;
        }
        _ => {}
    }
    None
}

/// Top-level draw: banner (optional) + chat (fills) + activity (3 rows, busy)
/// + streaming preview (2 rows, busy) + input separator + input + bottom
/// separator + statusline.
///
/// Why: When the LLM is working we want a dedicated, persistent activity
/// strip near the input — spinner + elapsed time + active model + the latest
/// thinking step — and a queued-response preview right above the prompt so
/// the user sees the assistant taking shape without scrolling. When idle
/// those rows collapse to zero so the chat fills the screen.
/// What: Layout constraints are computed dynamically from `app.busy()`
/// (true ⇔ thinking || busy_since.is_some()). The top separator that used
/// to sit between chat and input is removed when busy — the activity area
/// itself provides visual separation. The bottom separator (above the
/// statusline) stays put.
/// Test: Visual via `scripts/tmux-repl-test.sh`; geometry exercised via
/// the `draw_*` helpers + `repl_app_busy_*` unit tests on state.
pub fn draw(f: &mut ratatui::Frame, app: &ReplApp) {
    let busy = app.thinking || app.busy_since.is_some();

    // Inline choice picker height — computed early so chat sizing math (below)
    // can subtract it from available height. Same value used when pushing
    // constraint further down.
    let picker_height: u16 = if !app.choices.is_empty() {
        app.choices.len().min(8) as u16
    } else {
        0
    };

    // Build the constraint vector with semantic markers so destructuring is
    // explicit. The order is always:
    //   chat [activity preview] sep_above_input input sep_below_input statusline
    //
    // Note: the banner is no longer a separate layout chunk — it is prepended
    // to the chat line buffer in `draw_chat()` when `app.show_banner` is true,
    // so it scrolls naturally with chat history.
    let mut constraints: Vec<Constraint> = Vec::with_capacity(8);
    // Chat constraint: when both the banner and chat are absent, collapse to
    // Length(0) so the empty pane doesn't expand to fill the terminal. Once
    // either banner or chat content is present, size to actual content so the
    // input row floats up directly beneath the last message (issue #337). The
    // trailing Min(1) at the bottom of the layout absorbs remaining space.
    if app.chat.is_empty() && !app.show_banner {
        constraints.push(Constraint::Length(0)); // chat (empty — no forced expansion)
    } else if app.chat.is_empty() && app.show_banner {
        // Startup splash: size the chat pane to exactly the banner's height so
        // the bottom border sits flush against the input separator, instead of
        // letting an expanding constraint inflate the pane and leave a sea of
        // blank rows below the banner content. The trailing Min(1) spacer
        // below the statusline absorbs any leftover vertical space.
        let banner_h = banner_lines(app, f.area().width as usize).len() as u16;
        constraints.push(Constraint::Length(banner_h));
    } else {
        // Issue #337: size chat pane to actual content height (capped at the
        // available space). Previously used `Constraint::Min(3)` which expands
        // to fill the screen, pinning the input box to the bottom even with a
        // 5-line conversation. Length(content_h) lets the trailing `Min(1)`
        // spacer absorb leftover rows so input/statusline sit flush below the
        // content. When content >= available_h, behavior matches the old
        // Min(3) path because chat consumes all available rows.
        let content_h = chat_line_count(app, f.area().width as usize).max(1);
        // Reserved rows below chat: top_sep(1) + input(1) + bot_sep(1)
        // + activity(3 if busy) + picker_height + statusline(1)
        // + bottom spacer minimum(1).
        let reserved: u16 = 1 // top_sep
            + 1 // input
            + 1 // bot_sep
            + if busy { 3 } else { 0 } // activity
            + picker_height // inline picker
            + 1 // statusline
            + 1; // bottom spacer minimum
        let available_h = f.area().height.saturating_sub(reserved) as usize;
        let chat_h = content_h.min(available_h).max(1);
        constraints.push(Constraint::Length(chat_h as u16));
    }
    if busy {
        constraints.push(Constraint::Length(3)); // activity (busy only)
    }
    // Single separator above input. The dedicated queued-hint row was folded
    // into the empty input row as a dim italic placeholder (Claude Code style).
    constraints.push(Constraint::Length(1)); // separator above input
    constraints.push(Constraint::Length(1)); // input
    constraints.push(Constraint::Length(1)); // bottom separator
    // Inline choice picker (when active) sits BETWEEN the bottom input
    // separator and the statusline so it visually anchors below the input
    // box rather than crowding the input row itself. Height = min(N, 8) —
    // borderless flat list, capped so a long list doesn't crowd out the
    // chat area. (`picker_height` computed above for chat sizing.)
    if picker_height > 0 {
        constraints.push(Constraint::Length(picker_height));
    }
    constraints.push(Constraint::Length(1)); // statusline
    // Bug 2: a single blank terminal row below the statusline at all times
    // gives the bottom bar breathing room from the terminal's bottom edge.
    // Using Min(1) here (rather than Length(1)) doubles as the leftover-
    // absorber when chat is collapsed to Length(0) on startup, so the input
    // doesn't get pushed to the bottom by ratatui's space distribution.
    constraints.push(Constraint::Min(1)); // blank spacer below statusline

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(f.area());

    // Walk the chunks in the same order they were pushed.
    let mut idx = 0usize;
    let chat_area = chunks[idx];
    idx += 1;
    let activity_area = if busy {
        let a = chunks[idx];
        idx += 1;
        Some(a)
    } else {
        None
    };
    let top_sep_area = chunks[idx];
    idx += 1;
    let input_area = chunks[idx];
    idx += 1;
    let bot_sep_area = chunks[idx];
    idx += 1;
    let inline_picker_area = if picker_height > 0 {
        let a = chunks[idx];
        idx += 1;
        Some(a)
    } else {
        None
    };
    let status_area = chunks[idx];
    idx += 1;
    // Bug 2: trailing blank-row spacer below the statusline. Rendering an
    // explicit empty Paragraph here is defensive — the row is reserved by the
    // layout regardless, but rendering ensures alt-screen state is clean even
    // if some prior frame left artifacts.
    let bottom_spacer_area = chunks[idx];

    draw_chat(f, app, chat_area);
    if let Some(a) = activity_area {
        draw_activity(f, app, a);
    }
    draw_separator(f, top_sep_area);
    draw_input(f, app, input_area);
    draw_separator(f, bot_sep_area);
    if let Some(a) = inline_picker_area {
        draw_inline_choice_picker(f, app, a);
    }
    draw_statusline(f, app, status_area);
    // Bug 2: render the trailing blank row (no border, no text).
    f.render_widget(Paragraph::new(""), bottom_spacer_area);
    // Picker overlay renders LAST so it sits on top of every other widget.
    draw_picker(f, app);
}

/// Render the activity panel (spinner + elapsed time + model + latest step).
///
/// Why: Persistent feedback that the LLM call is in flight, distinct from
/// the inline `[thinking...]` label on the input row. Three rows so we can
/// surface the model id and the most recent thinking step alongside the
/// spinner without crowding the input.
/// What: Row 1 = spinner glyph + "processing..." (dim) + right-aligned
/// elapsed `Xs`. Row 2 = `↳ model: <model_name>` (dim). Row 3 = latest
/// thinking step (dim italic), truncated to width.
/// Test: Geometry via `scripts/tmux-repl-test.sh`; spinner cycle is
/// time-based and deterministic given the same `Instant`.
fn draw_activity(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let elapsed_secs = app.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);

    let row1 = build_activity_row1(app, elapsed_secs, area.width as usize);

    // Row 2: model id (kept). Strip the vendor prefix (`anthropic/`, `openai/`,
    // …) so users see the bare model name. The full id is still present in
    // logs and the statusline-source string for debugging.
    let model_text = format!("  ↳ {}", strip_vendor_prefix(&app.model_name));
    let row2 = Line::from(Span::styled(
        truncate_to(model_text, area.width as usize),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::ITALIC),
    ));

    // Row 3: latest thinking step or blank. Dedup against row 1's cycling
    // status word ("thinking" / "working" / "processing") — when the only
    // thinking line is identical (case-insensitive, modulo trailing
    // ellipses/whitespace) to the spinner word, leave row 3 blank rather
    // than echoing it twice.
    let raw_step = app.thinking_lines.last().cloned().unwrap_or_default();
    let step_text = if is_redundant_thinking_step(&raw_step) {
        String::new()
    } else {
        raw_step
    };
    let row3 = Line::from(Span::styled(
        truncate_to(format!("  {}", step_text), area.width as usize),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::ITALIC),
    ));

    let p = Paragraph::new(vec![row1, row2, row3]);
    f.render_widget(p, area);
}

/// Strip the leading `vendor/` prefix from a model id for compact display.
///
/// Why: The TUI activity row and statusline previously rendered the full id
/// (`anthropic/claude-haiku-4-5`) which wastes horizontal space and reads as
/// noise — users already know the provider from the statusline label.
/// What: Returns everything after the first `/`. If the input has no `/`,
/// returns it unchanged. Pure, allocation-light (`String` only when slicing).
/// Test: `strip_vendor_prefix_*` unit tests below.
fn strip_vendor_prefix(model: &str) -> String {
    match model.find('/') {
        Some(i) => model[i + 1..].to_string(),
        None => model.to_string(),
    }
}

/// True when a thinking-step line is redundant with row 1's cycling status word.
///
/// Why: `draw_activity` would otherwise show "thinking…" on both the spinner
/// row (row 1) and the latest-thinking row (row 3) when the LLM hasn't yet
/// emitted any meaningful step text. Suppressing the dup keeps row 3 free
/// for real progress signals when they arrive.
/// What: Lower-cases the step, trims whitespace and trailing `.` / `…`
/// characters, then matches against the three status words emitted by
/// `status_word_for`. Empty string is also considered redundant.
/// Test: `is_redundant_thinking_step_*` unit tests below.
fn is_redundant_thinking_step(step: &str) -> bool {
    let trimmed: String = step
        .trim()
        .trim_end_matches(['.', '…'])
        .trim()
        .to_ascii_lowercase();
    matches!(trimmed.as_str(), "" | "thinking" | "working" | "processing")
}

/// Format an elapsed-seconds value as `Xs`, `Xm Ys`, or `Xh Ym`.
///
/// Why: Claude Code's spinner shows `(2m 18s · ...)` style elapsed; we mirror
/// it so users don't decode large `1234s` figures.
/// What: <60s → `Ns`; <3600s → `Mm Ss`; otherwise → `Hh Mm`.
/// Test: `format_elapsed_buckets`.
fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Format a token count compactly: `<1000` raw, `>=1000` as `1.2k`.
///
/// Why: Claude Code spinner uses `↓ 2.9k tokens` — keep it short.
/// What: Round to one decimal at the `k` boundary; trim trailing `.0` to
/// keep `2k` instead of `2.0k`.
/// Test: `format_tokens_compact`.
fn format_tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = (n as f64) / 1000.0;
    let s = format!("{:.1}k", k);
    // Trim `.0k` → `k` for a tighter render.
    if let Some(stripped) = s.strip_suffix(".0k") {
        format!("{}k", stripped)
    } else {
        s
    }
}

/// Pick the cycling status word from elapsed seconds.
///
/// Why: Claude Code rotates "thinking / working / processing"; we bucket on
/// elapsed time so the word changes deterministically as the call grows.
/// What: 0–9s = thinking, 10–29s = working, 30s+ = processing.
/// Test: `status_word_buckets`.
fn status_word_for(elapsed_secs: u64) -> &'static str {
    match elapsed_secs {
        0..=9 => "thinking",
        10..=29 => "working",
        _ => "processing",
    }
}

/// Build the spinner row 1 of the activity panel.
///
/// Why: Pulled out so we can unit-test the composition (glyph, elapsed,
/// token segment, status word) without a Terminal.
/// What: `✻ Processing… (Xs · ↓ Yk tokens · word)` — `✻` in yellow, rest dim.
/// Token segment is omitted when both `tokens_in` and `tokens_out` are zero.
/// Test: `activity_row1_includes_elapsed_and_status`,
/// `activity_row1_omits_tokens_when_zero`.
fn build_activity_row1(app: &ReplApp, elapsed_secs: u64, max_width: usize) -> Line<'static> {
    let elapsed = format_elapsed(elapsed_secs);
    let word = status_word_for(elapsed_secs);

    let mut paren_parts: Vec<String> = Vec::with_capacity(3);
    paren_parts.push(elapsed);
    if app.tokens_in > 0 || app.tokens_out > 0 {
        // While streaming we usually only have completion tokens (the prompt
        // is not yet billed). Show `↓ N tokens` if no prompt tokens yet,
        // otherwise the full `↑ N ↓ N tokens` pair.
        let tok_seg = if app.tokens_in == 0 {
            format!("↓ {} tokens", format_tokens(app.tokens_out))
        } else {
            format!(
                "↑ {} ↓ {} tokens",
                format_tokens(app.tokens_in),
                format_tokens(app.tokens_out)
            )
        };
        paren_parts.push(tok_seg);
    }
    paren_parts.push(word.to_string());

    let body = format!("Processing… ({})", paren_parts.join(" · "));
    // Animated braille spinner cycled by `app.tick_count` (~10fps from the
    // event-loop tick). Frames mirror Claude Code's spinner so the activity
    // strip reads as "actively working" instead of a frozen glyph.
    let glyph_char = SPINNER_FRAMES[(app.tick_count as usize) % SPINNER_FRAMES.len()];
    let glyph = format!("{} ", glyph_char);
    let combined = format!("{}{}", glyph, body);
    let truncated = truncate_to(combined, max_width);
    // Apply the Rust-rainbow flow effect across every character of the
    // activity row while busy. The activity panel only renders when busy
    // (see `draw` layout in this module), so this code path implies busy.
    Line::from(rainbow_spans(&truncated, app.rainbow_tick))
}

/// Spinner frames cycled by `tick_count`. Braille pattern matches Claude Code's
/// spinner — visually distinct from the static `✻` and reads smoothly at 10fps.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// All slash commands surfaced by `/help`, used for inline autocomplete.
///
/// Why: As the user types `/` the inline picker filters this list by prefix,
/// letting them arrow-pick or Tab-complete a command instead of remembering
/// the exact name. Centralizing the list here keeps autocomplete in lockstep
/// with `write_help()` in `src/repl/mod.rs` — when a new command is added,
/// both must be updated.
/// What: `(name, short_description)` pairs for all 22 user-facing slash
/// commands. Names include the leading `/`. Description is shown alongside
/// in the future; current picker only renders the name.
/// Test: `slash_completions_filters_by_prefix`,
/// `slash_completions_clears_on_space`, `slash_completions_tab_completes`.
pub(crate) const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "show help"),
    ("/clear", "clear chat"),
    ("/exit", "quit"),
    ("/status", "system status"),
    ("/model", "select LLM model"),
    ("/provider", "select LLM provider"),
    ("/agent", "run a specific agent"),
    ("/switch", "switch persona"),
    ("/agents", "list agents"),
    ("/skills", "list skills"),
    ("/memories", "show memories"),
    ("/session", "session info"),
    ("/connect", "connect to project"),
    ("/version", "show version"),
    ("/projects", "list projects"),
    ("/log", "toggle log"),
    ("/run", "run workflow"),
    ("/history", "show history"),
    ("/telegram", "telegram bot control"),
    ("/logs", "show recent logs"),
    ("/local", "local inference control"),
    ("/tm", "tmux session manager"),
    ("/service", "manage persistent daemon"),
    ("/update", "check for and install updates"),
];

/// Recompute `app.choices` from the current input buffer to drive the inline
/// slash-command autocomplete picker.
///
/// Why: As the user types `/` the inline picker should narrow to commands that
/// match the typed prefix, then disappear once a space is typed (the command
/// name is locked in and the user is now typing arguments). Reusing the
/// existing `app.choices` plumbing means we get rendering, arrow navigation,
/// and Enter-to-insert for free — no new picker widget needed.
/// What: When `input_buf` starts with `/` and contains no space, populate
/// `choices` with matching command names (descriptions discarded for now to
/// keep the inserted text clean). Otherwise clear `choices`. Suppresses the
/// picker on an exact single-match (e.g. user typed `/help` fully) so the
/// picker doesn't visually echo what they already typed.
/// Test: `slash_completions_filters_by_prefix`,
/// `slash_completions_clears_on_space`,
/// `slash_completions_suppressed_on_exact_match`.
fn update_slash_completions(app: &mut ReplApp) {
    // Don't clobber an active context-driven picker (e.g. `/switch` persona
    // list). Those have a non-`None` context tag and a different lifecycle.
    if app.choices_context.is_some() {
        return;
    }
    let buf = &app.input_buf;
    if buf.starts_with('/') && !buf.contains(' ') {
        let prefix = buf.to_lowercase();
        let matches: Vec<String> = SLASH_COMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(prefix.as_str()))
            .map(|(cmd, _)| (*cmd).to_string())
            .collect();
        // Suppress when there's exactly one match and it equals what the
        // user already typed — no value showing the picker.
        let is_exact_single = matches.len() == 1 && matches[0] == *buf;
        if !matches.is_empty() && !is_exact_single {
            app.choices = matches;
            app.choice_cursor = 0;
            app.choices_context = None;
        } else {
            app.choices.clear();
            app.choice_cursor = 0;
        }
    } else {
        // Only clear if the choices we're holding are slash-command picks
        // (i.e. all start with '/'). Don't stomp on LLM-offered choice lists.
        let all_slash = !app.choices.is_empty() && app.choices.iter().all(|c| c.starts_with('/'));
        if all_slash {
            app.choices.clear();
            app.choice_cursor = 0;
        }
    }
}

/// Convert HSL (h: 0.0–360.0, s: 0.0–1.0, l: 0.0–1.0) to RGB (0–255 each).
///
/// Why: The flowing shimmer effect uses a continuous hue band rather than a
/// discrete palette, which requires HSL → RGB conversion at render time.
/// What: Standard HSL→RGB algorithm; returns the linear RGB triple.
/// Test: `hsl_to_rgb_edge_cases` covers white, black, and pure red.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h1 = h / 60.0;
    let x = c * (1.0 - (h1 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = if h1 < 1.0 {
        (c, x, 0.0)
    } else if h1 < 2.0 {
        (x, c, 0.0)
    } else if h1 < 3.0 {
        (0.0, c, x)
    } else if h1 < 4.0 {
        (0.0, x, c)
    } else if h1 < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

/// Animated horizontal HSL gradient across `text`, slowly drifting with `tick`.
///
/// Why: A discrete palette jumps between colors; the target effect (Claude
/// Code's spinner shimmer) is a smooth horizontal hue gradient that slowly
/// rotates over time. Equivalent to a CSS `linear-gradient` with `hue-rotate`
/// animation. Stays in the warm rust → orange → amber band.
/// What: Spreads chars across a 35° hue band starting at 5° (deep rust-red),
/// shifted by `tick * 0.8°` for the temporal drift. At ~10 ticks/sec this is
/// 8°/sec — a full 360° cycle every ~45s, matching the slow Claude Code drift.
/// One `Span` per char so each glyph carries its own color.
/// Test: `rainbow_spans_advances_with_tick`,
/// `rainbow_spans_one_span_per_char`, `hsl_to_rgb_edge_cases`.
fn rainbow_spans(text: &str, tick: usize) -> Vec<Span<'static>> {
    let len = text.chars().count().max(1);
    // Hue band: 5° (deep rust-red) → 40° (amber). Width = 35°.
    const BASE_HUE: f32 = 5.0;
    const BAND: f32 = 35.0;
    // Each tick shifts the gradient by 0.8° (full cycle ~450 ticks ≈ 45s at
    // 100ms/tick). The visible band sweeps through in ~4s.
    const DRIFT_PER_TICK: f32 = 0.8;
    let time_offset = (tick as f32 * DRIFT_PER_TICK) % 360.0;

    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let pos = i as f32 / len as f32;
            let hue = (BASE_HUE + pos * BAND + time_offset) % 360.0;
            let (r, g, b) = hsl_to_rgb(hue, 0.90, 0.55);
            Span::styled(c.to_string(), Style::default().fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

/// Parse an explicit `↓ N tokens` / `↓N tokens` count from a thinking-step
/// string (#298).
///
/// Why: Some upstream emitters surface partial completion-token counts in the
/// step text (e.g. `↓ 2.4k tokens`). When present, that's a more accurate
/// signal than the per-step estimate, so the TUI should snap to it.
/// What: Looks for a `↓` glyph followed by an optional space, a number with
/// optional `k`/`K` suffix (1k = 1000), and the literal `tokens` token. Returns
/// `None` if no match. Pure / allocation-light.
/// Test: `parse_token_count_from_step_*` unit tests below.
fn parse_token_count_from_step(s: &str) -> Option<u64> {
    let idx = s.find('↓')?;
    let rest = &s[idx + '↓'.len_utf8()..];
    let rest = rest.trim_start();
    // Read digits, optional `.` digits, optional k/K.
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let num_str = &rest[..i];
    let num: f64 = num_str.parse().ok()?;
    let after = &rest[i..];
    let (multiplier, after) = if let Some(stripped) = after.strip_prefix(['k', 'K']) {
        (1000.0, stripped)
    } else {
        (1.0, after)
    };
    // Require the word "tokens" follows (with optional whitespace) so we
    // don't false-match `↓ 5 lines` or similar.
    if !after.trim_start().starts_with("token") {
        return None;
    }
    Some((num * multiplier).round() as u64)
}

/// Truncate a `String` to `max_chars` characters (chars, not bytes), appending
/// nothing — this is a pure cap.
fn truncate_to(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    s.chars().take(max_chars).collect()
}

/// Strip leading and trailing whitespace-only lines from a multi-line string.
///
/// Why: LLM responses regularly include extra `\n\n` at the head or tail
/// (especially when the model emits a paragraph break before/after a closing
/// emoji). Rendering them verbatim produces visible blank rows in the chat
/// scrollback. Trimming once at the boundary keeps interior blank lines
/// (which carry meaning) intact.
/// What: Returns a String with all leading whitespace-only lines and all
/// trailing whitespace removed. Interior lines, including blank paragraph
/// breaks, are preserved.
/// Test: `trim_surrounding_blank_lines_*` unit tests.
/// Drop ALL whitespace-only lines from within a response.
///
/// Why: LLM responses arrive with markdown-style double-newline paragraph
/// breaks. In a terminal chat panel those blank rows accumulate into wasted
/// vertical space — the user reads consecutive paragraphs as a single
/// flowing thought, so the gaps just push later content off-screen. Removing
/// every interior blank produces a tight, compact response block.
/// What: Returns a String where every whitespace-only line is dropped.
/// Non-blank lines are preserved verbatim and joined with single `\n`s.
/// Test: `strip_interior_blank_lines_*` unit tests.
fn strip_interior_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if !first {
            out.push('\n');
        }
        out.push_str(line);
        first = false;
    }
    out
}

/// Drop runs of 2+ consecutive whitespace-only `Line`s, keeping at most one.
///
/// Why: Same motivation as `collapse_inner_blank_lines` but operates on the
/// already-spanned `Vec<Line>` produced by `draw_chat` so paragraph gaps the
/// renderer itself inserted (e.g. the trailing blank after every assistant
/// response) don't compound when adjacent.
/// What: Returns a new `Vec<Line>` with consecutive whitespace-only lines
/// collapsed to a single blank `Line`.
/// Test: `collapse_blank_lines_*` unit tests.
fn collapse_blank_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut prev_blank = false;
    for line in lines {
        let is_blank = line.spans.iter().all(|s| s.content.trim().is_empty());
        if is_blank && prev_blank {
            continue;
        }
        prev_blank = is_blank;
        out.push(line);
    }
    out
}

fn trim_surrounding_blank_lines(s: &str) -> String {
    // First trim trailing whitespace (covers tabs, spaces, and newlines).
    let trimmed_end = s.trim_end();
    // Then drop leading whitespace-only lines.
    let mut start = 0usize;
    let bytes = trimmed_end.as_bytes();
    while start < bytes.len() {
        // Find next newline.
        let nl = bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p);
        let line_end = nl.unwrap_or(bytes.len());
        let line = &trimmed_end[start..line_end];
        if line.trim().is_empty() {
            // Skip the blank line including the newline.
            start = match nl {
                Some(p) => p + 1,
                None => bytes.len(),
            };
        } else {
            break;
        }
    }
    trimmed_end[start..].to_string()
}

/// Render a dim horizontal rule across the given area.
///
/// Why: Acts as the visual "bar above the input" that #290 inadvertently
/// removed when it stripped the input's bordered block. Keeps the modern
/// borderless look while restoring the demarcation users rely on to find
/// the prompt at a glance.
/// What: A single `─` repeated across the row, dim-styled. Renders nothing
/// if `area.height == 0`.
/// Test: Visual via `scripts/tmux-repl-test.sh`; geometry is mechanical.
fn draw_separator(f: &mut ratatui::Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let rule: String = "─".repeat(area.width as usize);
    let p = Paragraph::new(rule).style(Style::default().add_modifier(Modifier::DIM));
    f.render_widget(p, area);
}

/// Build a centered popup `Rect` covering `percent_x` × `percent_y` of `r`.
///
/// Why: ratatui has no built-in helper; this is the canonical idiom from the
/// official examples. Used by `draw_picker` to position the overlay.
/// What: Vertical split → take middle band → horizontal split → take middle column.
/// Test: Geometry is exercised visually; correctness is mechanical.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Detect a multiple-choice list in an LLM response and extract the items.
///
/// Why: When the model asks the user to pick from a list (numbered "1." / "2."
/// or bulleted "- " / "• "), surfacing those choices as an interactive picker
/// below the input row is much faster than retyping. We only trigger when at
/// least 2 list items are present so prose with a single bullet doesn't false-
/// positive into picker mode.
/// What: Returns `Some(items)` if the response contains either:
///  - 2+ lines starting with `N.` (numbered list)
///  - 2+ lines starting with `- ` or `• ` (bulleted list)
/// The body of each list item (after the marker) is captured, trimmed, and
/// returned. Otherwise returns `None`.
/// Test: `detect_choices_*` unit tests below.
pub fn detect_choices(text: &str) -> Option<Vec<String>> {
    let mut numbered: Vec<String> = Vec::new();
    let mut bulleted: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        // Numbered: digits followed by `.` or `)` then space.
        if let Some(rest) = strip_numbered_marker(line) {
            let body = rest.trim();
            if !body.is_empty() {
                numbered.push(body.to_string());
            }
            continue;
        }
        // Bulleted.
        if let Some(rest) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("• "))
            .or_else(|| line.strip_prefix("* "))
        {
            let body = rest.trim();
            if !body.is_empty() {
                bulleted.push(body.to_string());
            }
        }
    }
    if numbered.len() >= 2 {
        return Some(numbered);
    }
    if bulleted.len() >= 2 {
        return Some(bulleted);
    }
    None
}

/// Strip a numbered-list marker (`1.`, `12)`, etc.) from the start of a line.
///
/// Why: Pulled out so `detect_choices` can stay readable and so the marker
/// parsing is unit-testable in isolation.
/// What: If the line starts with one or more digits followed by `.` or `)` and
/// then a space, returns the remainder; otherwise None.
/// Test: covered by `detect_choices_*` tests.
fn strip_numbered_marker(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    if i >= bytes.len() {
        return None;
    }
    let punct = bytes[i];
    if punct != b'.' && punct != b')' {
        return None;
    }
    let after_punct = i + 1;
    if after_punct >= bytes.len() {
        return None;
    }
    let n = line[after_punct..].chars().next()?;
    if !n.is_whitespace() {
        return None;
    }
    Some(&line[after_punct + n.len_utf8()..])
}

/// Render the inline multiple-choice picker (just below the input row).
///
/// Why: A non-modal picker right beneath the input keeps the user's eyes near
/// where they're typing — no popup, no chat scroll disruption. The picker
/// shrinks the chat pane by its own height so nothing visually jumps when it
/// appears or disappears.
/// What: Borderless flat list of choices — no Block, no title. Selected row
/// gets a `▶ ` cyan/dim prefix and bold text; non-selected rows get `  `
/// (two-space) prefix and dim text. When choices exceed the visible area,
/// a sliding window keeps the cursor visible.
/// Test: Visual via tmux REPL; state mutations covered by `inline_choice_*`.
fn draw_inline_choice_picker(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    if app.choices.is_empty() || area.height < 1 {
        return;
    }

    // Compute sliding window so cursor stays visible. Window size is the
    // smaller of the rendered area height and the total number of choices.
    let total = app.choices.len();
    let visible = (area.height as usize).min(total);
    if visible == 0 {
        return;
    }
    let start = if total <= visible {
        0
    } else if app.choice_cursor >= visible / 2 {
        let raw = app.choice_cursor.saturating_sub(visible / 2);
        raw.min(total - visible)
    } else {
        0
    };
    let end = (start + visible).min(total);

    let lines: Vec<Line> = app.choices[start..end]
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let abs = start + i;
            let is_sel = abs == app.choice_cursor;
            let indicator = if is_sel { "▶ " } else { "  " };
            let indicator_span = Span::styled(
                indicator,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
            );
            let text_span = if is_sel {
                Span::styled(item.clone(), Style::default().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(item.clone(), Style::default().add_modifier(Modifier::DIM))
            };
            Line::from(vec![indicator_span, text_span])
        })
        .collect();

    let para = Paragraph::new(lines);
    f.render_widget(para, area);
}

/// Render the picker overlay if one is open.
///
/// Why: `/model` and `/provider` (no arg) open a modal list so the user can
/// pick interactively instead of typing a model id from memory. Drawn last
/// so it sits on top of the chat / input bar.
/// What: Centered popup (~50% × 60%), `Clear` widget under the border so the
/// chat behind it is masked, items rendered as a `List` with the selected
/// row highlighted (cyan + bold + ● marker), footer hint at the bottom.
/// Test: Manual via tmux REPL — open `/model` and `/provider`, navigate, Esc.
fn draw_picker(f: &mut ratatui::Frame, app: &ReplApp) {
    let Some(picker) = &app.picker else { return };

    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = picker
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let is_sel = i == picker.selected;
            let content = if is_sel {
                format!("● {}", item)
            } else {
                format!("  {}", item)
            };
            let style = if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(content).style(style)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" {} ", picker.title),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    // Reserve the bottom row of the popup for the footer hint.
    let inner = block.inner(area);
    f.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default();
    list_state.select(Some(picker.selected));
    f.render_stateful_widget(list, split[0], &mut list_state);

    let hint = Paragraph::new(Line::from(Span::styled(
        "↑↓ navigate  Enter select  Esc cancel",
        Style::default().add_modifier(Modifier::DIM),
    )))
    .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(hint, split[1]);
}

/// Render the bottom rich statusline row.
///
/// Why: Replaces the dim `User`/segment-string statusline with a high-signal
/// info strip (mirroring Claude Code) showing harness identity, LLM provider
/// + model, tool/skill/MCP counts, and a hint to `/help`. Users want to see
/// at-a-glance what the next dispatch will use without scraping logs.
/// What: Bracketed `[open-mpm]` (cyan/bold) + green `✓` + `LLM:` label
/// (normal) + `provider (model)` (bold) + dim `·` separators + `Tools/Skills/
/// MCP` counts + dim trailer. Falls back to the plain `render_statusline`
/// (User/segment) string if `app.status_line` is unset (defensive).
/// Test: Visual via `scripts/tmux-repl-test.sh` (asserts "All systems go" appears).
fn draw_statusline(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    let line = build_rich_statusline(app);
    let p = Paragraph::new(line);
    f.render_widget(p, area);
}

/// Compose the rich statusline as a styled `Line`.
///
/// Why: Pulled out of `draw_statusline` so unit tests can assert the span
/// composition without a Terminal. The function is span-explicit (rather than
/// re-parsing `status_line`) because the underlying counts already live on
/// `ReplApp` (well — the original status string does). To keep the change
/// minimal and avoid plumbing new fields, we use `app.status_line` as the
/// source of truth: it's built once at startup with the exact counts and
/// the format is stable. We only re-style its parts.
/// What: If `status_line` is `Some`, return the bracketed `[open-mpm] <body>`
/// styled line. Otherwise fall back to the legacy `render_statusline`.
/// Test: `rich_statusline_renders_brackets_and_body`.
fn build_rich_statusline(app: &ReplApp) -> Line<'static> {
    let prefix_spans = vec![Span::styled(
        "[open-mpm] ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];

    let Some(status) = app.status_line.as_ref() else {
        // Legacy fallback: render the configured segment string dim.
        let text = render_statusline(app);
        let mut spans = prefix_spans;
        spans.push(Span::styled(
            text,
            Style::default().add_modifier(Modifier::DIM),
        ));
        return Line::from(spans);
    };

    // Re-style the well-known startup status string. The format is built in
    // `src/repl/mod.rs` as:
    //   "✓ LLM: {provider} ({model}) · All systems go."
    // We split on " · " so each chunk can be styled independently. Token
    // counts + estimated cost are injected dynamically from `app` (#293)
    // BEFORE the trailing `All systems go.` chunk, but only when the session
    // has accumulated tokens.
    let mut spans = prefix_spans;
    let raw_chunks: Vec<&str> = status.split(" · ").collect();
    // Build a vector of owned chunk strings with token/cost spliced in.
    let mut chunks: Vec<String> = Vec::with_capacity(raw_chunks.len() + 4);
    let has_tokens = app.tokens_in > 0 || app.tokens_out > 0;
    // #319: TM segment surfaces the live session count.
    let tm_chunk = format!(
        "TM: {} session{}",
        app.tm_session_count,
        if app.tm_session_count == 1 { "" } else { "s" }
    );
    // #319: local inference model segment — shown only when enabled+available.
    // Strip the "ollama/" vendor prefix so the display stays compact.
    let local_model_chunk: Option<String> = app.local_model.as_deref().map(|m| {
        let display = m.strip_prefix("ollama/").unwrap_or(m);
        format!("local: {display}")
    });
    let session_cost = crate::usage::daily::cost_from_tokens(app.tokens_in, app.tokens_out);
    let daily_cost = app.daily_cost_start + session_cost;
    // Show the daily-total segment only when there was prior usage today —
    // i.e. the "today" total exceeds the in-flight session cost. On a fresh
    // day, daily == session and the extra segment would be redundant.
    let show_daily = app.daily_cost_start > 0.0 && daily_cost > session_cost + 1e-9;
    for chunk in &raw_chunks {
        if has_tokens && chunk.starts_with("All systems go.") {
            chunks.push(format_token_chunk(app.tokens_in, app.tokens_out));
            if show_daily {
                chunks.push(format!("{} session", format_cost_value(session_cost)));
                chunks.push(format!("{} today", format_cost_value(daily_cost)));
            } else {
                chunks.push(format_cost_chunk(app.tokens_in, app.tokens_out));
            }
        }
        chunks.push((*chunk).to_string());
        // #319: insert the TM session count segment immediately after the
        // `LLM:` chunk so it sits at the high-signal end of the statusline.
        // Also insert the local model segment (when active) right after TM.
        if chunk.starts_with("✓ LLM:") {
            chunks.push(tm_chunk.clone());
            // #331: surface a distinct claude-mpm session count segment when
            // any TM session is running the claude-mpm adapter. Suppressed
            // when zero so the statusline doesn't carry empty noise.
            if app.claude_mpm_session_count > 0 {
                chunks.push(format!(
                    "style:claude_mpm MPM: {} session{}",
                    app.claude_mpm_session_count,
                    if app.claude_mpm_session_count == 1 {
                        ""
                    } else {
                        "s"
                    }
                ));
            }
            if let Some(ref lm) = local_model_chunk {
                chunks.push(lm.clone());
            }
        }
    }
    for (i, chunk) in chunks.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        spans.extend(style_status_chunk(chunk));
    }
    Line::from(spans)
}

/// Format the `↑1.2k ↓0.8k` token chunk for the statusline.
///
/// Why: Compact, two-arrow form mirrors Claude Code's status row.
/// What: Uses `format_tokens` (k-suffix when ≥1000) for both directions.
/// Test: `format_token_chunk_compacts_thousands`.
fn format_token_chunk(tokens_in: u64, tokens_out: u64) -> String {
    format!(
        "↑{} ↓{}",
        format_tokens(tokens_in),
        format_tokens(tokens_out)
    )
}

/// Format the `$0.0034` estimated-cost chunk for the statusline.
///
/// Why: Surfaces approximate spend at-a-glance using OpenRouter haiku
/// pricing (a reasonable default for the most-common harness model).
/// What: Cost = prompt_tokens * $0.00000025 + completion_tokens * $0.00000125.
/// Format with 4 decimals if <$0.01, 3 decimals otherwise.
/// Test: `format_cost_chunk_thresholds`.
fn format_cost_chunk(tokens_in: u64, tokens_out: u64) -> String {
    let cost = crate::usage::daily::cost_from_tokens(tokens_in, tokens_out);
    format_cost_value(cost)
}

/// Format a USD cost value with the same threshold rules as `format_cost_chunk`.
///
/// Why: Shared by the bare `$0.0034` chunk and the `$0.0034 session` /
/// `$0.0145 today` segments so the two never disagree on precision.
/// What: 4 decimals when cost < $0.01, 3 decimals otherwise.
/// Test: `format_cost_value_thresholds`.
fn format_cost_value(cost: f64) -> String {
    if cost < 0.01 {
        format!("${:.4}", cost)
    } else {
        format!("${:.3}", cost)
    }
}

/// Best-effort flush of the daily usage file, throttled to once per
/// `USAGE_WRITE_INTERVAL`.
///
/// Why: Token updates fire on every chunk; we don't want to fsync on each.
/// Throttling protects the disk while still surviving most crashes (worst
/// case: lose ≤5 s of in-flight cost).
/// What: Recomputes session cost from `tokens_in`/`tokens_out`, builds a
/// `DailyUsage` with today's date and `daily_cost_start + session_cost`,
/// then writes it atomically. I/O errors log at debug and are swallowed —
/// daily totals are observability, not control flow.
/// Test: `persist_daily_usage_writes_after_interval` (synchronous helper).
fn persist_daily_usage_if_due(app: &mut ReplApp) {
    let now = std::time::Instant::now();
    let due = match app.last_usage_write {
        None => true,
        Some(last) => now.duration_since(last) >= USAGE_WRITE_INTERVAL,
    };
    if !due {
        return;
    }
    let session_cost = crate::usage::daily::cost_from_tokens(app.tokens_in, app.tokens_out);
    let record = crate::usage::daily::DailyUsage {
        date: crate::usage::daily::today_local(),
        // Note: prompt_tokens / completion_tokens are *daily* totals on disk.
        // We don't carry forward prior session token counts (only their
        // cost), so this is best-effort: the cost line is the canonical
        // value, the token counts reflect this session only when no prior
        // session ran today. Good enough for the at-a-glance display.
        prompt_tokens: app.tokens_in,
        completion_tokens: app.tokens_out,
        cost_usd: app.daily_cost_start + session_cost,
    };
    if let Err(e) = crate::usage::daily::save_atomic(&app.usage_project_dir, &record) {
        tracing::debug!(error = %e, "daily usage: write failed");
    }
    app.last_usage_write = Some(now);
}

/// Style one ` · `-separated chunk of the startup status string.
///
/// Why: Each chunk has different semantics — the leading `✓ LLM: …` chunk
/// gets a green tick + bold model, the `Tools/Skills/MCP` chunks get dim
/// labels with normal counts, and the trailing `All systems go. Type /help …`
/// chunk gets green for the success message and dim for the hint.
/// What: Recognize chunk by prefix; emit a span vector. Unknown chunks pass
/// through dim.
/// Test: `rich_statusline_chunks_styled` covers each branch.
fn style_status_chunk(chunk: &str) -> Vec<Span<'static>> {
    // Leading `✓ LLM: provider (model)`.
    if let Some(rest) = chunk.strip_prefix("✓ LLM: ") {
        return vec![
            Span::styled("✓ ", Style::default().fg(Color::Green)),
            Span::raw("LLM: "),
            Span::styled(
                rest.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];
    }
    // Trailing combined chunk: `All systems go.` (#293 dropped the
    // `Type /help for commands.` hint). Defensively handle either form.
    if let Some(rest) = chunk.strip_prefix("All systems go.") {
        return vec![
            Span::styled("All systems go.", Style::default().fg(Color::Green)),
            Span::styled(
                rest.to_string(),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ];
    }
    // Token/cost chunks (#293): `↑1.2k ↓0.8k` and `$0.003` render normal.
    if chunk.starts_with('↑') || chunk.starts_with('$') {
        return vec![Span::raw(chunk.to_string())];
    }
    // #319: TM session count segment — bold count, dim label.
    if let Some(rest) = chunk.strip_prefix("TM: ") {
        return vec![
            Span::styled("TM: ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                rest.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];
    }
    // #331: claude-mpm session count segment. The `style:claude_mpm ` sentinel
    // prefix is stripped here and the remainder rendered in bright magenta to
    // visually distinguish it from the dim/bold TM count.
    if let Some(rest) = chunk.strip_prefix("style:claude_mpm ") {
        // Split label ("MPM: ") from value ("N sessions") for hybrid styling.
        let (label, value) = match rest.find(": ") {
            Some(i) => (&rest[..i + 2], &rest[i + 2..]),
            None => ("", rest),
        };
        return vec![
            Span::styled(
                label.to_string(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(
                value.to_string(),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
    }
    // Unknown chunk — pass through dim.
    vec![Span::styled(
        chunk.to_string(),
        Style::default().add_modifier(Modifier::DIM),
    )]
}

/// Produce the welcome banner as a `Vec<Line<'static>>` so it can be prepended
/// to the chat scroll buffer instead of occupying its own layout chunk.
///
/// Why: Treating the banner as part of the chat scrollback lets it scroll
/// upward (and eventually off the top) as new messages arrive — exactly like
/// terminal command history. This eliminates the abrupt "banner disappears
/// when first message is sent" UX from the previous design.
/// What: Returns the same content `draw_banner()` rendered as widgets, but as
/// raw `Line`s. Layout: left ASCII-art column, vertical `│` divider, right
/// info column. Column widths are computed from `width` (25% / 1 / rest).
/// Test: Visual via `scripts/tmux-repl-test.sh`; mechanical via the
/// `banner_lines_*` unit tests.
fn banner_lines(app: &ReplApp, width: usize) -> Vec<Line<'static>> {
    let version = env!("CARGO_PKG_VERSION");

    let total_w = width.max(40);
    let left_w = (total_w / 4).max(18);
    let div_w = 1usize;
    let right_w = total_w.saturating_sub(left_w + div_w + 2 /* margins */);

    // Left column rows (centered ASCII art + identity).
    let left_rows: Vec<(String, Option<Color>)> = vec![
        (String::new(), None),
        (String::new(), None),
        ("▐▛███▜▌ ▐▛███▜▌".to_string(), Some(Color::Cyan)),
        ("▝▜█████▛▘▝▜█████▛▘".to_string(), Some(Color::Cyan)),
        ("▘▘ ▝▝    ▘▘ ▝▝".to_string(), Some(Color::Cyan)),
        (String::new(), None),
        (format!("{} · {}", app.user_label, app.project_name), None),
    ];

    // Right column rows: app title + recent activity + commands.
    let mut right_rows: Vec<(String, Style)> = Vec::new();
    right_rows.push((
        format!(" Open MPM v{}", version),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    right_rows.push((String::new(), Style::default()));
    right_rows.push((
        " Recent activity".to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    for c in app.git_commits.iter().take(3) {
        right_rows.push((format!(" {}", c), Style::default()));
    }
    while right_rows.len() < 7 {
        right_rows.push((String::new(), Style::default()));
    }
    right_rows.push((
        " Commands".to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    right_rows.push((
        "   /help       - show all commands".to_string(),
        Style::default(),
    ));
    right_rows.push((
        "   /connect    - attach to a project".to_string(),
        Style::default(),
    ));
    right_rows.push((
        "   /clear      - reset conversation".to_string(),
        Style::default(),
    ));
    right_rows.push((
        "   /status     - show agent status".to_string(),
        Style::default(),
    ));

    let row_count = left_rows.len().max(right_rows.len());
    let mut out: Vec<Line<'static>> = Vec::with_capacity(row_count + 2);

    // Top rule. Uses a rounded-corner frame (`╭─── … ─╮`) so the banner reads
    // as a self-contained frame rather than fading into the chat scroll. The
    // tmux e2e test (`scripts/tmux-repl-test.sh`) asserts the literal
    // `╭─── open-mpm ctrl` substring, so the leading `╭───` and the title
    // format must be preserved verbatim.
    let title = format!("╭─── open-mpm ctrl  v{} ", version);
    let mut top = String::with_capacity(total_w);
    top.push_str(&title);
    while top.chars().count() + 1 < total_w {
        top.push('─');
    }
    top.push('╮');
    out.push(Line::from(Span::styled(
        top,
        Style::default().fg(Color::Cyan),
    )));

    // Body rows.
    for i in 0..row_count {
        let (left_raw, left_color) = left_rows
            .get(i)
            .cloned()
            .unwrap_or_else(|| (String::new(), None));
        let (right_raw, right_style) = right_rows
            .get(i)
            .cloned()
            .unwrap_or_else(|| (String::new(), Style::default()));

        // Center left text within left_w.
        let left_chars = left_raw.chars().count();
        let pad_total = left_w.saturating_sub(left_chars);
        let pad_left = pad_total / 2;
        let pad_right = pad_total - pad_left;
        let left_padded = format!(
            "{}{}{}",
            " ".repeat(pad_left),
            left_raw,
            " ".repeat(pad_right)
        );
        let left_style = match left_color {
            Some(c) => Style::default().fg(c),
            None => Style::default(),
        };

        // Truncate right to right_w.
        let right_truncated: String = right_raw.chars().take(right_w).collect();

        let spans = vec![
            Span::styled(left_padded, left_style),
            Span::styled(" ", Style::default()),
            Span::styled("│", Style::default().fg(Color::DarkGray)),
            Span::styled(" ", Style::default()),
            Span::styled(right_truncated, right_style),
        ];
        out.push(Line::from(spans));
    }

    // Bottom rule: closes the banner with a rounded-corner frame matching
    // the top bar, so the splash reads as a bounded box rather than fading
    // into the chat scroll. The tmux e2e test (`scripts/tmux-repl-test.sh`)
    // asserts the literal `╰─` opener.
    let mut bottom = String::with_capacity(total_w);
    bottom.push('╰');
    while bottom.chars().count() + 1 < total_w {
        bottom.push('─');
    }
    bottom.push('╯');
    out.push(Line::from(Span::styled(
        bottom,
        Style::default().fg(Color::DarkGray),
    )));

    out
}

#[allow(dead_code)]
fn draw_banner(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    // Use the compile-time crate version so the banner always matches the
    // shipped binary without an extra build step.
    let version = env!("CARGO_PKG_VERSION");
    // Title format mirrors the legacy banner so tmux test assertions
    // (`╭─── open-mpm ctrl`) keep working.
    let title = format!("─── open-mpm ctrl  v{} ", version);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(title, Style::default().fg(Color::Cyan)));

    let inner = block.inner(area);

    // Three-column inner layout: 25% identity / 1-char vertical divider /
    // remainder for activity+commands. The divider visually splits the
    // ASCII robot art on the left from the right-side content.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    f.render_widget(block, area);

    // Left panel.
    let left_lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "▐▛███▜▌ ▐▛███▜▌",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "▝▜█████▛▘▝▜█████▛▘",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "▘▘ ▝▝    ▘▘ ▝▝",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(format!("{} · {}", app.user_label, app.project_name)),
    ];
    let left = Paragraph::new(left_lines).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(left, cols[0]);

    // Vertical divider column: render a `│` glyph for each row so it spans
    // the full height of the banner inner area. Subtle dim grey so it reads
    // as a separator without competing with the cyan border.
    let divider_lines: Vec<Line<'static>> = (0..cols[1].height)
        .map(|_| Line::from(Span::styled("│", Style::default().fg(Color::DarkGray))))
        .collect();
    let divider = Paragraph::new(divider_lines);
    f.render_widget(divider, cols[1]);

    // Right panel: app title header, recent activity, commands.
    let mut right_lines: Vec<Line> = Vec::with_capacity(12);
    // App title — prominent at the top of the right column.
    right_lines.push(Line::from(Span::styled(
        format!(" Open MPM v{}", version),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    right_lines.push(Line::from(""));
    right_lines.push(Line::from(Span::styled(
        " Recent activity",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for c in app.git_commits.iter().take(3) {
        right_lines.push(Line::from(format!(" {}", c)));
    }
    while right_lines.len() < 7 {
        right_lines.push(Line::from(""));
    }
    right_lines.push(Line::from(Span::styled(
        " Commands",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    right_lines.push(Line::from("   /help       - show all commands"));
    right_lines.push(Line::from("   /connect    - attach to a project"));
    right_lines.push(Line::from("   /clear      - reset conversation"));
    right_lines.push(Line::from("   /status     - show agent status"));
    let right = Paragraph::new(right_lines).wrap(Wrap { trim: false });
    f.render_widget(right, cols[2]);
}

/// Parse a fenced code-block opener line and return the language tag.
///
/// Why: We need to distinguish opening fences (` ```bash `) from closing
/// fences (` ``` `) and from non-fence content. Returning `Some(lang)` for
/// openers (including `Some("")` for bare ``` ``` openers) and `None` for
/// non-fence lines lets the renderer drive its state machine cleanly.
/// What: Trims the line; if it starts with three backticks, returns the
/// trailing tag (lowercased) — empty string for bare openers, `None` for
/// anything that isn't a fence line at all.
/// Test: `code_fence_lang_*` asserts bash/sh/empty/non-fence cases.
fn code_fence_lang(line: &str) -> Option<String> {
    let t = line.trim();
    t.strip_prefix("```")
        .map(|rest| rest.trim().to_ascii_lowercase())
}

/// Whether a fenced-code-block language tag denotes an executable shell.
///
/// Why: Shell blocks get the bright-green `▶` indicator and feed the
/// Ctrl+E paste buffer; non-shell blocks (rust, python, json, …) use the
/// neutral `⬡` indicator and are NOT pasted into the input.
/// What: Matches `bash`, `sh`, `zsh`, `fish` (case-insensitive — caller
/// passes a lowercased tag).
/// Test: `is_executable_shell_lang_*` asserts each tag.
fn is_executable_shell_lang(lang: &str) -> bool {
    matches!(lang, "bash" | "sh" | "zsh" | "fish")
}

/// Extract the last executable-shell fenced code block body from a message.
///
/// Why: When an assistant message offers multiple shell blocks, Ctrl+E should
/// paste the *most recently shown* one — i.e. the last block in the message.
/// What: Walks the lines, tracks fence state, and returns `Some(body)` of
/// the last completed bash/sh/zsh/fish block. Body is joined with `\n`.
/// Returns `None` if no executable shell block is found (or the block was
/// never closed).
/// Test: `extract_last_shell_block_*` unit tests.
fn extract_last_shell_block(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let mut current_body: Option<Vec<String>> = None;
    let mut in_shell = false;
    for line in text.lines() {
        if let Some(lang) = code_fence_lang(line) {
            if let Some(body) = current_body.take() {
                // Closing fence
                if in_shell {
                    last = Some(body.join("\n"));
                }
                in_shell = false;
            } else {
                // Opening fence
                in_shell = is_executable_shell_lang(&lang);
                current_body = Some(Vec::new());
            }
        } else if let Some(body) = current_body.as_mut() {
            body.push(line.to_string());
        }
    }
    // Intentional: if `current_body` is Some here, it means the last fence
    // was opened but never closed (e.g. model truncated at max_tokens mid-block).
    // We discard the partial body — Ctrl+E should not paste an incomplete command.
    // This is distinct from a bug; the completed blocks accumulated in `last`
    // (if any) are returned instead.
    last
}

/// Detect a markdown table row: trimmed line starts with `|`.
///
/// Why: Table detection is the gate for box-drawing rendering — non-table
/// lines fall through to plain rendering.
/// What: Returns true if the trimmed line starts with `|`.
/// Test: Assert true for "| a | b |" and "  |x|", false for "hello" or "".
fn is_md_table_row(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('|')
}

/// Detect a markdown table separator row: cells contain only `-`, `:`, spaces.
///
/// Why: The separator row distinguishes header from body and confirms a block
/// is actually a markdown table (not just a line that happens to start with `|`).
/// What: Returns true if every non-empty cell after split-on-`|` is composed
/// solely of `-`, `:`, or whitespace, AND at least one `-` appears.
/// Test: Assert true for "|---|---|" and "|:--|--:|", false for "| a | b |".
fn is_md_table_separator(line: &str) -> bool {
    if !is_md_table_row(line) {
        return false;
    }
    let cells = parse_md_table_cells(line);
    if cells.is_empty() {
        return false;
    }
    let mut saw_dash = false;
    for c in &cells {
        for ch in c.chars() {
            match ch {
                '-' => saw_dash = true,
                ':' | ' ' | '\t' => {}
                _ => return false,
            }
        }
    }
    saw_dash
}

/// Split a markdown table row on `|` and trim each cell.
///
/// Why: Markdown table rows have leading and trailing pipes that produce
/// empty cells when split naïvely; consumers want only the real cell content.
/// What: Returns the trimmed cell strings, dropping leading/trailing empties
/// produced by the bordering pipes.
/// Test: Assert "| a | b |" yields ["a", "b"]; "|x|y|z|" yields ["x","y","z"].
fn parse_md_table_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let mut parts: Vec<String> = trimmed.split('|').map(|s| s.trim().to_string()).collect();
    // Drop leading empty (from leading `|`).
    if parts.first().map(|s| s.is_empty()).unwrap_or(false) {
        parts.remove(0);
    }
    // Drop trailing empty (from trailing `|`).
    if parts.last().map(|s| s.is_empty()).unwrap_or(false) {
        parts.pop();
    }
    parts
}

/// Truncate a string to `max` display chars, appending `…` if shortened.
///
/// Why: Table cells must fit within column width budget; oversized content
/// gets a visual ellipsis so the user knows truncation happened.
/// What: Returns the input unchanged if it fits, otherwise the first
/// `max-1` chars + `…`. If `max == 0`, returns empty string.
/// Test: Assert "abc" with max=5 returns "abc"; "abcdef" with max=4 returns "abc…".
fn truncate_cell(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Render a parsed markdown table as box-drawing styled `Line`s.
///
/// Why: Inline rendering keeps the table flush with surrounding chat text,
/// avoiding the layout split required by ratatui's `Table` widget. Box
/// characters give a clean visual frame that scans well in monospaced fonts.
/// What: Given header + body rows, computes per-column widths (max of header
/// and any body cell), clamps total table width to `available_width`, and
/// emits top border, header row, separator, body rows, bottom border. Border
/// glyphs use `Color::DarkGray`; cell content uses `body_color` if provided.
/// Test: Pass a 3x3 table (1 header row + 2 body rows), assert the returned
/// Vec has 6 lines (top, header, sep, 2 body, bottom), each starts with the
/// indent prefix, and the first/last lines contain `┌`/`└`.
fn render_markdown_table(
    header: &[String],
    body: &[Vec<String>],
    available_width: usize,
    indent: &str,
    body_color: Option<Color>,
) -> Vec<Line<'static>> {
    let ncols = header
        .len()
        .max(body.iter().map(|r| r.len()).max().unwrap_or(0));
    if ncols == 0 {
        return Vec::new();
    }

    // Normalize all rows to ncols by padding with empty cells.
    let header_n: Vec<String> = (0..ncols)
        .map(|i| header.get(i).cloned().unwrap_or_default())
        .collect();
    let body_n: Vec<Vec<String>> = body
        .iter()
        .map(|r| {
            (0..ncols)
                .map(|i| r.get(i).cloned().unwrap_or_default())
                .collect()
        })
        .collect();

    // Step 1: ideal column widths from content.
    let mut widths: Vec<usize> = (0..ncols)
        .map(|i| {
            let mut w = header_n[i].chars().count();
            for r in &body_n {
                w = w.max(r[i].chars().count());
            }
            w
        })
        .collect();

    // Step 2: compute total table width with " cell " padding (1 space each
    // side) and `│` borders. Total = indent + 1 (left border) + sum(2 + w_i)
    // + ncols (right borders, one per column).
    let indent_w = indent.chars().count();
    let frame_overhead = 1 + ncols; // left `│` + one `│` per column on right
    let padding_per_col = 2; // " " on each side of cell content
    let mut total: usize =
        indent_w + frame_overhead + widths.iter().map(|w| w + padding_per_col).sum::<usize>();

    // Step 3: if too wide, shrink the widest column repeatedly until we fit
    // or every column is at minimum width 1.
    let limit = available_width.max(indent_w + frame_overhead + ncols * (padding_per_col + 1));
    while total > available_width && available_width > 0 {
        // Find widest column with width > 1.
        let widest = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > 1)
            .max_by_key(|(_, w)| **w)
            .map(|(i, _)| i);
        match widest {
            Some(i) => {
                widths[i] -= 1;
                total -= 1;
            }
            None => break,
        }
    }
    let _ = limit; // referenced for clarity above; not used after shrink loop.

    // Step 4: build border row helper (top/sep/bottom variants).
    let border_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let cell_style = match body_color {
        Some(c) => Style::default().fg(c),
        None => Style::default(),
    };

    let make_border = |left: char, mid: char, right: char| -> String {
        let mut s = String::new();
        s.push_str(indent);
        s.push(left);
        for (i, w) in widths.iter().enumerate() {
            for _ in 0..(w + padding_per_col) {
                s.push('─');
            }
            if i + 1 < widths.len() {
                s.push(mid);
            }
        }
        s.push(right);
        s
    };

    let make_data_row = |cells: &[String]| -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(ncols * 2 + 2);
        spans.push(Span::raw(indent.to_string()));
        spans.push(Span::styled("│".to_string(), border_style));
        for (i, w) in widths.iter().enumerate() {
            let cell = truncate_cell(&cells[i], *w);
            let pad_right = w.saturating_sub(cell.chars().count());
            let mut content = String::with_capacity(2 + w);
            content.push(' ');
            content.push_str(&cell);
            for _ in 0..pad_right {
                content.push(' ');
            }
            content.push(' ');
            spans.push(Span::styled(content, cell_style));
            let _ = i;
            spans.push(Span::styled("│".to_string(), border_style));
        }
        Line::from(spans)
    };

    let mut out: Vec<Line<'static>> = Vec::with_capacity(body_n.len() + 4);
    out.push(Line::from(Span::styled(
        make_border('┌', '┬', '┐'),
        border_style,
    )));
    out.push(make_data_row(&header_n));
    out.push(Line::from(Span::styled(
        make_border('├', '┼', '┤'),
        border_style,
    )));
    for row in &body_n {
        out.push(make_data_row(row));
    }
    out.push(Line::from(Span::styled(
        make_border('└', '┴', '┘'),
        border_style,
    )));
    out
}

/// Build the rendered `Vec<Line>` for the chat pane.
///
/// Why: Extracted from `draw_chat` so layout code in `draw()` can compute the
/// chat content height *before* layout splitting, enabling input to follow
/// content (issue #337) instead of being pinned to the screen bottom by
/// `Constraint::Min(3)`.
/// What: Replicates the banner + chat entry line-building logic, returns the
/// post-collapse, pre-pad `Vec<Line>`. Width-dependent rendering (banner
/// wrapping, markdown table column sizing) uses the supplied `terminal_width`.
/// Test: Compare `build_chat_lines(app, w).len()` against the line count
/// observed inside `draw_chat` for representative `app.chat` fixtures.
fn build_chat_lines(app: &ReplApp, terminal_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Prepend the welcome banner as part of the chat scroll buffer. As new
    // messages arrive, the banner scrolls upward like terminal command history
    // and eventually disappears off the top — no explicit hide step needed.
    if app.show_banner {
        lines.extend(banner_lines(app, terminal_width));
        lines.push(Line::from("")); // gap between banner and first chat entry
    }

    let chat_len = app.chat.len();
    for (idx, entry) in app.chat.iter().enumerate() {
        match entry.role {
            ChatRole::User => {
                let mut iter = entry.text.lines();
                if let Some(first) = iter.next() {
                    lines.push(Line::from(vec![
                        Span::styled("❯", Style::default().fg(Color::Green)),
                        Span::raw(" "),
                        Span::raw(first.to_string()),
                    ]));
                }
                for cont in iter {
                    lines.push(Line::from(format!("  {}", cont)));
                }
                // Note: previously rendered `⟳ thinking…` lines beneath the
                // latest user prompt while the LLM was busy. Removed — the
                // activity strip (`draw_activity`) is now the sole live
                // thinking indicator. The `thinking_lines` event flow is
                // unaffected and still feeds row 3 of the activity strip.
            }
            ChatRole::Assistant | ChatRole::Error => {
                let body_color = if entry.role == ChatRole::Error {
                    Some(Color::Red)
                } else {
                    None
                };
                // Collect lines into a Vec for index-based table-block
                // lookahead. Walking with an iterator alone makes it awkward
                // to peek the separator row that distinguishes a table from
                // an arbitrary `|`-prefixed line.
                let body_lines: Vec<&str> = entry.text.lines().collect();
                let mut i = 0usize;
                let mut emitted_first = false;
                while i < body_lines.len() {
                    // Fenced code-block detection (#321). A line of the form
                    // ` ```<lang> ` opens a block; the next ` ``` ` closes it.
                    // Executable shell blocks (bash/sh/zsh/fish) get a bright
                    // green `▶ <lang>` header; other languages get a neutral
                    // `⬡ <lang>` header in light blue. Body lines render as
                    // dim gray under the standard 3-space indent. The closing
                    // fence renders as a thin dim separator.
                    if let Some(lang) = code_fence_lang(body_lines[i]) {
                        let is_shell = is_executable_shell_lang(&lang);
                        // Emit the agent leader if this is the first body
                        // element — keeps the `⏺ <name> · ` prefix consistent
                        // with table/prose paths above.
                        if !emitted_first {
                            let mut spans = vec![
                                Span::styled("⏺", Style::default().fg(Color::Indexed(208))),
                                Span::raw(" "),
                            ];
                            if body_color.is_none() {
                                let label_color = match app.agent_scope {
                                    AgentScope::User => Color::Cyan,
                                    AgentScope::Project => Color::Yellow,
                                };
                                spans.push(Span::styled(
                                    app.project_name.clone(),
                                    Style::default()
                                        .fg(label_color)
                                        .add_modifier(Modifier::BOLD),
                                ));
                                spans.push(Span::styled(
                                    " · ",
                                    Style::default().add_modifier(Modifier::DIM),
                                ));
                            }
                            lines.push(Line::from(spans));
                            emitted_first = true;
                        }
                        // Header line.
                        let header_label = if lang.is_empty() {
                            "code".to_string()
                        } else {
                            lang.clone()
                        };
                        if is_shell {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    format!("▶ {}", header_label),
                                    Style::default()
                                        .fg(Color::LightGreen)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                        } else {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    format!("⬡ {}", header_label),
                                    Style::default().fg(Color::Indexed(75)),
                                ),
                            ]));
                        }
                        // Body lines until the closing fence (or EOF).
                        let mut j = i + 1;
                        let body_style = Style::default().fg(Color::Indexed(244));
                        while j < body_lines.len() {
                            if code_fence_lang(body_lines[j]).is_some() {
                                break;
                            }
                            lines.push(Line::from(Span::styled(
                                format!("   {}", body_lines[j]),
                                body_style,
                            )));
                            j += 1;
                        }
                        // Closing fence — emit a thin dim separator.
                        // #324: Verify j landed on a real *closer* (lang == ""),
                        // not a nested opener. If body_lines[j] is itself a
                        // fence opener (e.g. docs example showing ```bash`
                        // inside another block), treating it as a closer would
                        // advance `i` past a real fence start and corrupt all
                        // subsequent rendering. Treat malformed/nested cases
                        // as unclosed: don't consume the line at j.
                        let j_is_closer = j < body_lines.len()
                            && code_fence_lang(body_lines[j]).is_some_and(|lang| lang.is_empty());
                        if j_is_closer {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    "─".repeat(20),
                                    Style::default().fg(Color::Indexed(240)),
                                ),
                            ]));
                            i = j + 1;
                        } else {
                            // Nested opener or EOF — block is unclosed; no
                            // closing separator. Don't consume body_lines[j]
                            // (it's either EOF or a new fence opener that the
                            // outer loop must process).
                            i = j;
                        }
                        continue;
                    }
                    // Try to detect a markdown table starting at i: header
                    // row + separator row + zero or more data rows.
                    let is_table_start = i + 1 < body_lines.len()
                        && is_md_table_row(body_lines[i])
                        && is_md_table_separator(body_lines[i + 1]);

                    if is_table_start {
                        let header = parse_md_table_cells(body_lines[i]);
                        let mut j = i + 2;
                        let mut body_rows: Vec<Vec<String>> = Vec::new();
                        while j < body_lines.len() && is_md_table_row(body_lines[j]) {
                            body_rows.push(parse_md_table_cells(body_lines[j]));
                            j += 1;
                        }
                        // Indent under the `⏺ ` glyph (3 spaces) — matches
                        // how non-table continuation lines are indented so
                        // the table aligns with surrounding body text.
                        // Both branches currently render the same three-space
                        // continuation indent; kept as a single constant for
                        // clarity (and to make future per-branch styling a
                        // one-line tweak).
                        let indent = "   ";
                        // Available width: the chat pane width. Subtract a
                        // tiny safety margin for ratatui's wrap behavior.
                        let avail = terminal_width.saturating_sub(1);
                        let table_lines =
                            render_markdown_table(&header, &body_rows, avail, indent, body_color);
                        // If this is the very first body line, we still need
                        // to emit the leader (`⏺ <name> · `) before the table.
                        // Produce a minimal leader line, then push table rows.
                        if !emitted_first {
                            let mut spans = vec![
                                Span::styled("⏺", Style::default().fg(Color::Indexed(208))),
                                Span::raw(" "),
                            ];
                            if body_color.is_none() {
                                let label_color = match app.agent_scope {
                                    AgentScope::User => Color::Cyan,
                                    AgentScope::Project => Color::Yellow,
                                };
                                spans.push(Span::styled(
                                    app.project_name.clone(),
                                    Style::default()
                                        .fg(label_color)
                                        .add_modifier(Modifier::BOLD),
                                ));
                                spans.push(Span::styled(
                                    " · ",
                                    Style::default().add_modifier(Modifier::DIM),
                                ));
                            }
                            // Empty content span — table follows on next line.
                            lines.push(Line::from(spans));
                            emitted_first = true;
                        }
                        for tl in table_lines {
                            lines.push(tl);
                        }
                        i = j;
                        continue;
                    }

                    // Non-table line: emit as before.
                    let raw = body_lines[i];
                    if !emitted_first {
                        let mut spans = vec![
                            Span::styled("⏺", Style::default().fg(Color::Indexed(208))),
                            Span::raw(" "),
                        ];
                        if body_color.is_none() {
                            let label_color = match app.agent_scope {
                                AgentScope::User => Color::Cyan,
                                AgentScope::Project => Color::Yellow,
                            };
                            spans.push(Span::styled(
                                app.project_name.clone(),
                                Style::default()
                                    .fg(label_color)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            spans.push(Span::styled(
                                " · ",
                                Style::default().add_modifier(Modifier::DIM),
                            ));
                        }
                        spans.push(match body_color {
                            Some(c) => Span::styled(raw.to_string(), Style::default().fg(c)),
                            None => Span::raw(raw.to_string()),
                        });
                        lines.push(Line::from(spans));
                        emitted_first = true;
                    } else {
                        // Blank source lines render as a TRULY empty Line so
                        // the post-pass `collapse_blank_lines` can fold them
                        // against siblings without the leading "   " indent
                        // throwing off its whitespace-only detection in
                        // adjacent renders. Non-blank continuations keep the
                        // 3-space indent so they line up under the `⏺ ` glyph.
                        let line = if raw.trim().is_empty() {
                            Line::from("")
                        } else {
                            match body_color {
                                Some(c) => Line::from(Span::styled(
                                    format!("   {}", raw),
                                    Style::default().fg(c),
                                )),
                                None => Line::from(Span::raw(format!("   {}", raw))),
                            }
                        };
                        lines.push(line);
                    }
                    i += 1;
                }
            }
            ChatRole::Status => {
                lines.push(Line::from(vec![
                    Span::styled("[open-mpm] ", Style::default().fg(Color::Green)),
                    Span::raw(entry.text.clone()),
                ]));
            }
        }
        // Inter-message separator: insert exactly one blank line between
        // every pair of distinct chat entries (regardless of role) so
        // sections never run together. Skip after the very last entry —
        // the trailing blank spacer below + bottom-pinned layout already
        // provide the gap above the input separator. Any doubling that
        // results from a message ending in its own blank is normalized
        // by the post-pass `collapse_blank_lines`.
        let is_last = idx + 1 == chat_len;
        if !is_last {
            lines.push(Line::from(""));
        }
    }

    // Collapse runs of consecutive blank lines (≥2 → 1) before pinning so
    // markdown-style `\n\n` paragraph breaks don't render as wasted vertical
    // space in the terminal. Operates after wrapping decisions but before
    // pad/scroll math so the geometry math sees the post-collapse line count.
    let mut lines = collapse_blank_lines(lines);

    // Always reserve one trailing blank line as breathing room between the
    // last chat message and the input/separator below. When chat is empty we
    // still skip this — there's nothing to give breathing room from, and
    // adding a stray blank would push the (suppressed) banner geometry off.
    if !app.chat.is_empty() {
        lines.push(Line::from(""));
    }

    lines
}

/// Count chat content lines for layout sizing.
///
/// Why: `draw()` needs to know the rendered content height *before* the
/// vertical layout split so the chat pane can use `Constraint::Length(h)`
/// instead of `Min(3)` (issue #337) — `Min` would expand to fill all
/// available space, pinning the input box to the screen bottom even when
/// the conversation is short.
/// What: Returns the post-collapse, pre-pad line count from `build_chat_lines`.
/// Test: For an empty chat with `show_banner=false`, returns 0; for a
/// non-empty chat, returns at least the chat entry count.
pub fn chat_line_count(app: &ReplApp, terminal_width: usize) -> usize {
    build_chat_lines(app, terminal_width).len()
}

fn draw_chat(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    // The startup `status_line` is now rendered in the bottom statusline row
    // (see `draw_statusline` / `build_rich_statusline`) instead of duplicated
    // here at the top of chat.
    let lines = build_chat_lines(app, area.width as usize);

    // Bottom-pinned chat: when chat has content and it fits in the visible
    // area, prepend blank padding lines so the content sits flush at the
    // BOTTOM of the chat pane (chat-app feel — like iMessage / Slack). On
    // startup with no messages we skip padding entirely so the pane stays
    // visually empty. When content overflows the visible area we don't pad
    // — the existing `max_offset` logic auto-scrolls so the newest line
    // stays in view (the user can PageUp to scroll back).
    let visible = area.height as usize;
    let total = lines.len();

    let final_lines = if !app.chat.is_empty() && total < visible {
        let pad = visible - total;
        let mut padded = Vec::with_capacity(visible);
        for _ in 0..pad {
            padded.push(Line::from(""));
        }
        padded.extend(lines);
        padded
    } else {
        lines
    };

    // When content fits (final_total <= visible), max_offset is 0 → no scroll.
    // When content overflows, max_offset pushes the newest content to the
    // bottom. `app.scroll_offset` lets the user scroll up from pinned.
    let final_total = final_lines.len();
    let max_offset = final_total.saturating_sub(visible);
    // #329: Publish the rendered max so `ReplApp::scroll` can clamp future
    // wheel/PageUp deltas. Shared via Arc<AtomicUsize> so this snapshot's
    // write is visible to the authoritative ReplApp behind the runtime mutex.
    app.last_max_scroll
        .store(max_offset, std::sync::atomic::Ordering::Relaxed);
    let effective_offset = max_offset.saturating_sub(app.scroll_offset);

    let paragraph = Paragraph::new(final_lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_offset as u16, 0));
    f.render_widget(paragraph, area);
}

fn draw_input(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    // Borderless input row — Claude Code-style. The prompt label `<name>>`
    // is sufficient demarcation; the surrounding box wasted a row of vertical
    // space and clashed with the new statusline row below.
    let inner = area;

    // Compose: `<label>> <input>                    [thinking...]`
    let prompt = format!("{}> ", app.project_name);
    let prompt_width = prompt.chars().count();
    let total_width = inner.width as usize;

    let thinking_label = "[thinking...]";
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(prompt.clone()));
    // Empty input: render dim italic placeholder (Claude Code style).
    // - Busy + empty: show "↑ to cancel"
    // - Idle + empty: show the discoverability hint
    // - Non-empty: render the buffer normally
    if app.input_buf.is_empty() {
        let placeholder = if app.thinking || app.busy_since.is_some() {
            "↑ to cancel"
        } else {
            "Ask ctrl anything, or /connect <path> for project work"
        };
        spans.push(Span::styled(
            placeholder.to_string(),
            Style::default()
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        spans.push(Span::raw(app.input_buf.clone()));
    }

    // Right-side decoration: prefer `[thinking...]` while busy; otherwise show
    // token counters when they are non-zero. Tokens render in dim style as
    // `↑{in} ↓{out}` to mirror the StatusBar format.
    let token_label: Option<String> = match (app.tokens_in, app.tokens_out) {
        (0, 0) => None,
        (0, out) => Some(format!("↓{}", out)),
        (inp, 0) => Some(format!("↑{}", inp)),
        (inp, out) => Some(format!("↑{} ↓{}", inp, out)),
    };

    if app.thinking {
        let used = prompt_width + app.input_buf.chars().count();
        let label_w = thinking_label.chars().count();
        if total_width > used + label_w + 1 {
            let pad = total_width - used - label_w;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(
                thinking_label,
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    } else if let Some(ref tok) = token_label {
        let used = prompt_width + app.input_buf.chars().count();
        let label_w = tok.chars().count();
        if total_width > used + label_w + 1 {
            let pad = total_width - used - label_w;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(
                tok.clone(),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }

    let line = Line::from(spans);
    let p = Paragraph::new(line);
    f.render_widget(p, inner);

    // Position the terminal cursor visually at the input position.
    let cursor_col =
        inner.x + (prompt_width as u16) + (app.input_buf[..app.cursor_pos].chars().count() as u16);
    let cursor_row = inner.y;
    f.set_cursor_position((cursor_col, cursor_row));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repl_app_insert_and_backspace() {
        let mut a = ReplApp::new("ctrl".into(), "tester".into());
        a.insert_char('h');
        a.insert_char('i');
        assert_eq!(a.input_buf, "hi");
        assert_eq!(a.cursor_pos, 2);
        a.backspace();
        assert_eq!(a.input_buf, "h");
        assert_eq!(a.cursor_pos, 1);
    }

    #[test]
    fn repl_app_cursor_left_right_clamps() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.insert_char('a');
        a.insert_char('b');
        a.cursor_left();
        a.cursor_left();
        a.cursor_left();
        assert_eq!(a.cursor_pos, 0);
        a.cursor_right();
        a.cursor_right();
        a.cursor_right();
        assert_eq!(a.cursor_pos, 2);
    }

    #[test]
    fn repl_app_take_input_resets_buffer() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.insert_char('h');
        a.insert_char('i');
        let line = a.take_input();
        assert_eq!(line, Some("hi".to_string()));
        assert!(a.input_buf.is_empty());
        assert_eq!(a.cursor_pos, 0);
    }

    #[test]
    fn repl_app_take_input_skips_whitespace_only() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.insert_char(' ');
        a.insert_char('\t');
        assert_eq!(a.take_input(), None);
    }

    #[test]
    fn repl_app_history_prev_next() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.history = vec!["one".into(), "two".into(), "three".into()];
        a.insert_char('x');
        a.history_prev();
        assert_eq!(a.input_buf, "three");
        a.history_prev();
        assert_eq!(a.input_buf, "two");
        a.history_next();
        assert_eq!(a.input_buf, "three");
        a.history_next();
        assert_eq!(a.input_buf, "x"); // restored
    }

    #[test]
    fn trim_surrounding_blank_lines_strips_leading_and_trailing() {
        let input = "\n\n  \nhello\n\nworld\n\n  \n";
        let out = trim_surrounding_blank_lines(input);
        assert_eq!(out, "hello\n\nworld");
    }

    #[test]
    fn trim_surrounding_blank_lines_preserves_when_no_blanks() {
        assert_eq!(trim_surrounding_blank_lines("hi"), "hi");
        assert_eq!(trim_surrounding_blank_lines("a\nb"), "a\nb");
    }

    #[test]
    fn trim_surrounding_blank_lines_empty_input() {
        assert_eq!(trim_surrounding_blank_lines(""), "");
        assert_eq!(trim_surrounding_blank_lines("\n\n  \n"), "");
    }

    #[test]
    fn strip_interior_blank_lines_drops_all_blanks() {
        let input = "para1\n\n\npara2\n\n\n\npara3";
        let out = strip_interior_blank_lines(input);
        assert_eq!(out, "para1\npara2\npara3");
    }

    #[test]
    fn strip_interior_blank_lines_drops_single_blank() {
        let input = "para1\n\npara2";
        assert_eq!(strip_interior_blank_lines(input), "para1\npara2");
    }

    #[test]
    fn strip_interior_blank_lines_treats_whitespace_only_as_blank() {
        let input = "a\n   \n\t\nb";
        assert_eq!(strip_interior_blank_lines(input), "a\nb");
    }

    #[test]
    fn collapse_blank_lines_drops_consecutive_empty_lines() {
        let input: Vec<Line<'static>> = vec![
            Line::from("a"),
            Line::from(""),
            Line::from(""),
            Line::from("b"),
            Line::from("   "),
            Line::from(""),
            Line::from("c"),
        ];
        let out = collapse_blank_lines(input);
        // Expected: a, blank, b, blank, c (two blanks collapsed each time).
        assert_eq!(out.len(), 5);
        assert_eq!(out[0].spans[0].content, "a");
        assert!(out[1].spans.iter().all(|s| s.content.trim().is_empty()));
        assert_eq!(out[2].spans[0].content, "b");
        assert!(out[3].spans.iter().all(|s| s.content.trim().is_empty()));
        assert_eq!(out[4].spans[0].content, "c");
    }

    #[test]
    fn is_md_table_row_basic() {
        assert!(is_md_table_row("| a | b |"));
        assert!(is_md_table_row("  |x|"));
        assert!(!is_md_table_row("hello"));
        assert!(!is_md_table_row(""));
    }

    #[test]
    fn is_md_table_separator_basic() {
        assert!(is_md_table_separator("|---|---|"));
        assert!(is_md_table_separator("| :--- | ---: |"));
        assert!(is_md_table_separator("|:-:|:-:|"));
        assert!(!is_md_table_separator("| a | b |"));
        assert!(!is_md_table_separator("hello"));
        // No dashes → not a separator.
        assert!(!is_md_table_separator("|   |   |"));
    }

    #[test]
    fn parse_md_table_cells_basic() {
        assert_eq!(
            parse_md_table_cells("| a | b |"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            parse_md_table_cells("|x|y|z|"),
            vec!["x".to_string(), "y".to_string(), "z".to_string()]
        );
        assert_eq!(
            parse_md_table_cells("| Technique | Impact | Effort |"),
            vec![
                "Technique".to_string(),
                "Impact".to_string(),
                "Effort".to_string()
            ]
        );
    }

    #[test]
    fn truncate_cell_basic() {
        assert_eq!(truncate_cell("abc", 5), "abc");
        assert_eq!(truncate_cell("abcdef", 4), "abc…");
        assert_eq!(truncate_cell("hello", 0), "");
        assert_eq!(truncate_cell("hello", 5), "hello");
    }

    #[test]
    fn render_markdown_table_emits_expected_lines() {
        let header = vec![
            "Technique".to_string(),
            "Impact".to_string(),
            "Effort".to_string(),
        ];
        let body = vec![
            vec![
                "Prompt constraints".to_string(),
                "10–20%".to_string(),
                "Low".to_string(),
            ],
            vec![
                "max_tokens caps".to_string(),
                "Prevents runaway".to_string(),
                "Low".to_string(),
            ],
        ];
        let out = render_markdown_table(&header, &body, 200, "   ", None);
        // top border + header + separator + 2 body + bottom = 6 lines.
        assert_eq!(out.len(), 6);
        // First line should contain the top-left corner.
        let first: String = out[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            first.contains('┌'),
            "expected ┌ in top border, got: {first}"
        );
        assert!(first.contains('┬'));
        assert!(first.contains('┐'));
        // Header row contains "Technique" cell content.
        let header_line: String = out[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(header_line.contains("Technique"));
        assert!(header_line.contains("Impact"));
        assert!(header_line.contains("Effort"));
        // Separator row uses ├ ┼ ┤.
        let sep_line: String = out[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(sep_line.contains('├'));
        assert!(sep_line.contains('┼'));
        assert!(sep_line.contains('┤'));
        // Bottom border uses └ ┴ ┘.
        let last: String = out[5]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(last.contains('└'));
        assert!(last.contains('┴'));
        assert!(last.contains('┘'));
        // Every line begins with the indent prefix.
        for l in &out {
            let s: String = l
                .spans
                .iter()
                .map(|sp| sp.content.as_ref())
                .collect::<Vec<_>>()
                .join("");
            assert!(s.starts_with("   "), "line missing indent: {s:?}");
        }
    }

    #[test]
    fn render_markdown_table_truncates_when_too_wide() {
        let header = vec!["AVeryLongHeaderName".to_string(), "B".to_string()];
        let body = vec![vec!["X".to_string(), "Y".to_string()]];
        // Tight width forces truncation of the long header.
        let out = render_markdown_table(&header, &body, 18, "", None);
        assert_eq!(out.len(), 5);
        let header_line: String = out[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        // Either truncated with ellipsis OR fits — but width must not exceed limit.
        assert!(
            header_line.chars().count() <= 18,
            "row too wide: {header_line:?}"
        );
    }

    #[test]
    fn push_assistant_trims_surrounding_blanks() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.push_assistant("\n\n2 + 2 = 4.\n\n   No tools needed.\n\n\n", false);
        assert_eq!(a.chat.len(), 1);
        assert_eq!(a.chat[0].text, "2 + 2 = 4.\n   No tools needed.");
    }

    #[test]
    fn repl_app_push_user_keeps_banner() {
        // The banner now lives in the chat scroll buffer (see `banner_lines`)
        // and scrolls off the top naturally as content grows. push_user no
        // longer toggles `show_banner`.
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        assert!(a.show_banner);
        a.push_user("hello");
        assert!(
            a.show_banner,
            "banner should remain visible after first message"
        );
        assert_eq!(a.chat.len(), 1);
        assert_eq!(a.chat[0].role, ChatRole::User);
    }

    #[test]
    fn repl_app_remember_input_dedups() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.remember_input("hello");
        a.remember_input("hello"); // dup
        a.remember_input("world");
        assert_eq!(a.history, vec!["hello", "world"]);
    }

    #[test]
    fn repl_app_scroll_clamps_at_zero() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        // #329: scroll() now also clamps against `last_max_scroll`. Publish a
        // generous cap so this test continues to exercise the floor (0)
        // without colliding with the upper clamp.
        a.last_max_scroll
            .store(100, std::sync::atomic::Ordering::Relaxed);
        a.scroll(5);
        assert_eq!(a.scroll_offset, 0);
        a.scroll(-3);
        assert_eq!(a.scroll_offset, 3);
        a.scroll(10);
        assert_eq!(a.scroll_offset, 0);
    }

    /// Why (#329): Mouse wheel scroll-up used to accumulate `scroll_offset`
    /// indefinitely past the actual scrollback height; subsequent scroll-down
    /// had to "burn off" the phantom offset before any visible movement.
    /// What: After `draw_chat` publishes a max via `last_max_scroll`,
    /// `scroll()` must clamp upward deltas at that cap.
    /// Test: simulate render (publish cap=5), apply -100 → offset should be 5.
    #[test]
    fn repl_app_scroll_clamps_at_max_offset() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        // Simulate the render path publishing a max_offset of 5.
        a.last_max_scroll
            .store(5, std::sync::atomic::Ordering::Relaxed);
        a.scroll(-100);
        assert_eq!(a.scroll_offset, 5, "should clamp to last_max_scroll");
        // Scrolling up further is a no-op.
        a.scroll(-3);
        assert_eq!(a.scroll_offset, 5);
        // Scrolling down decrements normally.
        a.scroll(2);
        assert_eq!(a.scroll_offset, 3);
        // When the cap shrinks (e.g. content removed), an explicit scroll
        // brings the offset back into range.
        a.last_max_scroll
            .store(1, std::sync::atomic::Ordering::Relaxed);
        a.scroll(0);
        assert_eq!(a.scroll_offset, 1);
    }

    /// Why (#331): The claude-mpm session segment must surface a styled
    /// `MPM:` chunk distinct from the `TM:` chunk when any TM session is
    /// running the claude-mpm adapter; suppressed entirely when zero.
    /// What: Build the rich statusline with `claude_mpm_session_count > 0`
    /// and assert the rendered text contains the MPM segment.
    /// Test: flatten spans → string and look for `MPM: 2 sessions`.
    #[test]
    fn rich_statusline_includes_claude_mpm_segment_when_present() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.status_line = Some("✓ LLM: openrouter (sonnet) · All systems go.".into());
        a.tm_session_count = 3;
        a.claude_mpm_session_count = 2;
        let line = build_rich_statusline(&a);
        let text: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains("MPM: 2 sessions"),
            "missing MPM segment: {text}"
        );
        assert!(
            text.contains("TM: 3 sessions"),
            "missing TM segment: {text}"
        );
        // Suppress when zero.
        a.claude_mpm_session_count = 0;
        let line2 = build_rich_statusline(&a);
        let text2: String = line2
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            !text2.contains("MPM:"),
            "MPM segment should be hidden when 0: {text2}"
        );
    }

    // --- AgentScope tests ---

    /// Why: Default scope must be User so ctrl starts with cyan label without explicit init.
    /// What: ReplApp::new yields agent_scope == AgentScope::User.
    /// Test: Construct a fresh ReplApp and assert agent_scope is User.
    #[test]
    fn repl_app_agent_scope_default() {
        let a = ReplApp::new("ctrl".into(), "u".into());
        assert_eq!(a.agent_scope, AgentScope::User);
    }

    /// Why: User scope must map to cyan; project scope must map to yellow.
    /// What: match expression on AgentScope returns the correct Color variant.
    /// Test: Assert both branches of the scope-to-color match.
    #[test]
    fn agent_scope_label_color_user_vs_project() {
        let user_color = match AgentScope::User {
            AgentScope::User => Color::Cyan,
            AgentScope::Project => Color::Yellow,
        };
        let project_color = match AgentScope::Project {
            AgentScope::User => Color::Cyan,
            AgentScope::Project => Color::Yellow,
        };
        assert_eq!(user_color, Color::Cyan);
        assert_eq!(project_color, Color::Yellow);
    }

    /// Why: TokenUpdate must increment cumulative counters so the input bar
    /// reflects live token usage; TokenReset zeros both counters.
    /// What: Mutate `tokens_in`/`tokens_out` directly (mirrors process_event).
    /// Test: Apply two updates and assert sum; then reset and assert zero.
    #[test]
    fn repl_app_token_update_accumulates() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.tokens_in = a.tokens_in.saturating_add(100);
        a.tokens_out = a.tokens_out.saturating_add(50);
        a.tokens_in = a.tokens_in.saturating_add(25);
        a.tokens_out = a.tokens_out.saturating_add(75);
        assert_eq!(a.tokens_in, 125);
        assert_eq!(a.tokens_out, 125);
        a.tokens_in = 0;
        a.tokens_out = 0;
        assert_eq!(a.tokens_in, 0);
        assert_eq!(a.tokens_out, 0);
    }

    /// Why: `/clear` zeroes the running session token counters but must NOT
    /// erase what the user has spent earlier today — daily totals survive
    /// across `/clear` and process restarts.
    /// What: Set `daily_cost_start` to a non-zero value, simulate
    /// TokenReset by zeroing tokens, and assert `daily_cost_start` is intact.
    /// Test: Self-explanatory.
    #[test]
    fn repl_app_token_reset_preserves_daily_cost_start() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.daily_cost_start = 0.0123;
        a.tokens_in = 1000;
        a.tokens_out = 500;
        // Mirrors what TokenReset does in process_event.
        a.tokens_in = 0;
        a.tokens_out = 0;
        assert_eq!(a.tokens_in, 0);
        assert_eq!(a.tokens_out, 0);
        assert!((a.daily_cost_start - 0.0123).abs() < 1e-9);
    }

    /// Why: The first TokenUpdate must flush to disk; later updates within
    /// the throttle window must not. Verifies the throttle window guards the
    /// hot path.
    /// What: Call `persist_daily_usage_if_due` twice in quick succession;
    /// assert the file was written exactly once (mtime stable on second
    /// call). Then advance `last_usage_write` past the window and assert
    /// the next call writes again.
    /// Test: Self-explanatory.
    #[test]
    fn persist_daily_usage_writes_then_throttles() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.usage_project_dir = dir.path().to_path_buf();
        a.tokens_in = 1000;
        a.tokens_out = 1000;

        // First call: no prior write → must persist.
        persist_daily_usage_if_due(&mut a);
        let path = crate::usage::daily::usage_path(dir.path());
        assert!(path.exists(), "first call should create the file");
        let first_stamp = a.last_usage_write.expect("first call records timestamp");

        // Second call immediately: throttled, timestamp unchanged.
        persist_daily_usage_if_due(&mut a);
        assert_eq!(a.last_usage_write.unwrap(), first_stamp);

        // Force the throttle to expire and call again.
        a.last_usage_write = Some(
            std::time::Instant::now() - USAGE_WRITE_INTERVAL - std::time::Duration::from_secs(1),
        );
        persist_daily_usage_if_due(&mut a);
        assert!(a.last_usage_write.unwrap() > first_stamp);
    }

    /// Why: Cost rendering must use 4 decimals below $0.01 and 3 above so the
    /// statusline stays readable across small (sub-cent) and larger costs.
    /// What: Spot-check both branches.
    /// Test: Self-explanatory.
    #[test]
    fn format_cost_value_thresholds() {
        assert_eq!(format_cost_value(0.0001), "$0.0001");
        assert_eq!(format_cost_value(0.5), "$0.500");
    }

    /// Why: Picker navigation must wrap from the last item back to the first
    /// (and vice versa) so the user can reach any item with either arrow.
    /// What: Down on the last index → 0; Up on index 0 → last index.
    /// Test: Construct a 3-item picker, walk the indices.
    #[test]
    fn repl_app_picker_navigation_wraps() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.picker = Some(PickerState {
            items: vec!["a".into(), "b".into(), "c".into()],
            selected: 0,
            title: "T".into(),
            kind: PickerKind::Model,
        });
        // Up from 0 → wraps to last (2).
        let key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        handle_picker_key(&mut a, key);
        assert_eq!(a.picker.as_ref().unwrap().selected, 2);
        // Down from 2 → wraps to 0.
        let key = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        handle_picker_key(&mut a, key);
        assert_eq!(a.picker.as_ref().unwrap().selected, 0);
        // Down 0→1.
        handle_picker_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(a.picker.as_ref().unwrap().selected, 1);
    }

    /// Why: Enter must close the picker and stash the selection so the event
    /// loop can synthesize a `/model …` Submit.
    /// What: Picker becomes None; `pending_picker_selection` gets `(kind, item)`.
    /// Test: Open picker, simulate Enter on selected item, assert state.
    #[test]
    fn repl_app_picker_enter_sets_pending_selection() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.picker = Some(PickerState {
            items: vec![
                "anthropic/claude-haiku-4-5".into(),
                "anthropic/claude-sonnet-4-6".into(),
            ],
            selected: 1,
            title: "Select Model".into(),
            kind: PickerKind::Model,
        });
        handle_picker_key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(a.picker.is_none());
        assert_eq!(
            a.pending_picker_selection,
            Some((PickerKind::Model, "anthropic/claude-sonnet-4-6".into()))
        );
    }

    /// Why: Esc must dismiss the picker without leaving any pending selection
    /// — picker is purely a cancel, no state side-effects.
    /// What: After Esc, both `picker` and `pending_picker_selection` are None.
    /// Test: Open picker, press Esc, assert clean state.
    #[test]
    fn repl_app_picker_esc_dismisses() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.picker = Some(PickerState {
            items: vec!["x".into()],
            selected: 0,
            title: "T".into(),
            kind: PickerKind::Provider,
        });
        handle_picker_key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(a.picker.is_none());
        assert!(a.pending_picker_selection.is_none());
    }

    /// Why bug fix (#switch-popup): `/switch` (no arg) populates the inline
    /// flat-list picker with `choices_context = Some("switch")`. Pressing
    /// Enter on a selection must NOT insert the persona name into the input
    /// buffer (legacy behavior); it must directly queue a synthetic
    /// `/switch <name>` Submit so the persona swap happens in one keypress.
    /// What: Populate choices + context, simulate Down + Enter, assert
    /// `pending_submit == Some("/switch Izzie")` and `input_buf` is empty.
    /// Test: Self-contained.
    #[test]
    fn inline_choices_switch_context_dispatches_submit() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.choices = vec!["ctrl".into(), "Izzie".into(), "CTO Assistant".into()];
        a.choice_cursor = 0;
        a.choices_context = Some("switch".into());
        // Down → cursor on "Izzie".
        handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(a.choice_cursor, 1);
        // Enter → synthetic `/switch Izzie` queued, input untouched.
        handle_key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(a.choices.is_empty(), "choices must clear on selection");
        assert!(
            a.choices_context.is_none(),
            "context must clear on selection"
        );
        assert!(
            a.input_buf.is_empty(),
            "switch context must NOT insert into input buffer"
        );
        assert_eq!(a.pending_submit.as_deref(), Some("/switch Izzie"));
    }

    /// Why: Inline choices WITHOUT a context (the legacy LLM-offered list
    /// path) must keep their original behavior — Enter inserts the
    /// selected text into the input buffer for the user to edit/submit.
    /// What: choices set, no context; Enter places pick into input_buf
    /// and leaves `pending_submit` empty.
    /// Test: Self-contained.
    #[test]
    fn inline_choices_no_context_inserts_into_input() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.choices = vec!["alpha".into(), "beta".into()];
        a.choice_cursor = 1;
        a.choices_context = None;
        handle_key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.input_buf, "beta");
        assert!(a.pending_submit.is_none());
    }

    /// Why: Slash-command autocomplete must filter SLASH_COMMANDS by the
    /// prefix the user has typed so far. Typing `/me` should narrow to
    /// `/memories` (and any other `/me*` commands).
    /// What: Insert chars one by one; assert `app.choices` contains
    /// `/memories` after `/me` is typed and is empty before the leading `/`.
    /// Test: Self-contained.
    #[test]
    fn slash_completions_filters_by_prefix() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        // No `/` yet → no slash autocomplete picker.
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        assert!(a.choices.is_empty());
        // Reset and type `/`.
        a.input_buf.clear();
        a.cursor_pos = 0;
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        // Single `/` matches all commands.
        assert!(!a.choices.is_empty(), "expected matches after `/`");
        assert!(a.choices.iter().any(|c| c == "/memories"));
        assert!(a.choices.iter().any(|c| c == "/help"));
        // Narrow with `m` → both `/memories` and `/model` match.
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
        );
        assert!(a.choices.iter().any(|c| c == "/memories"));
        assert!(a.choices.iter().any(|c| c == "/model"));
        assert!(!a.choices.iter().any(|c| c == "/help"));
        // Narrow with `e` → only `/memories` matches.
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        );
        // `/me` is exact-suppressed only if it's the unique full match — but
        // it's a prefix of `/memories`, so the picker stays with that single
        // entry until the user finishes typing `/memories`.
        assert!(a.choices.iter().any(|c| c == "/memories"));
        assert!(!a.choices.iter().any(|c| c == "/model"));
    }

    /// Why: Once the user types a space after a slash command, the picker
    /// must vanish — they're now typing arguments, not picking a command.
    /// What: Type `/help`, picker may appear or be exact-suppressed; type
    /// space, assert `choices` is empty.
    /// Test: Self-contained.
    #[test]
    fn slash_completions_clears_on_space() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        for c in "/help".chars() {
            handle_key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // Type space → choices must clear.
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        assert!(a.choices.is_empty(), "space must dismiss slash picker");
    }

    /// Why: Tab on an active slash picker must complete the highlighted
    /// command into the input buffer (with trailing space) and dismiss the
    /// picker, so the user can immediately type arguments.
    /// What: Type `/me`, arrow Down to `/memories` (or whichever is at
    /// index 1), Tab; assert `input_buf == "<picked> "` and choices clear.
    /// Test: Self-contained.
    #[test]
    fn slash_completions_tab_completes() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        for c in "/me".chars() {
            handle_key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(!a.choices.is_empty(), "expected slash picker after `/me`");
        let pick = a.choices[a.choice_cursor].clone();
        handle_key(&mut a, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(a.input_buf, format!("{pick} "));
        assert_eq!(a.cursor_pos, a.input_buf.len());
        assert!(a.choices.is_empty(), "Tab must dismiss picker");
    }

    /// Why: When the user has typed an exact full match (e.g. `/help`) the
    /// picker should not pop up showing only the same string they already
    /// typed — it would just be visual noise.
    /// What: Type `/help` (which is the full command); assert choices is
    /// empty (suppressed because exact single-match).
    /// Test: Self-contained.
    #[test]
    fn slash_completions_suppressed_on_exact_match() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        for c in "/help".chars() {
            handle_key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // `/help` is unique — no other command starts with `/help`.
        // Picker should be suppressed.
        assert!(
            a.choices.is_empty(),
            "exact single match must suppress picker, got {:?}",
            a.choices
        );
    }

    /// Why: Backspacing past the leading `/` must clear the slash picker —
    /// once the buffer no longer starts with `/`, autocomplete is irrelevant.
    /// What: Type `/me`, backspace 3 times; assert choices empty after the
    /// final backspace removes the `/`.
    /// Test: Self-contained.
    #[test]
    fn slash_completions_clears_on_backspace_past_slash() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        for c in "/me".chars() {
            handle_key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(!a.choices.is_empty());
        for _ in 0..3 {
            handle_key(
                &mut a,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            );
        }
        assert_eq!(a.input_buf, "");
        assert!(a.choices.is_empty(), "removing `/` must clear picker");
    }

    /// Why: Inline-picker context (e.g. `/switch` persona list) must NOT be
    /// stomped by the slash-completion helper — those choices have their
    /// own lifecycle and Enter-action.
    /// What: Set `choices_context = Some("switch")` with persona names;
    /// type a `/` char (which would normally trigger slash autocomplete);
    /// assert choices and context survive untouched.
    /// Test: Self-contained.
    #[test]
    fn slash_completions_does_not_stomp_context_picker() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.choices = vec!["ctrl".into(), "Izzie".into()];
        a.choices_context = Some("switch".into());
        a.choice_cursor = 0;
        // Send a Char that would normally trigger autocomplete update.
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(a.choices, vec!["ctrl".to_string(), "Izzie".to_string()]);
        assert_eq!(a.choices_context.as_deref(), Some("switch"));
    }

    /// Why: When the picker is open, ALL keys must be intercepted — typing a
    /// regular character must NOT leak through to the input editor.
    /// What: With picker open, sending KeyCode::Char('x') leaves input_buf
    /// empty. With picker closed, the same key inserts into the buffer.
    /// Test: Two parallel cases.
    #[test]
    fn handle_key_modal_gates_input_editor() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.picker = Some(PickerState {
            items: vec!["x".into()],
            selected: 0,
            title: "T".into(),
            kind: PickerKind::Model,
        });
        let r = handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(r, None);
        assert!(a.input_buf.is_empty(), "modal must swallow chars");

        // Close picker → same key inserts.
        a.picker = None;
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(a.input_buf, "x");
    }

    /// Why: The rich statusline must include the `[open-mpm]` prefix and
    /// preserve every piece of the underlying status string so users can read
    /// LLM, counts, and the help hint at a glance.
    /// What: Build a status_line, render via `build_rich_statusline`, flatten
    /// span text, and assert the well-known substrings appear in order.
    /// Test: Asserts prefix, ✓ tick, "LLM:", model name, and
    /// "All systems go" show up. The Tools/Skills/MCP counts and `/help`
    /// hint were intentionally dropped in #293.
    #[test]
    fn rich_statusline_renders_brackets_and_body() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.status_line = Some("✓ LLM: openrouter:claude-haiku-4-5 · All systems go.".to_string());
        let line = build_rich_statusline(&a);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("[open-mpm] "), "missing prefix: {text}");
        assert!(text.contains("✓ "), "missing tick: {text}");
        assert!(text.contains("LLM: "), "missing LLM label: {text}");
        assert!(
            text.contains("openrouter:claude-haiku-4-5"),
            "missing model: {text}"
        );
        assert!(
            !text.contains("anthropic/"),
            "vendor prefix should be stripped: {text}"
        );
        assert!(!text.contains('('), "parens should be removed: {text}");
        assert!(
            text.contains("All systems go."),
            "missing OK marker: {text}"
        );
        // Removed in #293:
        assert!(
            !text.contains("Tools: "),
            "Tools count should be removed: {text}"
        );
        assert!(
            !text.contains("Skills: "),
            "Skills count should be removed: {text}"
        );
        assert!(
            !text.contains("MCP: "),
            "MCP count should be removed: {text}"
        );
        assert!(
            !text.contains("/help"),
            "help hint should be removed: {text}"
        );
    }

    /// Why: With tokens accumulated, the statusline must inject
    /// `↑prompt ↓completion · $cost` before `All systems go.` (#293).
    /// What: Set tokens_in/tokens_out, render rich statusline, assert
    /// substrings appear.
    /// Test: prompt 1500, completion 800 → `↑1.5k ↓0.8k` and a `$` chunk.
    #[test]
    fn rich_statusline_includes_tokens_and_cost_when_present() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.status_line = Some("✓ LLM: openrouter:claude-haiku-4-5 · All systems go.".to_string());
        a.tokens_in = 1500;
        a.tokens_out = 1200;
        let line = build_rich_statusline(&a);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("↑1.5k ↓1.2k"), "missing token chunk: {text}");
        assert!(text.contains('$'), "missing cost chunk: {text}");
    }

    /// Why: When no tokens have been used yet, the token + cost segments
    /// must be omitted (don't show `↑0 ↓0 · $0.0000`) — #293.
    /// What: Fresh app with status_line set but tokens at 0, render and
    /// confirm the arrow/dollar glyphs are absent.
    /// Test: assert! NOT contains.
    #[test]
    fn rich_statusline_omits_tokens_when_zero() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.status_line = Some("✓ LLM: openrouter:claude-haiku-4-5 · All systems go.".to_string());
        let line = build_rich_statusline(&a);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains('↑'), "token arrow should be absent: {text}");
        assert!(!text.contains('$'), "cost glyph should be absent: {text}");
    }

    /// Why: `format_tokens` is the shared compactor for both the spinner and
    /// the statusline; small numbers stay raw, ≥1000 collapses to `Nk` form.
    /// What: Spot-check key boundaries and fractional rounding.
    /// Test: 0, 999, 1000, 1234, 2000, 12345.
    #[test]
    fn format_tokens_compact() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1000), "1k");
        assert_eq!(format_tokens(1234), "1.2k");
        assert_eq!(format_tokens(2000), "2k");
        assert_eq!(format_tokens(12345), "12.3k");
    }

    /// Why: `format_elapsed` powers the Claude Code-style `(2m 18s · …)`
    /// spinner timer; verify the three buckets.
    /// What: 5s → "5s", 78s → "1m 18s", 3700s → "1h 1m".
    /// Test: deterministic mapping.
    #[test]
    fn format_elapsed_buckets() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(5), "5s");
        assert_eq!(format_elapsed(59), "59s");
        assert_eq!(format_elapsed(60), "1m 0s");
        assert_eq!(format_elapsed(78), "1m 18s");
        assert_eq!(format_elapsed(3700), "1h 1m");
    }

    /// Why: `status_word_for` cycles through thinking/working/processing on
    /// elapsed-time buckets — verify boundaries.
    /// What: 0/9/10/29/30s thresholds.
    /// Test: deterministic.
    #[test]
    fn status_word_buckets() {
        assert_eq!(status_word_for(0), "thinking");
        assert_eq!(status_word_for(9), "thinking");
        assert_eq!(status_word_for(10), "working");
        assert_eq!(status_word_for(29), "working");
        assert_eq!(status_word_for(30), "processing");
        assert_eq!(status_word_for(600), "processing");
    }

    /// Why: `format_token_chunk` is the statusline arrow form — must use
    /// the compact `k` form for both directions.
    /// What: ↑1.2k ↓0.8k for 1234/800.
    /// Test: deterministic.
    #[test]
    fn format_token_chunk_compacts_thousands() {
        assert_eq!(format_token_chunk(1234, 800), "↑1.2k ↓800");
        assert_eq!(format_token_chunk(0, 0), "↑0 ↓0");
    }

    /// Why: Cost format precision flips at $0.01 — 4 decimals below, 3 at/above.
    /// What: Tiny costs render with 4 decimals, larger ones with 3.
    /// Test: deterministic boundary check.
    #[test]
    fn format_cost_chunk_thresholds() {
        // 1000 prompt + 1000 completion @ haiku rates =
        //   1000 * 0.00000025 + 1000 * 0.00000125 = 0.0015
        let s = format_cost_chunk(1000, 1000);
        assert_eq!(s, "$0.0015");
        // 100k prompt + 100k completion = 0.025 + 0.125 = 0.150 → 3 decimals.
        let s = format_cost_chunk(100_000, 100_000);
        assert_eq!(s, "$0.150");
    }

    /// Why: Activity row 1 must include elapsed time and the cycling status
    /// word, with `✻` glyph and `Processing…` label (Claude Code style).
    /// What: Build with elapsed=5 and zero tokens; assert glyph + word.
    /// Test: substring presence.
    #[test]
    fn activity_row1_includes_elapsed_and_status() {
        let a = ReplApp::new("ctrl".into(), "u".into());
        let line = build_activity_row1(&a, 5, 120);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        // Spinner is animated — assert the leading glyph is one of the
        // braille frames rather than a hardcoded character.
        assert!(
            SPINNER_FRAMES.iter().any(|f| text.starts_with(f)),
            "spinner glyph not in animation set: {text}"
        );
        assert!(text.contains("Processing…"), "missing label: {text}");
        assert!(text.contains("5s"), "missing elapsed: {text}");
        assert!(text.contains("thinking"), "missing status word: {text}");
        // Token segment should be omitted with zero tokens.
        assert!(!text.contains('↓'), "token segment leaked at zero: {text}");
    }

    /// Why: The activity spinner must visibly cycle frame-to-frame so users
    /// can tell the LLM is actively working. `tick_count` drives the index;
    /// bumping it must change the leading glyph.
    /// What: Render row1 at tick=0 and tick=1, assert the glyph differs.
    /// Test: Direct comparison of leading character.
    #[test]
    fn activity_row1_spinner_animates_with_tick() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.tick_count = 0;
        let line0 = build_activity_row1(&a, 5, 120);
        let glyph0 = line0.spans[0].content.to_string();

        a.tick_count = 1;
        let line1 = build_activity_row1(&a, 5, 120);
        let glyph1 = line1.spans[0].content.to_string();

        assert_ne!(glyph0, glyph1, "spinner did not advance with tick_count");
        assert!(
            SPINNER_FRAMES.iter().any(|f| glyph0.starts_with(f)),
            "frame 0 not in animation set: {glyph0}"
        );
        assert!(
            SPINNER_FRAMES.iter().any(|f| glyph1.starts_with(f)),
            "frame 1 not in animation set: {glyph1}"
        );
    }

    /// Why: The rust-rainbow shimmer must flow across the spinner line as
    /// `rainbow_tick` advances. Same character at different ticks must get
    /// different colors, otherwise the effect is static.
    /// What: Build rainbow_spans for "abc" at tick=0 and tick=1; assert that
    /// the color of index 0 changes between the two ticks.
    /// Test: Direct color comparison.
    #[test]
    fn rainbow_spans_advances_with_tick() {
        let s0 = rainbow_spans("abc", 0);
        let s1 = rainbow_spans("abc", 1);
        // Each tick shifts the gradient hue, so the same character index
        // must get a different color between consecutive ticks.
        assert_ne!(s0[0].style.fg, s1[0].style.fg, "rainbow did not flow");
        assert_ne!(s0[1].style.fg, s1[1].style.fg, "rainbow did not flow");
    }

    /// Why: `hsl_to_rgb` is the foundation of the smooth gradient; if the
    /// conversion is wrong the entire shimmer is wrong. Spot-check the three
    /// canonical edge cases that pin the algorithm.
    /// What: white (l=1), black (l=0), pure red (h=0,s=1,l=0.5).
    /// Test: Direct equality assertions.
    #[test]
    fn hsl_to_rgb_edge_cases() {
        assert_eq!(hsl_to_rgb(0.0, 0.0, 1.0), (255, 255, 255));
        assert_eq!(hsl_to_rgb(0.0, 0.0, 0.0), (0, 0, 0));
        assert_eq!(hsl_to_rgb(0.0, 1.0, 0.5), (255, 0, 0));
    }

    /// Why: `rainbow_spans` must emit one span per character so each glyph
    /// can carry its own color — collapsing multiple chars into one span
    /// would defeat the per-character flow.
    /// What: Build for a 5-char string; assert 5 spans.
    /// Test: Length check.
    #[test]
    fn rainbow_spans_one_span_per_char() {
        let spans = rainbow_spans("hello", 0);
        assert_eq!(spans.len(), 5);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "hello");
    }

    /// Why: When tokens have been streamed, row 1 should add the `↓ Nk
    /// tokens` segment between elapsed and status word.
    /// What: Set tokens_out and assert segment appears.
    /// Test: substring presence.
    #[test]
    fn activity_row1_omits_tokens_when_zero() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.tokens_out = 2900;
        let line = build_activity_row1(&a, 138, 200);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("↓ 2.9k tokens"),
            "missing token segment: {text}"
        );
        assert!(text.contains("2m 18s"), "missing elapsed: {text}");
    }

    /// Why: When `status_line` is unset (defensive path) the rich statusline
    /// must still render the `[open-mpm]` prefix so the row never goes blank.
    /// What: Fresh app with no status_line → prefix appears, body is the
    /// legacy segment string ("User" by default).
    /// Test: Assert both substrings.
    #[test]
    fn rich_statusline_fallback_when_status_line_missing() {
        let a = ReplApp::new("ctrl".into(), "u".into());
        let line = build_rich_statusline(&a);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("[open-mpm] "), "missing prefix: {text}");
        assert!(text.contains("User"), "missing fallback segment: {text}");
    }

    /// Why: `style_status_chunk` is the per-chunk styler; each known prefix
    /// must produce the right span composition (count of spans + text).
    /// What: Exercise each branch (LLM, count, success, unknown).
    /// Test: Assert flattened text matches the input for each chunk.
    #[test]
    fn rich_statusline_chunks_styled() {
        let llm = style_status_chunk("✓ LLM: openrouter (m)");
        let llm_text: String = llm.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(llm_text, "✓ LLM: openrouter (m)");

        let ok = style_status_chunk("All systems go.");
        let ok_text: String = ok.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(ok_text, "All systems go.");

        // #293: token + cost chunks pass through normally.
        let tok = style_status_chunk("↑1.2k ↓0.8k");
        let tok_text: String = tok.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(tok_text, "↑1.2k ↓0.8k");

        let cost = style_status_chunk("$0.0034");
        let cost_text: String = cost.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(cost_text, "$0.0034");

        let unknown = style_status_chunk("anything else");
        let unknown_text: String = unknown.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(unknown_text, "anything else");
    }

    /// Why: `strip_vendor_prefix` powers the activity-row and statusline
    /// model-display compaction. It must drop everything up to and including
    /// the first `/`, leaving bare ids alone.
    /// What: `anthropic/claude-haiku-4-5` → `claude-haiku-4-5`,
    /// `openai/gpt-4o` → `gpt-4o`, `claude-haiku-4-5` → unchanged.
    /// Test: Three cases.
    #[test]
    fn strip_vendor_prefix_strips_first_segment() {
        assert_eq!(
            strip_vendor_prefix("anthropic/claude-haiku-4-5"),
            "claude-haiku-4-5"
        );
        assert_eq!(strip_vendor_prefix("openai/gpt-4o"), "gpt-4o");
        assert_eq!(strip_vendor_prefix("claude-haiku-4-5"), "claude-haiku-4-5");
    }

    /// Why: Row 3 of the activity panel must NOT echo the cycling status
    /// word from row 1. `is_redundant_thinking_step` collapses any
    /// "thinking"/"working"/"processing" string (with or without trailing
    /// dots/ellipses) to true so callers can blank the row.
    /// What: Empty, "thinking", "Thinking…", "WORKING.", and "processing"
    /// all redundant; meaningful step text ("reading file") is kept.
    /// Test: Five redundant + one kept.
    #[test]
    fn is_redundant_thinking_step_collapses_status_words() {
        assert!(is_redundant_thinking_step(""));
        assert!(is_redundant_thinking_step("thinking"));
        assert!(is_redundant_thinking_step("Thinking…"));
        assert!(is_redundant_thinking_step("WORKING."));
        assert!(is_redundant_thinking_step("processing"));
        assert!(!is_redundant_thinking_step("reading file"));
    }

    /// Why: Up-arrow should restore `last_prompt` into the input buffer when
    /// idle, so users can recall and edit/resubmit the most recent prompt
    /// without re-typing.
    /// What: With `last_prompt` set and `thinking == false`, KeyCode::Up
    /// copies `last_prompt` into `input_buf` and does NOT set `pending_cancel`.
    /// Test: Set last_prompt, send Up, assert input_buf matches and cancel
    /// was NOT signaled.
    #[test]
    fn repl_app_up_arrow_recalls_last_prompt() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.last_prompt = "hello world".to_string();
        a.thinking = false;
        let r = handle_key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(r, None);
        assert_eq!(a.input_buf, "hello world");
        assert_eq!(a.cursor_pos, "hello world".len());
        assert!(!a.pending_cancel, "idle Up must NOT signal cancel");
    }

    /// Why: Up-arrow while the LLM is busy must signal cancellation (so the
    /// event loop can abort the JoinHandle) AND restore the last prompt for
    /// editing.
    /// What: With `thinking == true`, KeyCode::Up sets `pending_cancel` AND
    /// copies `last_prompt` into `input_buf`.
    /// Test: Set thinking + last_prompt, send Up, assert both effects.
    #[test]
    fn repl_app_up_arrow_when_busy_signals_cancel() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.last_prompt = "long task".to_string();
        a.thinking = true;
        let r = handle_key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(r, None);
        assert!(a.pending_cancel, "busy Up must signal cancel");
        assert_eq!(a.input_buf, "long task", "must restore last_prompt");
    }

    /// Why: With no prior submission, Up-arrow has nothing to recall — it
    /// must be a no-op rather than overwriting whatever the user has typed.
    /// What: Empty `last_prompt`, non-empty input_buf → input_buf unchanged.
    /// Test: Type chars, send Up with empty last_prompt, assert input_buf
    /// preserved.
    #[test]
    fn repl_app_up_arrow_noop_when_no_last_prompt() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.insert_char('a');
        a.insert_char('b');
        a.last_prompt.clear();
        a.thinking = false;
        handle_key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(a.input_buf, "ab");
        assert!(!a.pending_cancel);
    }

    /// Why: `parse_token_count_from_step` should pull explicit `↓ N tokens`
    /// values out of activity-step text so the input-row counter snaps to the
    /// real number when an upstream emitter provides it (#298).
    /// What: Verify k-suffix, plain digits, decimal-k, and the no-match cases.
    /// Test: Pure function, table-driven assertions.
    #[test]
    fn parse_token_count_from_step_handles_common_shapes() {
        assert_eq!(parse_token_count_from_step("↓ 2.4k tokens"), Some(2400));
        assert_eq!(parse_token_count_from_step("↓2k tokens"), Some(2000));
        assert_eq!(parse_token_count_from_step("↓ 512 tokens"), Some(512));
        assert_eq!(
            parse_token_count_from_step("foo ↓ 100 tokens bar"),
            Some(100)
        );
        assert_eq!(parse_token_count_from_step("processing..."), None);
        assert_eq!(parse_token_count_from_step("↓ 5 lines"), None);
    }

    /// Why: `busy_since` is the activity panel's source of truth for the
    /// elapsed timer + spinner cycling — it must default to None on a fresh
    /// app so the chat fills the screen when idle.
    /// What: Construct a fresh ReplApp, assert busy_since == None.
    /// Test: Mechanical assertion.
    #[test]
    fn repl_app_busy_since_default_none() {
        let a = ReplApp::new("ctrl".into(), "u".into());
        assert!(a.busy_since.is_none());
        assert!(a.streaming_preview.is_empty());
    }

    /// Why: When LlmResponse arrives, the activity panel must collapse —
    /// busy_since must clear so the layout swaps back to the idle chat-fills
    /// layout, and streaming_preview must clear so it doesn't ghost.
    /// What: Set busy_since + preview, then mirror the LlmResponse mutations
    /// directly (mirrors process_event arm body), assert both are cleared.
    /// Test: Direct state mutation; full event flow tested indirectly via tmux.
    #[test]
    fn repl_app_busy_since_and_preview_cleared_on_response() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.busy_since = Some(std::time::Instant::now());
        a.streaming_preview = "in flight".into();
        a.thinking = true;

        // Simulate LlmResponse handler.
        a.push_assistant("done", false);
        a.thinking = false;
        a.thinking_lines.clear();
        a.busy_since = None;
        a.streaming_preview.clear();

        assert!(a.busy_since.is_none());
        assert!(a.streaming_preview.is_empty());
        assert!(!a.thinking);
    }

    /// Why: `truncate_to` is the width-clamp helper used by the activity
    /// panel's model + step rows — it must respect char boundaries (not byte)
    /// so multi-byte glyphs don't get sliced.
    /// What: Truncate a unicode-laden string to a small width and assert the
    /// char count matches and no panic occurs.
    /// Test: Pass-through case + truncation case.
    #[test]
    fn truncate_to_respects_char_boundaries() {
        assert_eq!(truncate_to("hello".to_string(), 10), "hello");
        assert_eq!(truncate_to("hello world".to_string(), 5), "hello");
        // Multi-byte: '⠋' is 3 bytes but 1 char.
        let s = "⠋⠙⠹⠸⠼".to_string();
        assert_eq!(truncate_to(s, 3).chars().count(), 3);
    }

    /// Why: AgentScopeChanged must update agent_scope so the next render picks
    /// up the new color without any additional state wiring.
    /// What: Simulate process_event logic by directly mutating app.agent_scope
    ///   (mirrors the handler body) and assert the new value.
    /// Test: Start as User, set to Project, assert Project; then back to User.
    #[tokio::test]
    async fn repl_app_agent_scope_changed_event_updates_state() {
        let app = std::sync::Arc::new(tokio::sync::Mutex::new(ReplApp::new(
            "ctrl".into(),
            "u".into(),
        )));

        // Simulate handling AgentScopeChanged(Project).
        {
            let mut a = app.lock().await;
            a.agent_scope = AgentScope::Project;
        }
        assert_eq!(app.lock().await.agent_scope, AgentScope::Project);

        // Simulate handling AgentScopeChanged(User) (e.g. disconnect).
        {
            let mut a = app.lock().await;
            a.agent_scope = AgentScope::User;
        }
        assert_eq!(app.lock().await.agent_scope, AgentScope::User);
    }

    // === Fenced code-block tests (#321) ============================

    #[test]
    fn code_fence_lang_recognizes_openers_and_closers() {
        assert_eq!(code_fence_lang("```bash"), Some("bash".into()));
        assert_eq!(code_fence_lang("```sh"), Some("sh".into()));
        assert_eq!(code_fence_lang("```Rust"), Some("rust".into()));
        assert_eq!(code_fence_lang("```"), Some("".into()));
        assert_eq!(code_fence_lang("  ```bash  "), Some("bash".into()));
        assert_eq!(code_fence_lang("hello"), None);
        assert_eq!(code_fence_lang("``"), None);
    }

    #[test]
    fn is_executable_shell_lang_matches_shells() {
        assert!(is_executable_shell_lang("bash"));
        assert!(is_executable_shell_lang("sh"));
        assert!(is_executable_shell_lang("zsh"));
        assert!(is_executable_shell_lang("fish"));
        assert!(!is_executable_shell_lang("rust"));
        assert!(!is_executable_shell_lang("python"));
        assert!(!is_executable_shell_lang(""));
    }

    #[test]
    fn extract_last_shell_block_finds_bash() {
        let text = "Run this:\n```bash\necho hello\nls -la\n```\nDone.";
        assert_eq!(
            extract_last_shell_block(text),
            Some("echo hello\nls -la".into())
        );
    }

    #[test]
    fn extract_last_shell_block_returns_last_when_multiple() {
        let text = "```bash\nfirst\n```\nstuff\n```sh\nsecond\nthird\n```\n";
        assert_eq!(extract_last_shell_block(text), Some("second\nthird".into()));
    }

    #[test]
    fn extract_last_shell_block_ignores_non_shell() {
        let text = "```rust\nfn main() {}\n```";
        assert_eq!(extract_last_shell_block(text), None);
    }

    #[test]
    fn extract_last_shell_block_none_without_block() {
        assert_eq!(extract_last_shell_block("plain prose only"), None);
    }

    #[test]
    fn repl_app_last_bash_block_updates_on_push() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        assert_eq!(a.last_bash_block, None);
        a.push_assistant("Try `ls`:\n```bash\nls -la\n```", false);
        assert_eq!(a.last_bash_block, Some("ls -la".into()));
        // A non-shell block does not overwrite, but a fresh shell one does.
        a.push_assistant("Here:\n```sh\npwd\n```", false);
        assert_eq!(a.last_bash_block, Some("pwd".into()));
    }

    #[test]
    fn repl_app_last_bash_block_skips_errors() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.push_assistant("```bash\nls\n```", false);
        a.push_assistant("```bash\nrm -rf /\n```", true); // error entry
        // Error entries are not used as the source of truth.
        assert_eq!(a.last_bash_block, Some("ls".into()));
    }

    #[test]
    fn ctrl_e_pastes_last_bash_block_when_input_empty() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.push_assistant("```bash\necho hi\n```", false);
        let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        let _ = handle_key(&mut a, key);
        assert_eq!(a.input_buf, "echo hi");
        assert_eq!(a.cursor_pos, "echo hi".len());
    }

    #[test]
    fn ctrl_e_falls_back_to_end_of_line_when_input_nonempty() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.push_assistant("```bash\necho hi\n```", false);
        a.set_input("typed text".into());
        a.cursor_pos = 0;
        let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        let _ = handle_key(&mut a, key);
        // Input unchanged; cursor moved to end (readline End-of-line).
        assert_eq!(a.input_buf, "typed text");
        assert_eq!(a.cursor_pos, "typed text".len());
    }

    #[test]
    fn ctrl_e_no_op_when_no_block_and_input_empty() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        let _ = handle_key(&mut a, key);
        assert_eq!(a.input_buf, "");
        assert_eq!(a.cursor_pos, 0);
    }

    // #326 Tests 3 & 4: extract_last_shell_block edge cases for unclosed
    // fences. These cover truncation-at-max-tokens scenarios and malformed
    // assistant output where a fence opens but never closes.

    #[test]
    fn extract_last_shell_block_unclosed_fence_returns_none() {
        // Simulates truncation at max_tokens — fence opened, never closed.
        // Without a closing fence we cannot know the block's intended end,
        // so we must return None rather than silently inferring EOF.
        let text = "Here's a script:\n```bash\necho hello\nls -la";
        assert_eq!(extract_last_shell_block(text), None);
    }

    #[test]
    fn extract_last_shell_block_closed_then_unclosed_returns_closed() {
        // Complete block, then an unclosed one — should return the
        // completed block's content (the unclosed trailing block is ignored
        // for the same safety reason as the test above).
        let text = "```bash\necho first\n```\n```sh\nunclosed";
        assert_eq!(
            extract_last_shell_block(text),
            Some("echo first".to_string())
        );
    }

    // #326 Test 5: Ctrl+E must be a no-op when the only fenced block is a
    // non-shell language (e.g. python). `last_bash_block` should remain None
    // and the input/cursor must not change.
    #[test]
    fn ctrl_e_noop_when_python_block_but_no_shell_block() {
        let mut a = ReplApp::new("ctrl".into(), "u".into());
        a.push_assistant("Result:\n```python\nprint('hi')\n```", false);
        // Python block → last_bash_block should be None.
        assert_eq!(a.last_bash_block, None);
        // Ctrl+E with empty input and no bash block: must not change input
        // or cursor.
        let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        let _ = handle_key(&mut a, key);
        assert_eq!(a.input_buf, "");
        assert_eq!(a.cursor_pos, 0);
    }
}
