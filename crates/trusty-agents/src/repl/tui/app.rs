//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

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

    /// Append a `[trusty-agents]`-style status line.
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
