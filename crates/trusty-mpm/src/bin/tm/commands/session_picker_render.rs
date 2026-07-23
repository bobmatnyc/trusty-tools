//! Verbless, color-coded row rendering for the `tm ls`/bare-`tm` session
//! picker (issue #3723).
//!
//! Why: Bob's feedback on the pre-existing `[N] restart <name> (state)` /
//! `[N] resume <name> (state)` rows — "remove 'restart/resume', that's
//! confusing. Use colors to indicate their current state." The verb (restart
//! vs. resume vs. attach) is an implementation detail chosen at SELECTION
//! time (see `guided_resume::plan_resume`); it tells the operator nothing
//! about the session's CURRENT state and duplicates the state word already
//! printed in parentheses. Extracted out of `session_picker.rs` (which sits
//! near the 500-SLOC production cap) so this new logic doesn't push it over.
//!
//! What: [`picker_use_color`] is the TTY/`NO_COLOR` gate, resolved ONCE per
//! picker invocation at the I/O boundary (`run_tty_picker`) rather than
//! probed per-row — matching this crate's established pattern of an explicit
//! `use_color: bool` parameter threaded into pure formatting functions
//! (`formatters::banner::shade_image`) instead of relying on the `colored`
//! crate's global `colored::control` override, whose process-wide mutable
//! state is documented (`two_panel/tests.rs`) to race across parallel tests.
//! [`format_session_row`] is the pure, verbless row formatter: `[N] <name>
//! (<colored state>)`, with the deleted/unresumable notices preserved
//! unchanged apart from the verb drop. [`colorize`] wraps text in a raw ANSI
//! SGR escape when `use_color` is true, else returns it unchanged — no
//! external color crate dependency, fully deterministic and side-effect-free.
//!
//! Test: `session_picker_tests.rs` — `format_session_row_*` (verbless output,
//! color on/off), `state_color_*` (mapping), `colorize_*` (escape/plain).

use trusty_mpm::client::ManagedSessionSummary;

/// Color category for a picker row's state word.
///
/// Why: a small closed set — rather than raw ANSI codes scattered through the
/// formatter — keeps [`state_color`]'s mapping table the single place state
/// strings turn into a color, and keeps [`colorize`] trivially testable
/// against each variant.
/// What: `Green` (active, not attached), `AttachedCyan` (a client is
/// attached — bold cyan, deliberately distinct from plain `Green`), `Dim`
/// (stopped — gray/faint), `Red` (errored, dead, deleted), `Yellow`
/// (provisioning), `Plain` (unrecognised/future state — never fails closed
/// into a misleading color).
/// Test: `state_color_*` in `session_picker_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateColor {
    Green,
    AttachedCyan,
    Dim,
    Red,
    Yellow,
    Plain,
}

/// Map a [`StateColor`] to its raw ANSI SGR parameter, or `None` for `Plain`.
fn ansi_param(color: StateColor) -> Option<&'static str> {
    match color {
        StateColor::Green => Some("32"),
        StateColor::AttachedCyan => Some("1;36"),
        StateColor::Dim => Some("2"),
        StateColor::Red => Some("31"),
        StateColor::Yellow => Some("33"),
        StateColor::Plain => None,
    }
}

/// Wrap `text` in `color`'s ANSI escape when `use_color` is true; otherwise
/// return `text` unchanged.
///
/// Why: a single seam every colored fragment in this module goes through, so
/// the NO_COLOR/non-TTY fallback (Bob: "respect NO_COLOR / non-TTY by
/// falling back to plain text") is enforced in exactly one place.
/// What: `use_color && ansi_param(color).is_some()` → `"\x1b[{param}m{text}\x1b[0m"`;
/// otherwise `text.to_string()`.
/// Test: `colorize_wraps_when_color_enabled`, `colorize_plain_when_color_disabled`,
/// `colorize_plain_variant_never_wraps`.
pub(crate) fn colorize(text: &str, color: StateColor, use_color: bool) -> String {
    match (use_color, ansi_param(color)) {
        (true, Some(param)) => format!("\x1b[{param}m{text}\x1b[0m"),
        _ => text.to_string(),
    }
}

/// Resolve the color for a session's displayed state word.
///
/// Why: centralizes Bob's suggested mapping (active=green, attached=cyan/
/// bold-green, stopped=dim/gray, dead/errored=red, provisioning=yellow) so
/// [`format_session_row`]'s three call shapes (deleted/unresumable/normal)
/// never diverge on what a given state renders as.
/// What: `attached` wins over every `state` value (a client is connected
/// RIGHT NOW, the strongest signal); otherwise `"active"` → `Green`,
/// `"stopped"` → `Dim`, `"errored"` → `Red`, `"provisioning"` → `Yellow`,
/// anything else → `Plain` (never guesses a color for a state this mapping
/// doesn't recognise).
/// Test: `state_color_attached_wins_over_active`, `state_color_maps_known_states`,
/// `state_color_unknown_state_is_plain`.
pub(crate) fn state_color(state: &str, attached: bool) -> StateColor {
    if attached {
        return StateColor::AttachedCyan;
    }
    match state {
        "active" => StateColor::Green,
        "stopped" => StateColor::Dim,
        "errored" => StateColor::Red,
        "provisioning" => StateColor::Yellow,
        _ => StateColor::Plain,
    }
}

/// Decide whether the picker should emit color, resolved ONCE per invocation.
///
/// Why: the picker writes its menu via `eprintln!` (stderr), not `println!`
/// — probing `stdout`'s TTY-ness (as `colored`'s own env-based default does)
/// would be the WRONG stream. `should_show_picker` already requires stdin
/// AND stdout to be TTYs before the interactive picker ever runs, so a
/// non-TTY stderr here is the narrow remaining case (e.g. `2>file`) this
/// still needs to catch on its own.
/// What: `stderr_tty && NO_COLOR is unset` (any value, including empty,
/// per the NO_COLOR spec — https://no-color.org — disables color).
/// Test: `picker_use_color_*` in `session_picker_tests.rs`.
pub(crate) fn picker_use_color(stderr_tty: bool) -> bool {
    stderr_tty && std::env::var_os("NO_COLOR").is_none()
}

/// Format one verbless, color-coded picker menu row (issue #3723).
///
/// Why: replaces the old `[N] restart|resume <name> (state)` line — the verb
/// is dropped entirely (an implementation detail of *how* the session will be
/// reached, decided at selection time, not a property of its current state);
/// the state word itself now carries the signal via color, in addition to
/// its plain text (so a NO_COLOR/non-TTY reader loses nothing).
/// What: `sessions[idx].deleted` → the tombstone notice (`-- deleted --`,
/// colored red); `unresumable` → the DEAD notice, its state word colored red;
/// otherwise the normal row, `[N] <name> (<state>)` — `<state>` is
/// `"attached"` when a client is connected (else the raw `state` string),
/// colored via [`state_color`]. Never prints a verb in any branch.
/// Test: `format_session_row_normal_is_verbless`,
/// `format_session_row_attached_uses_attached_word`,
/// `format_session_row_deleted_notice`, `format_session_row_unresumable_notice`,
/// `format_session_row_color_disabled_is_plain_text`.
pub(crate) fn format_session_row(num: u32, s: &ManagedSessionSummary, use_color: bool) -> String {
    if s.deleted {
        let label = colorize("-- deleted --", StateColor::Red, use_color);
        return format!("tm:   [{num}] {label}");
    }
    if s.unresumable {
        let state = colorize(&s.state, StateColor::Red, use_color);
        return format!(
            "tm:   [{num}] {} ({state}) — DEAD: workspace removed; use [d{num}] to remove the record",
            s.name
        );
    }
    let shown_state = if s.attached {
        "attached"
    } else {
        s.state.as_str()
    };
    let color = state_color(&s.state, s.attached);
    let state = colorize(shown_state, color, use_color);
    format!("tm:   [{num}] {} ({state})", s.name)
}
