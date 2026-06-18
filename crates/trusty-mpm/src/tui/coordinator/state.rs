//! Coordinator TUI application state (Child #1 skeleton).
//!
//! Why: the render loop and the key dispatcher both read and mutate one piece
//! of state — the input buffer, an input-history ring, the placeholder session
//! rows, and which row is selected. Holding it in a single struct with pure,
//! unit-testable mutators keeps the terminal glue thin and the selection /
//! history logic verifiable without a terminal (DOC-13 §9).
//! What: [`CoordinatorState`] plus its placeholder [`SessionEntry`] rows, the
//! input-history ring, and [`CoordinatorState::clamp_selection`] /
//! selection-movement helpers that mirror `DashboardState::clamp_selection`.
//! Test: `super::tests` covers `clamp_selection` boundary cases (empty list,
//! single row, selection past end) and the history ring.

use super::nav::ListNav;

/// Maximum number of submitted inputs retained in the history ring.
///
/// Why: the input box recalls prior submissions with ↑/↓ (like the dashboard's
/// `CommandBar`); bounding the ring keeps memory flat over a long session.
/// What: the cap used by [`CoordinatorState::submit_input`].
/// Test: `history_ring_is_bounded`.
pub const INPUT_HISTORY_LIMIT: usize = 50;

/// One placeholder session row shown beneath the controller bullet.
///
/// Why: Child #1 renders static rows so the layout, selection, and two-column
/// active row can be exercised before the live feed lands (Child #2). Modelling
/// the row now — id, routing prefix, status, placeholder summary — means the
/// live feed later fills the *same* shape rather than reshaping the UI.
/// What: a managed session's short id, its `@prefix:` routing prefix, a
/// one-word status, and a placeholder summary for the active-row right column.
/// Test: rows are consumed by the row-builders, themselves unit-tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// Short (8-hex) session id shown in the list's left column.
    pub short_id: String,
    /// Routing prefix (`aipowerranking`) shown after the id.
    pub prefix: String,
    /// One-word status word (`Active`, `Idle`, …) shown dimmed.
    pub status: String,
    /// Placeholder "last summarized message" for the active-row right column.
    ///
    /// Replaced by the daemon `last_summary` in Child #3.
    pub summary: String,
}

impl SessionEntry {
    /// Construct a placeholder session entry.
    ///
    /// Why: the skeleton seeds a couple of illustrative rows; a constructor
    /// keeps that seeding terse and the field order explicit.
    /// What: moves the four owned strings into a [`SessionEntry`].
    /// Test: indirectly via `default_state_has_placeholder_rows`.
    pub fn new(
        short_id: impl Into<String>,
        prefix: impl Into<String>,
        status: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            short_id: short_id.into(),
            prefix: prefix.into(),
            status: status.into(),
            summary: summary.into(),
        }
    }
}

/// Everything the coordinator TUI renders and mutates this frame (skeleton).
///
/// Why: a single owned struct (like the dashboard's `DashboardState`) keeps the
/// data/render split clean — the event loop mutates it, the layout reads it.
/// What: the input buffer + history ring, the placeholder session rows, the
/// selected row index (`0` = controller), and an `should_exit` flag the loop
/// polls to leave on `q` / Ctrl-C.
/// Test: `super::tests` covers selection clamp, movement, and history.
#[derive(Debug, Clone, Default)]
pub struct CoordinatorState {
    /// The current input-box edit buffer.
    pub input: String,
    /// Submitted inputs, newest last, bounded by [`INPUT_HISTORY_LIMIT`].
    pub history: Vec<String>,
    /// Cursor into [`Self::history`] while recalling with ↑/↓; `None` = live.
    history_cursor: Option<usize>,
    /// Placeholder managed-session rows (Child #2 swaps in the live feed).
    pub sessions: Vec<SessionEntry>,
    /// Numbered-list navigation: the selected row index AND the scroll offset,
    /// preserved across the timer re-polls (STUI-1).
    ///
    /// Why: STUI-1 (#1278) replaces the old bare `selected: usize` with a
    /// [`ListState`](ratatui::widgets::ListState)-backed model so the list scrolls
    /// (≥ 5 visible rows), supports Page-Up/Page-Down, and — critically —
    /// preserves the selection + scroll offset across the timer re-polls Child #2
    /// introduced. Held as a [`ListNav`] so the render path can hand the wrapped
    /// `ListState` straight to `render_stateful_widget`.
    /// What: row 0 is the controller; sessions follow. Movement and clamping go
    /// through [`Self::select_up`] / [`Self::select_down`] / [`Self::page_up`] /
    /// [`Self::page_down`] / [`Self::clamp_selection`], which delegate to `nav`.
    /// Test: `nav_*` and `clamp_selection_*` in `super::tests`.
    pub nav: ListNav,
    /// Set when a list-mutating action (create/kill/stop/resume) just succeeded
    /// and the operator should see the fleet refreshed without waiting for the
    /// next timer tick (STUI-1 AC: "immediate re-poll after a mutation").
    ///
    /// Why: a poll only happens on the `--interval-ms` cadence; after the
    /// operator changes the fleet they expect the list to reflect it now, not up
    /// to a whole interval later. The run loop reads this flag each tick and, when
    /// set, re-polls immediately and resets its interval timer. The flag is the
    /// seam the (still-stubbed) slash-command dispatch will pull once #1278's
    /// sibling STUI-6 wires mutations.
    /// What: set by [`Self::request_repoll`]; consumed (read-and-cleared) by
    /// [`Self::take_repoll`].
    /// Test: `request_repoll_sets_and_take_clears`.
    needs_repoll: bool,
    /// Whether the daemon answered its last health probe.
    ///
    /// Why: Child #2 polls the coordinator-context endpoint on a timer and must
    /// surface daemon-down to the operator (a `daemon ✗` status indicator) and
    /// clear the session list rather than showing stale rows. Mirrors
    /// `DashboardState::daemon_reachable`. Defaults to `false` so the TUI reads
    /// "unreachable" until the priming poll confirms otherwise.
    /// What: set by [`super::poll::coord_poll_daemon`] after each health probe.
    /// Test: `poll_marks_unreachable_clears_sessions` in `super::tests` covers the
    /// daemon-down flip to `false`; the daemon-up flip needs a live daemon and is
    /// exercised manually.
    pub daemon_reachable: bool,
    /// Whether the daemon reports the deployed catalog content is stale (HR-3).
    ///
    /// Why: DOC-17 §HR-3 requires the operator surface to show that an update is
    /// available so they can rebuild. The coordinator poll reads the additive
    /// `catalog_stale` field off `GET /health` and the status bar renders a small
    /// `catalog: ↑updates available` hint when it is set. Defaults to `false`
    /// (no update) until the first health poll reports otherwise.
    /// What: set by [`super::poll::coord_poll_daemon`] from the health probe.
    /// Test: `poll_marks_unreachable_clears_sessions` confirms it stays `false`
    /// on a dead daemon; the rendered hint is covered by
    /// `catalog_indicator_reflects_staleness` in `super::tests`.
    pub catalog_stale: bool,
    /// Set when the operator asks to quit; the event loop exits on the next tick.
    pub should_exit: bool,
}

impl CoordinatorState {
    /// Build the skeleton state with a couple of illustrative placeholder rows.
    ///
    /// Why: Child #1 has no daemon feed, but an empty list would make the
    /// layout, selection movement, and two-column active row impossible to see
    /// or test. Seeding static rows exercises the full UI path; Child #2
    /// replaces them with `poll_daemon` output.
    /// What: returns a [`CoordinatorState`] with two placeholder sessions and
    /// the controller selected (row 0).
    /// Test: `default_state_has_placeholder_rows`.
    pub fn skeleton() -> Self {
        Self {
            sessions: vec![
                SessionEntry::new(
                    "4f9ca1b2",
                    "aipowerranking",
                    "Active",
                    "Running tests — 12 passed, fixing flaky timeout",
                ),
                SessionEntry::new(
                    "7b2ec0d1",
                    "genealogy",
                    "Idle",
                    "Idle — last activity 6m ago",
                ),
            ],
            ..Self::default()
        }
    }

    /// Build the live starting state: no rows, daemon presumed unreachable.
    ///
    /// Why: Child #2 drives the list from the daemon, not from the placeholder
    /// rows [`Self::skeleton`] seeds. The run loop starts here and the priming
    /// poll fills `sessions` + flips `daemon_reachable` once the context lands;
    /// starting empty (rather than with placeholders) means a single transient
    /// daemon outage never leaves stale illustrative rows on screen.
    /// What: returns [`CoordinatorState::default`] — empty input/history, no
    /// sessions, controller (row 0) selected, `daemon_reachable = false`.
    /// Test: `live_state_starts_empty_and_unreachable`.
    pub fn live() -> Self {
        Self::default()
    }

    /// Total number of selectable rows: the controller plus every session.
    ///
    /// Why: selection spans a unified list whose row 0 is always the controller
    /// bullet, so bounds checks must account for that synthetic first row.
    /// What: `1 + sessions.len()`.
    /// Test: `clamp_selection_*` rely on this count implicitly.
    pub fn row_count(&self) -> usize {
        1 + self.sessions.len()
    }

    /// The selected row index across the unified list (`0` = controller bullet).
    ///
    /// Why: callers that used to read the old `selected: usize` field (the
    /// renderer's active-row branch, `selected_session`) need the index; routing
    /// them through one accessor keeps the [`ListNav`] the single source of truth.
    /// What: returns [`ListNav::selected`].
    /// Test: `selection_movement_saturates`, `nav_default_selects_controller_row`.
    pub fn selected(&self) -> usize {
        self.nav.selected()
    }

    /// Focus a specific row by index, clamped into `0..row_count`.
    ///
    /// Why: STUI-1 lets the operator jump straight to a numbered session (and the
    /// post-mutation / focus paths need a way to set the selection directly rather
    /// than stepping it). Routing through one clamped setter keeps every selection
    /// change inside the list bounds.
    /// What: re-syncs `nav` to the current row count (so the bound is live) then
    /// selects `min(row, row_count - 1)`.
    /// Test: `select_row_clamps_into_bounds`.
    pub fn select_row(&mut self, row: usize) {
        self.nav.sync_len(self.row_count());
        self.nav.select(row);
    }

    /// Re-clamp the selection + scroll offset into `0..row_count` after a poll.
    ///
    /// Why: the live list shrinks between polls (sessions end); a stale index
    /// would render — and later index — out of bounds. STUI-1 additionally
    /// requires the selection AND scroll offset to be PRESERVED across a refresh
    /// when still valid, so this delegates to [`ListNav::sync_len`] rather than a
    /// bare index pin: a grown or unchanged list keeps the cursor exactly where
    /// the operator left it; only a shrink clamps inward.
    /// What: calls `self.nav.sync_len(self.row_count())`. `row_count` is always
    /// ≥ 1 because the controller row always exists, so the index never
    /// underflows.
    /// Test: `clamp_selection_empty_list`, `clamp_selection_single_row`,
    /// `clamp_selection_past_end`, `clamp_selection_preserves_valid_selection`.
    pub fn clamp_selection(&mut self) {
        self.nav.sync_len(self.row_count());
    }

    /// Move the selection up one row (saturating at the controller, row 0).
    ///
    /// Why: ↑ / `k` move the highlight toward the controller.
    /// What: delegates to [`ListNav::up`].
    /// Test: `selection_movement_saturates`.
    pub fn select_up(&mut self) {
        self.nav.up();
    }

    /// Move the selection down one row (saturating at the last session).
    ///
    /// Why: ↓ / `j` move the highlight toward the newest session.
    /// What: re-syncs the row count (so `nav` knows the live bound) then
    /// delegates to [`ListNav::down`].
    /// Test: `selection_movement_saturates`.
    pub fn select_down(&mut self) {
        self.nav.sync_len(self.row_count());
        self.nav.down();
    }

    /// Jump the selection up by one page (saturating at the controller, row 0).
    ///
    /// Why: Page-Up traverses a long fleet quickly (STUI-1 AC).
    /// What: delegates to [`ListNav::page_up`].
    /// Test: `page_scroll_moves_by_page_and_saturates`.
    pub fn page_up(&mut self) {
        self.nav.page_up();
    }

    /// Jump the selection down by one page (saturating at the last session).
    ///
    /// Why: Page-Down is the symmetric fast-scroll toward the newest session.
    /// What: re-syncs the row count then delegates to [`ListNav::page_down`].
    /// Test: `page_scroll_moves_by_page_and_saturates`.
    pub fn page_down(&mut self) {
        self.nav.sync_len(self.row_count());
        self.nav.page_down();
    }

    /// True when the controller bullet (row 0) is the active selection.
    ///
    /// Why: the layout renders the controller's right column as a synthetic
    /// status and a session's as its summary; the renderer branches on this.
    /// What: `self.selected() == 0`.
    /// Test: `controller_is_selected_at_row_zero`.
    pub fn controller_selected(&self) -> bool {
        self.selected() == 0
    }

    /// The session under the current selection, if a session (not the
    /// controller) is selected.
    ///
    /// Why: the active-row two-column renderer and future focus/action paths
    /// need the selected session; row 0 maps to the controller, so session
    /// indices are offset by one.
    /// What: returns `sessions[selected - 1]` when `selected > 0`, else `None`.
    /// Test: `selected_session_offsets_by_controller_row`.
    pub fn selected_session(&self) -> Option<&SessionEntry> {
        self.selected()
            .checked_sub(1)
            .and_then(|idx| self.sessions.get(idx))
    }

    /// Request an immediate daemon re-poll on the next run-loop tick (STUI-1).
    ///
    /// Why: after a list-mutating action (create/kill/stop/resume) the operator
    /// expects the fleet to refresh now, not up to a whole `--interval-ms` later.
    /// Setting a flag — rather than polling inline — keeps the mutation path free
    /// of async/IO and lets the single run-loop own all daemon calls.
    /// What: sets the private `needs_repoll` flag.
    /// Test: `request_repoll_sets_and_take_clears`.
    pub fn request_repoll(&mut self) {
        self.needs_repoll = true;
    }

    /// Read-and-clear the immediate-re-poll request.
    ///
    /// Why: the run loop checks this each tick; a take-style consume guarantees a
    /// single requested re-poll fires exactly once and is not re-triggered every
    /// subsequent tick.
    /// What: returns the current `needs_repoll` value and resets it to `false`.
    /// Test: `request_repoll_sets_and_take_clears`.
    pub fn take_repoll(&mut self) -> bool {
        std::mem::take(&mut self.needs_repoll)
    }

    /// Append a character to the input buffer (a printable keystroke).
    ///
    /// Why: typing builds the coordinator message; editing resets the recall
    /// cursor so the next ↑ starts from the newest history entry.
    /// What: pushes `c`; clears the history cursor.
    /// Test: `input_edits_buffer`.
    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
        self.history_cursor = None;
    }

    /// Delete the last input character (Backspace).
    ///
    /// Why: Backspace edits a mistyped message.
    /// What: pops the trailing char; clears the recall cursor.
    /// Test: `input_edits_buffer`.
    pub fn backspace(&mut self) {
        self.input.pop();
        self.history_cursor = None;
    }

    /// Clear the input buffer (Esc).
    ///
    /// Why: Esc abandons a half-typed message.
    /// What: empties [`Self::input`]; clears the recall cursor.
    /// Test: `input_edits_buffer`.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.history_cursor = None;
    }

    /// Submit the current input: record it in history and return it, clearing
    /// the buffer.
    ///
    /// Why: Enter submits. Child #1 only *echoes* the submission to history (no
    /// daemon wiring yet); later children route it to the coordinator chat.
    /// What: trims the buffer; when non-empty, pushes it onto the bounded
    /// history ring (dropping the oldest beyond [`INPUT_HISTORY_LIMIT`]) and
    /// returns `Some(text)`; an empty/whitespace buffer returns `None`. Always
    /// clears the buffer and resets the recall cursor.
    /// Test: `submit_records_history`, `history_ring_is_bounded`,
    /// `submit_empty_input_is_noop`.
    pub fn submit_input(&mut self) -> Option<String> {
        let typed = std::mem::take(&mut self.input);
        self.history_cursor = None;
        let trimmed = typed.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        self.history.push(trimmed.clone());
        if self.history.len() > INPUT_HISTORY_LIMIT {
            let overflow = self.history.len() - INPUT_HISTORY_LIMIT;
            self.history.drain(0..overflow);
        }
        Some(trimmed)
    }

    /// Recall the previous (older) history entry into the input (↑).
    ///
    /// Why: re-sending or editing a recent message should not require retyping.
    /// What: steps the recall cursor one entry toward the oldest and loads it;
    /// a no-op when history is empty.
    /// Test: `history_recall_walks_entries`.
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

    /// Recall the next (newer) history entry into the input (↓).
    ///
    /// Why: lets the operator step back down after recalling too far with ↑.
    /// What: advances the cursor toward the newest entry; stepping past the
    /// newest clears the cursor and empties the buffer; a no-op when not
    /// currently recalling.
    /// Test: `history_recall_walks_entries`.
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
}
