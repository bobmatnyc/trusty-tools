//! `tm f` — the type-to-filter session picker.
//!
//! Why: `tm ls <term>` is a one-shot filter — you type a guess, get a table,
//! and retype the whole command when the guess was wrong. With ~90 sessions
//! that is the slow half of every lookup. `tm f` keeps the list on screen and
//! narrows it per keystroke, so the guess is refined instead of re-issued.
//!
//! What this reuses, and what it does not: rows render through
//! [`session_picker_render::format_session_row`](super::session_picker_render::format_session_row),
//! selection resolves through
//! [`session_picker::decide_for_index`](super::session_picker::decide_for_index),
//! and Enter dispatches into the same
//! [`guided_resume::resume_guided_session`](super::guided_resume::resume_guided_session)
//! the numbered picker uses — so the tombstone (#3034), dead-session (#2595),
//! and attach/switch-client (#2678) semantics are the picker's, not a
//! reimplementation. What is NOT shared is the INPUT loop: the numbered picker
//! reads whole lines (`stdin().read_line`), which cannot re-render per
//! keystroke. Per-keystroke input requires raw mode, so this module owns a raw
//! key loop and the numbered picker keeps its line loop. That is the one
//! deliberate divergence; the keys below are additive (arrows and Backspace
//! have no meaning in a line-reader) rather than conflicting.
//!
//! Keys: printable characters extend the pattern, Backspace deletes, Up/Down
//! move the selection, Enter acts on the selected session, Esc / Ctrl-C / Ctrl-D
//! cancel, Ctrl-U clears the pattern.
//!
//! Non-TTY safety: [`interactive_filter_allowed`] is the single gate, and it is
//! checked BEFORE raw mode is ever requested. A pipe, `--json`, `--all`, or
//! `TERM=dumb` all fall through to the static one-shot listing — a picker that
//! grabbed raw mode on a pipe would hang every script that pipes `tm`.
//!
//! Test: `session_picker_filter_tests.rs` covers the gate, the match predicate,
//! and every state transition. The terminal I/O in [`read_filter_selection`] is
//! verified by hand only.

use std::io::Write as _;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, queue, terminal};
use trusty_mpm::client::ManagedSessionSummary;

use super::session_picker::{PickerDecision, SessionFilter, SessionSortArg};

/// Most rows drawn at once, so a 90-session fleet cannot scroll its own prompt
/// off the screen before the operator has typed anything.
const MAX_VISIBLE_ROWS: usize = 15;

/// One key press, reduced to the actions this picker understands.
///
/// Why: separating "which key" from "what crossterm reported" makes every
/// transition testable without a terminal — the untestable part shrinks to the
/// one `From<KeyEvent>`-shaped mapping in [`read_filter_selection`].
/// What: the seven inputs the key loop acts on.
/// Test: `filter_state_*` transitions in `session_picker_filter_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterKey {
    /// A printable character to append to the pattern.
    Char(char),
    /// Delete the last pattern character.
    Backspace,
    /// Move the selection one row up.
    Up,
    /// Move the selection one row down.
    Down,
    /// Clear the whole pattern (Ctrl-U).
    ClearPattern,
    /// Act on the selected row (Enter).
    Accept,
    /// Leave without acting (Esc / Ctrl-C / Ctrl-D).
    Cancel,
}

/// What the key loop should do after a [`FilterKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterAction {
    /// State changed — redraw the list.
    Redraw,
    /// Act on the currently selected visible row.
    Accept,
    /// Exit the picker without acting.
    Cancel,
    /// Nothing changed; do not redraw.
    Ignore,
}

/// The mutable state of a type-to-filter session: the pattern and the cursor.
///
/// Why: the whole point of extracting this is that a TUI loop is otherwise
/// untestable. Every rule that could regress — the selection clamping when a
/// keystroke shrinks the list under the cursor, Enter on an empty result set,
/// Backspace on an empty pattern — lives here as a pure transition and is unit
/// tested, leaving only the draw calls unverified.
/// What: `pattern` is the raw text the operator typed (matched
/// case-insensitively; see [`matches_pattern`]); `selected` is a 0-based index
/// into the CURRENTLY VISIBLE rows, not into the full session list.
/// Test: every `filter_state_*` test in `session_picker_filter_tests.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FilterState {
    pattern: String,
    selected: usize,
}

impl FilterState {
    /// Start with `pattern` pre-typed (the `tm f <pattern>` seed) and the first
    /// row selected.
    pub(crate) fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            selected: 0,
        }
    }

    /// The pattern typed so far.
    pub(crate) fn pattern(&self) -> &str {
        &self.pattern
    }

    /// The 0-based index into the visible rows.
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// Apply one key and report what the caller should do next.
    ///
    /// Why: `visible_len` is passed in rather than stored because the visible
    /// set is recomputed from the pattern after every edit. Editing the pattern
    /// can shrink the list out from under the cursor, so an edit ALWAYS resets
    /// the selection to the first row — leaving it where it was would point at
    /// a different session than the one under it a keystroke ago, which is how
    /// a fuzzy picker acts on the wrong row.
    /// What: `Char`/`Backspace`/`ClearPattern` edit the pattern and reset the
    /// selection, returning `Redraw` (or `Ignore` when the edit is a no-op, e.g.
    /// Backspace on an empty pattern). `Up`/`Down` move the selection, saturating
    /// at both ends and returning `Ignore` when already there. `Accept` returns
    /// `Ignore` when nothing is visible — Enter on an empty result set must do
    /// nothing rather than act on a row that is not there. `Cancel` always
    /// returns `Cancel`.
    /// Test: `filter_state_char_appends_and_resets_selection`,
    /// `filter_state_backspace_on_empty_pattern_is_ignored`,
    /// `filter_state_down_saturates_at_last_row`,
    /// `filter_state_up_saturates_at_first_row`,
    /// `filter_state_accept_with_no_visible_rows_is_ignored`,
    /// `filter_state_edit_resets_selection_when_list_shrinks`,
    /// `filter_state_clear_pattern_empties_and_resets`.
    pub(crate) fn apply(&mut self, key: FilterKey, visible_len: usize) -> FilterAction {
        match key {
            FilterKey::Char(c) => {
                self.pattern.push(c);
                self.selected = 0;
                FilterAction::Redraw
            }
            FilterKey::Backspace => {
                if self.pattern.pop().is_none() {
                    return FilterAction::Ignore;
                }
                self.selected = 0;
                FilterAction::Redraw
            }
            FilterKey::ClearPattern => {
                if self.pattern.is_empty() {
                    return FilterAction::Ignore;
                }
                self.pattern.clear();
                self.selected = 0;
                FilterAction::Redraw
            }
            FilterKey::Up => {
                if self.selected == 0 {
                    return FilterAction::Ignore;
                }
                self.selected -= 1;
                FilterAction::Redraw
            }
            FilterKey::Down => {
                let last = visible_len.saturating_sub(1);
                if visible_len == 0 || self.selected >= last {
                    return FilterAction::Ignore;
                }
                self.selected += 1;
                FilterAction::Redraw
            }
            FilterKey::Accept => {
                if visible_len == 0 {
                    FilterAction::Ignore
                } else {
                    FilterAction::Accept
                }
            }
            FilterKey::Cancel => FilterAction::Cancel,
        }
    }
}

/// Does this session's NAME contain `pattern`, case-insensitively?
///
/// Why: routed through [`SessionFilter::name`] rather than an inline
/// `to_lowercase().contains(…)` so the interactive filter and the one-shot
/// `tm f` / `tm ls` filters can never disagree about what "matches" means.
/// What: `true` for an empty pattern (an unfiltered list is the starting state).
/// Test: `matches_pattern_is_case_insensitive_on_name`,
/// `matches_pattern_ignores_task_and_project`,
/// `matches_pattern_empty_matches_everything`.
pub(crate) fn matches_pattern(s: &ManagedSessionSummary, pattern: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    SessionFilter::name(pattern).matches(s)
}

/// Indices of the sessions currently passing `pattern`, in list order.
///
/// Why: indices, not clones — the selection has to resolve back to the ORIGINAL
/// session so the resume path acts on the row the operator was looking at.
/// Test: `visible_indices_narrows_to_matching_names`.
pub(crate) fn visible_indices(sessions: &[ManagedSessionSummary], pattern: &str) -> Vec<usize> {
    sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| matches_pattern(s, pattern))
        .map(|(i, _)| i)
        .collect()
}

/// May `tm f` take over the terminal, or must it print a static list?
///
/// Why: this is the regression that hangs scripts. `tm ls` is piped in the
/// wild; if the interactive path engages when stdout is a pipe, the process
/// grabs raw mode and blocks on a keyboard nobody is at. The gate is a pure
/// function, checked before raw mode is requested, so the non-TTY case is a
/// unit test rather than a thing discovered in production.
/// What: requires BOTH stdin and stdout to be TTYs (a piped stdin would EOF
/// immediately; a piped stdout must stay a clean table), no `--json`, no
/// `--all`, and a `TERM` that is neither absent nor `dumb` — a dumb terminal
/// has no cursor addressing, so the redraw would smear the list down the
/// screen. `NO_COLOR` is deliberately NOT consulted: it suppresses color, not
/// interactivity, and the row renderer already honours it.
/// Test: `interactive_filter_allowed_requires_both_ttys`,
/// `interactive_filter_allowed_false_for_json_and_all`,
/// `interactive_filter_allowed_false_for_dumb_or_missing_term`,
/// `interactive_filter_allowed_true_on_a_real_terminal`,
/// `interactive_filter_allowed_ignores_no_color`.
pub(crate) fn interactive_filter_allowed(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    all: bool,
    term: Option<&str>,
) -> bool {
    stdin_tty
        && stdout_tty
        && !json
        && !all
        && matches!(term, Some(t) if !t.is_empty() && !t.eq_ignore_ascii_case("dumb"))
}

/// `tm f [pattern]` — filter sessions by name, interactively when possible.
///
/// Why: one entry point owns the interactive-vs-static decision so the fallback
/// can never be bypassed by a future caller.
/// What: when [`interactive_filter_allowed`] says no, delegates to the same
/// static renderer `tm ls` uses, with the pattern applied as a NAME-scoped
/// [`SessionFilter`] — so a piped `tm f api` is exactly a filtered `tm ls`.
/// Otherwise fetches the fleet once and runs the raw-mode key loop, dispatching
/// an accepted row through the numbered picker's own resume path.
/// Test: the gate by `interactive_filter_allowed_*`; the static branch shares
/// `session_ls`'s coverage; the key loop by hand.
pub(crate) async fn run_f_command(
    client: &reqwest::Client,
    url: &str,
    args: crate::cli::FindArgs,
) -> anyhow::Result<()> {
    use std::io::IsTerminal as _;

    let crate::cli::FindArgs {
        pattern,
        json,
        source_id,
        current,
        all,
    } = args;
    let pattern = pattern.join(" ");
    let sid: Option<String> = if current {
        super::session::derive_source_id_from_cwd()
    } else {
        source_id
    };
    let filter = (!pattern.is_empty()).then(|| SessionFilter::name(&pattern));

    let term = std::env::var("TERM").ok();
    if !interactive_filter_allowed(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        json,
        all,
        term.as_deref(),
    ) {
        return super::managed::session_ls(
            client,
            url,
            json,
            sid.as_deref(),
            all,
            // `-a` is a `tm ls` flag only; `tm f`'s fallback is unchanged.
            false,
            SessionSortArg::Recent,
            filter,
            // `tm f` has no `--no-prune` flag; its fallback keeps the #4702 default.
            false,
        )
        .await;
    }

    let mut sessions =
        super::session_picker::fetch_live_sessions(client, url, sid.as_deref(), false).await?;
    super::session_picker::sort_sessions(&mut sessions, SessionSortArg::Recent);
    if sessions.is_empty() {
        super::managed_render::render_session_table(&sessions, sid.as_deref());
        return Ok(());
    }

    let Some(idx) = read_filter_selection(&sessions, &pattern)? else {
        eprintln!("tm: quit.");
        return Ok(());
    };

    // The SAME resolution the numbered picker applies to an explicit numeric
    // choice, so the tombstone (#3034) and dead-session (#2595) gates hold here
    // too. `false` because those gates key off bare Enter in the numbered
    // picker; here Enter is always an explicit selection of a highlighted row.
    match super::session_picker::decide_for_index(&sessions, idx, false) {
        PickerDecision::Resume(i) => {
            super::guided_resume::resume_guided_session(client, url, &sessions[i]).await?;
        }
        PickerDecision::Unresumable(i) => {
            eprintln!(
                "tm: '{}' is dead — its workspace no longer exists anywhere on disk; \
                 run `tm sessions rm {}` to remove the record.",
                sessions[i].name, sessions[i].id
            );
        }
        PickerDecision::SlotDeleted(i) => {
            eprintln!(
                "tm: session [{}] was deleted.",
                super::session_picker::shown_slot(&sessions, i)
            );
        }
        _ => {}
    }
    Ok(())
}

/// Restores cooked mode however the key loop exits — including on `?`.
///
/// Why: leaving a terminal in raw mode is the worst failure this module can
/// have; the operator's shell stops echoing and Ctrl-C stops working. A `Drop`
/// guard covers the early-return and panic paths a trailing
/// `disable_raw_mode()` would miss.
struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(std::io::stderr(), cursor::Show);
    }
}

/// Run the raw-mode key loop; return the accepted session's index, or `None` on
/// cancel.
///
/// Why: the list is drawn to STDERR, matching the numbered picker — stdout stays
/// clean so `tm f … | …` never carries escape sequences even in the branch
/// where a TTY was detected on both streams.
/// What: draws the pattern line plus up to [`MAX_VISIBLE_ROWS`] matching rows,
/// blocks on one key at a time, and redraws in place by rewinding the cursor
/// over the lines it drew. Raw mode is dropped before returning, so the caller's
/// tmux attach gets a cooked terminal.
/// Test: hand-verified. The pure decisions it delegates to ([`FilterState::apply`],
/// [`visible_indices`], [`interactive_filter_allowed`]) are unit tested.
fn read_filter_selection(
    sessions: &[ManagedSessionSummary],
    seed: &str,
) -> anyhow::Result<Option<usize>> {
    let use_color = super::session_picker_render::picker_use_color(true);
    terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;

    let mut state = FilterState::new(seed);
    let mut visible = visible_indices(sessions, state.pattern());
    let mut drawn = 0usize;
    loop {
        drawn = draw(sessions, &visible, &state, drawn, use_color)?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Windows reports both press and release; act on press only.
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mapped = match key.code {
            KeyCode::Esc => Some(FilterKey::Cancel),
            KeyCode::Char('c' | 'd') if ctrl => Some(FilterKey::Cancel),
            KeyCode::Char('u') if ctrl => Some(FilterKey::ClearPattern),
            KeyCode::Char(c) if !ctrl => Some(FilterKey::Char(c)),
            KeyCode::Backspace => Some(FilterKey::Backspace),
            KeyCode::Up => Some(FilterKey::Up),
            KeyCode::Down => Some(FilterKey::Down),
            KeyCode::Enter => Some(FilterKey::Accept),
            _ => None,
        };
        let Some(mapped) = mapped else { continue };

        match state.apply(mapped, visible.len()) {
            FilterAction::Redraw => visible = visible_indices(sessions, state.pattern()),
            FilterAction::Accept => return Ok(visible.get(state.selected()).copied()),
            FilterAction::Cancel => return Ok(None),
            FilterAction::Ignore => {}
        }
    }
}

/// Draw the filter prompt and the visible rows; return the line count drawn.
///
/// Why: the returned count is how the next call rewinds — an off-by-one here
/// smears the list, so the drawing function is the one that reports it.
/// What: rewinds `previous` lines, clears downward, then writes the pattern
/// line, up to [`MAX_VISIBLE_ROWS`] rows (the selected one marked `>` and
/// bolded via the shared row renderer's color gate), an overflow note when the
/// match count exceeds the cap, and the key legend. Raw mode needs `\r\n`.
fn draw(
    sessions: &[ManagedSessionSummary],
    visible: &[usize],
    state: &FilterState,
    previous: usize,
    use_color: bool,
) -> anyhow::Result<usize> {
    let mut out = std::io::stderr();
    if previous > 0 {
        queue!(out, cursor::MoveUp(previous as u16))?;
    }
    queue!(
        out,
        cursor::MoveToColumn(0),
        terminal::Clear(terminal::ClearType::FromCursorDown),
        cursor::Hide
    )?;

    let shown = visible.len().min(MAX_VISIBLE_ROWS);
    write!(
        out,
        "tm: filter> {}\u{2588}\r\n{} of {} session(s)\r\n",
        state.pattern(),
        visible.len(),
        sessions.len()
    )?;
    let mut lines = 2;
    for (row, &idx) in visible.iter().take(shown).enumerate() {
        let marker = if row == state.selected() { ">" } else { " " };
        let text = super::session_picker_render::format_session_row(
            super::session_picker::shown_slot(sessions, idx),
            &sessions[idx],
            use_color,
        );
        write!(out, "{marker} {text}\r\n")?;
        lines += 1;
    }
    if visible.len() > shown {
        write!(out, "  … {} more — keep typing\r\n", visible.len() - shown)?;
        lines += 1;
    }
    write!(
        out,
        "[type] filter  [\u{2191}\u{2193}] select  [Enter] open  [Esc] cancel\r\n"
    )?;
    lines += 1;
    out.flush()?;
    Ok(lines)
}

#[cfg(test)]
#[path = "session_picker_filter_tests.rs"]
mod tests;
