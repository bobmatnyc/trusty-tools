//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

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
    /// `[trusty-agents] …` informational status line — green `[trusty-agents]` prefix.
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
    /// Statusline segment configuration loaded from `.trusty-agents/config.toml`.
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
    /// the `tx` channel or the `TrustyAgentsRepl` handler. Stashing the choice
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
    /// startup from `.trusty-agents/state/usage.json`. The session's own running
    /// cost is computed from `tokens_in`/`tokens_out`; daily total = this
    /// plus session cost.
    ///
    /// Why: Lets the statusline show "today" alongside "session" without
    /// double-counting the in-flight session.
    /// What: 0.0 on a fresh day; carried forward from disk otherwise.
    /// Survives `/clear` (which only zeroes session tokens).
    /// Test: `repl_app_token_reset_preserves_daily_cost_start`.
    pub daily_cost_start: f64,
    /// Project dir whose `.trusty-agents/state/usage.json` we read/write.
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
    /// Why: `/model` and `/provider` mutate routing state inside `TrustyAgentsRepl`;
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
