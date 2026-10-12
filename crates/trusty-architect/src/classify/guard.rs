//! The fail-closed input-box reading that [`crate::send_guarded`] consumes.

use super::matchers::{ansi_len, is_rule};
use super::{find_box, find_dialog, screen_lines};
use crate::input::{InputState, classify_input};
use crate::pystr::{is_space, splitlines, strip};

const ESC: char = '\u{1b}';

/// The input-box state to hand [`crate::send_guarded`] for one capture.
///
/// Why: `input-state.py`'s classifier reads the last `❯` row anywhere on
/// screen, so a stale `❯` above a busy turn reads `empty`, and a draft that
/// wraps past its first row shows only that row. It also calls a row a
/// `suggestion` when any dim span appears on it, so typed text next to a dim
/// hint reads as a suggestion. The prototype's `wake()` covers the first two
/// with separate checks; the send path here must not depend on a caller
/// remembering them, so this reading fails closed (#9171 A2).
/// What: returns `Empty` or `Suggestion` only when all hold — no open dialog;
/// a box bounded by a top and a bottom rule; [`classify_input`] reads
/// `empty`/`suggestion` from the box's own `❯` row; every wrapped row inside
/// the box is blank (`Empty`); and, for `Suggestion`, every visible character
/// on the `❯` row and on each wrapped row is printed while dim is on (see
/// [`all_visible_dim`]). Otherwise it returns `NoPrompt` (no readable box) or
/// `Typed` (the box holds text it cannot prove is a suggestion).
/// Test: `unrecognised_captures_never_accept_input`,
/// `test_wake_detects_its_own_stale_pointer_and_does_not_send`.
pub fn input_state_for_send(capture: &str) -> InputState {
    let lines = screen_lines(capture);
    let Some((top, bottom)) = find_box(&lines) else {
        return InputState::NoPrompt;
    };
    if find_dialog(&lines, Some(bottom)).is_some() || !is_rule(&lines[bottom]) {
        return InputState::NoPrompt;
    }
    let (state, _) = classify_input(capture);
    if !state.accepts_input() {
        return state;
    }
    let raw = splitlines(capture);
    // The classifier must have read the box's own row, not another ❯ row.
    if raw.iter().rposition(|l| l.contains('❯')) != Some(top + 1) {
        return InputState::NoPrompt;
    }
    // #9171: classify_input's Suggestion only means "a dim span somewhere on
    // the row"; the send path requires the whole visible text to be dim.
    let prompt_row = raw[top + 1].split_once('❯').map_or("", |(_, r)| r);
    if state == InputState::Suggestion && !all_visible_dim(prompt_row) {
        return InputState::Typed;
    }
    for row in top + 2..bottom {
        if strip(&lines[row]).is_empty() {
            continue;
        }
        if state == InputState::Empty || !all_visible_dim(raw[row]) {
            return InputState::Typed;
        }
    }
    state
}

/// True when every visible non-space character of `row` is printed while dim
/// is on.
///
/// Why: a Claude Code suggestion is drawn wholly dim; typed text is not, even
/// when a dim argument hint follows it on the same row (#9171).
/// What: walks the raw row with dim off at its start. SGR `2` sets dim; `0`,
/// an empty parameter (or an empty `ESC[m`) and `22` clear it; the colour
/// arguments of `38`/`48`/`58` are skipped so `38;5;2` is not read as dim.
/// Other escape sequences are skipped. An escape it cannot parse, or an
/// `m`-final sequence with non-numeric parameters, fails closed (`false`).
/// Test: `unrecognised_captures_never_accept_input`.
fn all_visible_dim(row: &str) -> bool {
    let s: Vec<char> = row.chars().collect();
    let mut dim = false;
    let mut i = 0;
    while i < s.len() {
        if s[i] == ESC {
            let Some(n) = ansi_len(&s[i..]) else {
                return false;
            };
            let seq = &s[i..i + n];
            if seq.get(1) == Some(&'[') && seq.last() == Some(&'m') {
                let params: String = seq[2..n - 1].iter().collect();
                if !params.chars().all(|c| c.is_ascii_digit() || c == ';') {
                    return false;
                }
                apply_sgr(&params, &mut dim);
            }
            i += n;
            continue;
        }
        if !dim && !is_space(s[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// Applies one SGR parameter list (digits and `;` only) to the dim flag.
fn apply_sgr(params: &str, dim: &mut bool) {
    let codes: Vec<u32> = params
        .split(';')
        // An empty or overlong parameter reads as 0, which clears dim.
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let mut k = 0;
    while k < codes.len() {
        match codes[k] {
            0 | 22 => *dim = false,
            2 => *dim = true,
            38 | 48 | 58 => {
                k += match codes.get(k + 1) {
                    Some(5) => 2,
                    Some(2) => 4,
                    _ => 0,
                };
            }
            _ => {}
        }
        k += 1;
    }
}
