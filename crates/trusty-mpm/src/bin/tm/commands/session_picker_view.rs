//! In-picker sort cycling, inline filtering, and the pinned default row
//! (issue #3552).
//!
//! Why: `tm ls recent|alpha <filter>` (#3483) put sort and filter on the CLI
//! only. Once the interactive picker is open there is no way to reorder or
//! narrow it — the operator has to quit, retype the command with the right
//! positional words, and lose whatever they were looking at. A user who opens
//! `tm ls` and hunts for a sort control inside the picker finds nothing, so the
//! feature is discoverable only by already knowing the arg grammar. This module
//! adds the in-picker half: `[s]` cycles the sort order and `/<text>` filters,
//! both mutating the same [`PickerScope`] fields the CLI grammar already sets,
//! so there is one sort implementation and one filter implementation rather
//! than a second pair reachable only from the picker.
//!
//! What: [`parse_view_command`] is the pure input→[`ViewCommand`] seam,
//! [`apply_view_command`] the pure state transition it drives, and
//! [`default_index`] the pinned-selection lookup that keeps bare Enter aimed at
//! the same session across a re-sort or a filter change. [`view_status_line`]
//! renders the one-line "what is this list showing" banner. Living in its own
//! module keeps `session_picker.rs` under the 500-SLOC production cap.
//!
//! Test: `session_picker_view_tests.rs` — `parse_view_command_*`,
//! `sort_cycle_*`, `apply_view_command_*`, `default_index_*`,
//! `view_status_line_*`.

use trusty_mpm::client::ManagedSessionSummary;

use super::session_picker::{PickerScope, SessionFilter, SessionSortArg};

/// The escape byte a terminal sends for the Esc key.
///
/// Why: the picker reads whole LINES from stdin, so Esc arrives as this raw
/// byte inside the line rather than as a keypress event. Recognising it is what
/// makes "press Esc to drop the filter" true for an operator who reaches for
/// the key the rest of the world uses to cancel a filter box.
const ESC: char = '\u{1b}';

/// A change to WHAT the picker is showing, as opposed to an action on a
/// session (#3552).
///
/// Why: sort order and the inline filter are view state — nothing is resumed,
/// launched, renamed, or deleted — so they resolve to their own decision
/// variant and the driver applies them without a daemon round-trip. Keeping
/// them off [`super::session_picker::PickerDecision`]'s session-acting variants
/// is what lets the driver `continue` straight back to a fresh render.
/// What: three variants covering the whole in-picker grammar. `SetFilter`
/// carries the raw typed text; the case-insensitive lowering happens inside
/// [`SessionFilter`], which is the one place matching is defined.
/// Test: `parse_view_command_s_cycles_sort`,
/// `parse_view_command_slash_sets_the_filter`,
/// `parse_view_command_bare_slash_clears_the_filter`,
/// `parse_view_command_esc_clears_the_filter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ViewCommand {
    /// `s` / `sort` — advance to the next sort order, wrapping.
    CycleSort,
    /// `/<text>` — narrow the list to rows matching `text`.
    SetFilter(String),
    /// `/` alone, backspaced to empty, or Esc — show every row again.
    ClearFilter,
}

/// Parse one trimmed picker line into a [`ViewCommand`], or `None` when the
/// line is not a view command at all (#3552).
///
/// Why: a pure seam means the whole in-picker grammar is unit-testable without
/// a terminal, a daemon, or a tmux server — the same reason
/// [`super::session_picker::parse_picker_choice`] exists. Returning `Option`
/// rather than a decision keeps this a strict ADDITION to that parser: a line
/// this function declines falls through to every pre-existing branch unchanged.
/// What: grammar —
///   • `s` / `S` / `sort` (case-insensitive) → [`ViewCommand::CycleSort`]
///   • `/<text>` → [`ViewCommand::SetFilter`] with `text` trimmed
///   • `/` alone, or `/` with only whitespace after it → [`ViewCommand::ClearFilter`]
///     — this is the "backspaced the filter box back to empty" case
///   • a line whose only non-whitespace content is Esc → [`ViewCommand::ClearFilter`]
///   • anything else → `None`
///
/// `s` cannot collide with the picker's other keys: `q`, `ls`/`list`, the
/// `r<N>`/`d<N>` prefixes, the `n` launch grammar, and bare numbers all start
/// with a different character, and `sort` is not a session name the parser ever
/// reads.
/// Test: `parse_view_command_s_cycles_sort`,
/// `parse_view_command_sort_word_cycles_sort`,
/// `parse_view_command_is_case_insensitive`,
/// `parse_view_command_slash_sets_the_filter`,
/// `parse_view_command_slash_filter_is_trimmed`,
/// `parse_view_command_bare_slash_clears_the_filter`,
/// `parse_view_command_esc_clears_the_filter`,
/// `parse_view_command_declines_every_other_token`.
pub(crate) fn parse_view_command(choice: &str) -> Option<ViewCommand> {
    let choice = choice.trim();
    if choice.eq_ignore_ascii_case("s") || choice.eq_ignore_ascii_case("sort") {
        return Some(ViewCommand::CycleSort);
    }
    // Esc arrives as a raw byte in the line, never as a keypress (see `ESC`).
    if !choice.is_empty() && choice.chars().all(|c| c == ESC) {
        return Some(ViewCommand::ClearFilter);
    }
    let rest = choice.strip_prefix('/')?;
    let rest = rest.trim();
    Some(match rest.is_empty() {
        true => ViewCommand::ClearFilter,
        false => ViewCommand::SetFilter(rest.to_string()),
    })
}

/// Apply a [`ViewCommand`] to the picker's live scope (#3552).
///
/// Why: the scope already carries the sort order and the filter the CLI
/// grammar set, and [`super::session_picker_order::prepare_menu`] already
/// re-applies both on every render. Mutating those same two fields is
/// therefore the whole feature — no second ordering path, no second filtering
/// path, and no way for the in-picker view to drift from the `tm ls recent
/// <filter>` view it started as.
/// What: `CycleSort` advances [`PickerScope::sort`] via [`SessionSortArg::next`];
/// `SetFilter` replaces [`PickerScope::term`] with a visible-column filter;
/// `ClearFilter` sets it to `None`. The filter is always
/// [`SessionFilter::visible`] because the numbered picker is only ever reached
/// from `tm ls` and bare `tm` — `tm f`'s NAME-scoped filter drives its own
/// raw-mode loop in [`super::session_picker_filter`] and never arrives here.
/// Test: `apply_view_command_cycle_advances_and_wraps`,
/// `apply_view_command_set_filter_narrows_then_clear_restores`.
pub(crate) fn apply_view_command(scope: &mut PickerScope, cmd: &ViewCommand) {
    match cmd {
        ViewCommand::CycleSort => scope.sort = scope.sort.next(),
        ViewCommand::SetFilter(text) => scope.term = Some(SessionFilter::visible(text)),
        ViewCommand::ClearFilter => scope.term = None,
    }
}

impl SessionSortArg {
    /// Every sort order the picker cycles through, in cycle order (#3552).
    ///
    /// Why: `[s]` has to enumerate the orders, and the legend has to name them
    /// in the order pressing the key will actually produce. One array is what
    /// keeps those two from disagreeing when a third order is added.
    /// What: the documented cycle, `recent` then `alpha`.
    /// Test: `sort_cycle_visits_every_documented_order_and_wraps`.
    pub(crate) const CYCLE: [Self; 2] = [Self::Recent, Self::Alpha];

    /// The next sort order in the cycle, wrapping at the end (#3552).
    ///
    /// Why: pressing `[s]` repeatedly must always land somewhere — a cycle that
    /// stopped at the last order would leave the operator unable to get back
    /// without quitting, which is the exact friction this issue is about.
    /// What: the successor in [`Self::CYCLE`], wrapping to the first.
    /// Test: `sort_cycle_visits_every_documented_order_and_wraps`,
    /// `sort_next_wraps_from_the_last_order`.
    pub(crate) fn next(self) -> Self {
        let at = Self::CYCLE.iter().position(|c| *c == self).unwrap_or(0);
        Self::CYCLE[(at + 1) % Self::CYCLE.len()]
    }

    /// The order's name as the CLI grammar and the picker both spell it.
    ///
    /// Why: `tm ls recent` and the picker's status line must use one spelling,
    /// or the status line teaches an arg that does not parse.
    /// Test: `view_status_line_names_the_active_sort`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Alpha => "alpha",
        }
    }
}

/// Which row bare Enter targets, honoring the pinned selection (#3552).
///
/// Why: cycling the sort or typing a filter is a BROWSING action, but both
/// reorder the list, and bare Enter has always aimed at position 0. Without a
/// pin, pressing `[s]` to hunt for one session silently repoints the default at
/// a different one — and that default resumes or (via `ConfirmRestart`) offers
/// to restart a real session. Pinning by session id means re-sorting and
/// filtering change what the operator SEES without changing what Enter DOES,
/// as long as the pinned session is still on screen.
/// What: the position of `selected_id` in `sessions`, or `0` when nothing is
/// pinned or the pinned session is no longer visible (filtered out, deleted, or
/// gone from the fleet). `0` on an empty list is never indexed — every caller
/// checks `is_empty()` first.
/// Test: `default_index_pins_the_selected_session_across_a_resort`,
/// `default_index_falls_back_to_zero_when_the_pin_is_filtered_out`,
/// `default_index_falls_back_to_zero_without_a_pin`.
pub(crate) fn default_index(
    sessions: &[ManagedSessionSummary],
    selected_id: Option<&str>,
) -> usize {
    selected_id
        .and_then(|id| sessions.iter().position(|s| s.id == id))
        .unwrap_or(0)
}

/// The one-line "what is this list showing" banner printed above the prompt
/// (#3552).
///
/// Why: `[s]` and `/<text>` are only usable if the operator can see what they
/// did. A picker that silently reorders after a keypress reads as a glitch;
/// naming the active sort and filter — and the key that clears the filter —
/// makes the state and its undo visible at the moment they matter.
/// What: `sort=<label>` always; `filter='<term>'` and the `[/] clears` hint
/// only while a filter is active. The term is the filter's own lowercased
/// needle, which is exactly what matching uses.
/// Test: `view_status_line_names_the_active_sort`,
/// `view_status_line_names_the_active_filter_and_its_undo`,
/// `view_status_line_omits_the_filter_when_none_is_set`.
pub(crate) fn view_status_line(sort: SessionSortArg, filter: Option<&SessionFilter>) -> String {
    let base = format!("tm: sort={} ([s] cycles)", sort.label());
    match filter {
        None => base,
        Some(f) => format!("{base}, filter='{}' ([/] clears)", f.needle()),
    }
}

#[cfg(test)]
#[path = "session_picker_view_tests.rs"]
mod tests;
