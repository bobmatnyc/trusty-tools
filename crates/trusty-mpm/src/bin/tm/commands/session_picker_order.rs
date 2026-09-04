//! What order the picker's menu shows, and the numbers it prints beside each
//! row (#3483, #3723, #6753).
//!
//! Why: split out of `session_picker.rs` when #6753 pushed that file past the
//! 500-SLOC production cap. The cut is a domain rather than a size convenience:
//! everything here answers "which row comes first and what number does it
//! carry", which is exactly the question #6753 found two different answers to —
//! the picker's first menu was ordered by the daemon's ascending slot and every
//! later one by the scope. [`prepare_menu`] is now the single answer, and living
//! in its own module is what keeps a second one from being added beside it.
//! What: the scope filter, the attached-then-active-then-rest sort, the stable
//! slot numbering with its positional fallback, and [`prepare_menu`], which
//! composes them into the whole state one render is decided by.
//! Test: `session_picker_tests.rs` — `prepare_menu_*`, `sort_sessions_*`,
//! `filter_sessions_by_term_*`, `next_launch_slot_*`.

use trusty_mpm::client::ManagedSessionSummary;

use super::session_picker::{
    PickerScope, SessionFilter, SessionSortArg, next_launch_slot, slots_are_stale,
};

/// Apply a [`SessionFilter`] to a session list (#3483).
///
/// Why: the static table and the interactive picker must filter identically, so
/// both call this rather than open-coding the predicate.
/// What: keeps the sessions [`SessionFilter::matches`] accepts. `filter = None`
/// is a no-op (returns `sessions` unchanged).
/// Test: `filter_sessions_by_term_matches_name`,
/// `filter_sessions_by_term_matches_task`,
/// `filter_sessions_by_term_matches_source_id`,
/// `filter_sessions_by_term_is_case_insensitive`,
/// `filter_sessions_by_term_no_match_returns_empty`,
/// `filter_sessions_by_term_none_is_noop`,
/// `filter_sessions_by_name_ignores_non_name_columns`.
pub(crate) fn filter_sessions_by_term(
    sessions: Vec<ManagedSessionSummary>,
    filter: Option<&SessionFilter>,
) -> Vec<ManagedSessionSummary> {
    let Some(filter) = filter else {
        return sessions;
    };
    sessions.into_iter().filter(|s| filter.matches(s)).collect()
}

/// Put a freshly-obtained session list into the order and scope the picker
/// renders (#6753).
///
/// Why: [`run_tty_picker`](super::session_picker::run_tty_picker) applied [`filter_sessions_by_term`] and
/// [`sort_sessions`] at the BOTTOM of its loop only, so its FIRST menu showed
/// the daemon's ascending-slot order while every later menu showed the scope's
/// order. The attached session — usually the highest slot — printed last, bare
/// Enter therefore targeted the oldest session while the hint read "resume most
/// recent", and the whole list reordered under the operator after the first
/// action. `tm ls`'s connector normalized before its first render and the bare
/// `tm` path did not, which is the divergence this function removes: it is now
/// the ONE place a list is prepared, applied at the TOP of the loop, so the
/// first render and every later one are the same code path rather than two that
/// must be kept in step.
/// What: the scope's term filter, then the scope's sort. Both are idempotent, so
/// a caller that already normalized (the `tm ls` connector) is unaffected.
/// Test: `prepare_menu_orders_the_first_render_like_every_later_one`,
/// `prepare_menu_is_idempotent`, `prepare_menu_applies_the_scopes_term_filter`.
pub(crate) fn normalize_for_scope(
    sessions: Vec<ManagedSessionSummary>,
    scope: &PickerScope,
) -> Vec<ManagedSessionSummary> {
    let mut sessions = filter_sessions_by_term(sessions, scope.term.as_ref());
    sort_sessions(&mut sessions, scope.sort);
    sessions
}

/// Everything one render of the picker menu is decided by (#6753).
///
/// Why: the three values below were computed inline at the top of
/// [`run_tty_picker`](super::session_picker::run_tty_picker)'s loop, from whatever order `sessions` happened to be in.
/// That made the ORDER an input nothing named and nothing could test, which is
/// how the first render came to disagree with every later one. Naming the
/// prepared list and its derived values together means a render cannot be
/// produced without going through [`prepare_menu`], so there is no second path
/// for the order to diverge on.
/// What: the normalized list plus the three menu-wide facts derived from it.
/// Test: `prepare_menu_*` in `session_picker_tests.rs`.
pub(crate) struct Menu {
    /// The list in the order the menu prints it.
    pub(crate) sessions: Vec<ManagedSessionSummary>,
    /// #3678: every `slot` decoded to the `0` sentinel, so numbers are positional.
    pub(crate) stale_slots: bool,
    /// The "launch new session" menu number (see [`next_launch_slot`]).
    pub(crate) new_idx: u32,
    /// #2148: bare Enter on position 0 would restart rather than resume.
    pub(crate) first_needs_restart: bool,
}

/// Prepare one render of the picker menu from a freshly-obtained list (#6753).
///
/// Why: this is the ONE place a menu is derived, so the first menu and every
/// later one are the same computation rather than two that must be kept in step.
/// [`run_tty_picker`](super::session_picker::run_tty_picker) used to normalize at the BOTTOM of its loop only: its first
/// menu therefore printed the daemon's ascending-slot order, in which the
/// attached session — usually the highest slot — came LAST. Bare Enter targets
/// position 0, so it aimed at the oldest session (usually stopped, so it fired
/// `ConfirmRestart`) while the hint read "resume most recent", and the whole list
/// reordered under the operator after the first action.
/// What: [`normalize_for_scope`] first, then [`slots_are_stale`],
/// [`next_launch_slot`] and the `first_needs_restart` flag off the NORMALIZED
/// list. A deleted position 0 is never `first_needs_restart`, because
/// [`decide_for_index`](super::session_picker::decide_for_index) checks `deleted` ahead of this flag.
/// Test: `prepare_menu_orders_the_first_render_like_every_later_one`,
/// `prepare_menu_bare_enter_targets_the_attached_session`,
/// `prepare_menu_is_idempotent`, `prepare_menu_applies_the_scopes_term_filter`,
/// `prepare_menu_launch_slot_is_the_maximum_not_the_last`.
pub(crate) fn prepare_menu(sessions: Vec<ManagedSessionSummary>, scope: &PickerScope) -> Menu {
    let sessions = normalize_for_scope(sessions, scope);
    let stale_slots = slots_are_stale(&sessions);
    let new_idx = next_launch_slot(&sessions);
    let first_needs_restart = sessions
        .first()
        .map(|s| !s.deleted && super::guided_resume::needs_restart(&s.state))
        .unwrap_or(false);
    Menu {
        sessions,
        stale_slots,
        new_idx,
        first_needs_restart,
    }
}

/// The attached→active→everything-else group a session belongs in (owner
/// request 2026-07-29).
///
/// Why: Bob's ask — the listing should group attached sessions first, then
/// active ones, then the rest (stopped/errored/provisioning/etc.) — ABOVE
/// whatever `recent`/`alpha` secondary order the operator picked, so a
/// session they're actively connected to never scrolls below a merely-recent
/// stopped one.
/// What: `0` when `s.attached` (a client is connected RIGHT NOW — the
/// strongest signal, mirrors [`session_picker_render::state_color`](crate::commands::session_picker_render::state_color)'s own
/// precedence); `1` for `state == "active"` (not attached); `2` for every
/// other state. Lower sorts first.
/// Test: `sort_sessions_recent_groups_attached_before_active_before_stopped`,
/// `sort_sessions_alpha_groups_attached_before_active_before_stopped`.
fn group_rank(s: &ManagedSessionSummary) -> u8 {
    if s.attached {
        0
    } else if s.state == "active" {
        1
    } else {
        2
    }
}

/// Sort `sessions` in place per [`SessionSortArg`] (#3483), grouped
/// attached→active→everything-else (owner request 2026-07-29).
///
/// Why: shared by the static table (`tm ls` / `tm sessions ls`) and the
/// interactive picker so both views order sessions identically.
/// What: primary key is [`group_rank`] (attached, then active, then the
/// rest); within each group, `Recent` sorts descending by [`recency_key`]
/// (most recent first) and `Alpha` sorts ascending, case-insensitively, by
/// `name`. Both use the stable `sort_by`, so equal keys preserve the
/// daemon's original relative order.
/// Test: `sort_sessions_recent_orders_by_last_activity`,
/// `sort_sessions_recent_falls_back_to_created_at`,
/// `sort_sessions_alpha_orders_by_name_case_insensitive`,
/// `sort_sessions_recent_groups_attached_before_active_before_stopped`,
/// `sort_sessions_alpha_groups_attached_before_active_before_stopped`.
pub(crate) fn sort_sessions(sessions: &mut [ManagedSessionSummary], sort: SessionSortArg) {
    match sort {
        SessionSortArg::Recent => {
            sessions.sort_by(|a, b| {
                group_rank(a)
                    .cmp(&group_rank(b))
                    .then_with(|| recency_key(b).cmp(recency_key(a)))
            });
        }
        SessionSortArg::Alpha => {
            sessions.sort_by(|a, b| {
                group_rank(a)
                    .cmp(&group_rank(b))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        }
    }
}

/// Best-available recency signal for a session (#3483).
///
/// Why: `last_activity_at` reflects actual usage (the daemon updates it on
/// every interaction), which is the signal an operator scanning `tm ls`
/// actually wants — a session touched five minutes ago should outrank one
/// merely CREATED first. `created_at` is the fallback for legacy/additive
/// records that predate the activity timestamp; a session with neither sorts
/// last (empty string is the lexicographic minimum).
/// What: RFC 3339 timestamps compare correctly as plain strings because the
/// daemon always emits them in the same normalized (UTC, fixed-precision)
/// form.
/// Test: covered indirectly by `sort_sessions_recent_orders_by_last_activity`
/// and `sort_sessions_recent_falls_back_to_created_at`.
fn recency_key(s: &ManagedSessionSummary) -> &str {
    s.last_activity_at
        .as_deref()
        .or(s.created_at.as_deref())
        .unwrap_or("")
}
