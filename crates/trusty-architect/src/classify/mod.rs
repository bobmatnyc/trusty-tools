//! Pane parsing: the Rust port of the classifier inline in the prototype's
//! `fleet-poll.py`.
//!
//! Why: the Architect's poller decides from one `tmux capture-pane -e` per PM
//! pane whether that PM is idle, asking, blocked on a dialog, near its context
//! limit or erroring. #9171 A2 ports that pure half first, held to
//! byte-identical JSON against the classifier the live host runs: the inline
//! copy in `<prototype-dir>/fleet-poll.py`, not the stale `fleet-classify.py`.
//! What: [`parse_pane`] turns a capture into [`PaneInfo`]; [`alerts`] turns
//! facts into alerts; [`dedup()`] splits alerts into new and suppressed;
//! [`input_state_for_send`] is the fail-closed reading `send_guarded` takes.
//! No tmux call, file read or clock read happens here: `now` is passed in.
//! Test: `tests/classify_ported.rs` (the prototype's classify tests that pass
//! against `fleet-poll.py`, by name) and `tests/parity.rs` (the golden corpus).

pub mod alerts;
pub mod dedup;
mod guard;
pub(crate) mod matchers;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use serde::{Serialize, Serializer};

use crate::input::{InputState, classify_input};
use crate::pystr::{is_space, lstrip, splitlines, strip};
use matchers::{
    account_field, find_pct, has_dialog_marker, has_waiting, is_active, is_agent_row,
    is_box_top_rule, is_completed, is_error, is_footer_noise, is_limit, is_option, is_rc_failed,
    is_rule, is_spinner, strip_ansi, strip_timers,
};

pub use alerts::{
    Alert, CTX_HIGH, CTX_WARN, DETAIL_MAX, IDLE_AFTER, PLAN_HIGH, PaneState, alert, evaluate,
    fleet_plan_alerts,
};
pub use dedup::{DedupState, REEMIT, Seen, dedup};
pub use guard::input_state_for_send;

/// What `parse_pane` reports in its `box` field.
///
/// Why: `fleet-poll.py` writes the string `"dialog"` there when no input
/// box is found but a dialog is open, which is not an [`InputState`].
/// What: the input-box state, or `Dialog`; serializes as the Python string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxReading {
    /// The input-box classifier's reading.
    Input(InputState),
    /// No input box, but an open dialog.
    Dialog,
}

impl BoxReading {
    /// The string `fleet-poll.py` stores for this reading.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input(s) => s.as_str(),
            Self::Dialog => "dialog",
        }
    }
}

impl Serialize for BoxReading {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// The facts `parse_pane` reads off one capture, in `fleet-poll.py`'s key
/// order so [`crate::to_python_json`] reproduces its dict byte for byte.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaneInfo {
    /// Input-box reading (see [`BoxReading`]).
    #[serde(rename = "box")]
    pub box_reading: BoxReading,
    /// The input line's text with escapes removed (`input-state.py`).
    pub box_text: String,
    /// True when a rule-bounded input box was found.
    pub has_box: bool,
    /// `(fingerprint text, detail)` of an open dialog below the box.
    pub dialog: Option<(String, String)>,
    /// The latest turn's spinner/status line, or `""`.
    pub status: String,
    /// Running background-agent rows (with a timer) anywhere on screen.
    pub agents: usize,
    /// Agents running, waiting on agents, or a turn in progress.
    pub busy: bool,
    /// The current turn is spinning or waiting on background agents.
    pub turn_active: bool,
    /// `ctx N%` from the status line.
    pub ctx: Option<u64>,
    /// The 5-hour plan usage percent.
    pub plan5h: Option<u64>,
    /// The weekly plan usage percent.
    pub planwk: Option<u64>,
    /// The PM's last line when it ends in `?` and the pane is idle.
    pub question: Option<String>,
    /// The status line's `✻<account>` field, when the line is not truncated.
    pub account: Option<String>,
    /// `/rc failed` (Remote Control dropped) in the last 12 rows.
    pub rc_failed: bool,
    /// The newest usage-limit notice in the last 30 rows.
    pub limit: Option<String>,
    /// Distinct error lines in the last 30 rows, sorted.
    pub errors: Vec<String>,
}

/// Escape-stripped, right-trimmed screen rows, trailing blank rows dropped.
pub(crate) fn screen_lines(capture: &str) -> Vec<String> {
    let mut lines: Vec<String> = splitlines(capture)
        .into_iter()
        .map(|l| crate::pystr::rstrip(&strip_ansi(l)).to_owned())
        .collect();
    while lines.last().is_some_and(|l| strip(l).is_empty()) {
        lines.pop();
    }
    lines
}

/// `(top_rule, bottom_rule)` row indexes of the input box: the last `❯` row
/// under a (possibly labeled) rule that is not a dialog's option cursor.
pub(crate) fn find_box(lines: &[String]) -> Option<(usize, usize)> {
    (1..lines.len()).rev().find_map(|i| {
        let l = &lines[i];
        if !(lstrip(l).starts_with('❯') && !is_option(l) && is_box_top_rule(&lines[i - 1])) {
            return None;
        }
        let bottom = (i + 1..lines.len())
            .find(|&j| is_rule(&lines[j]))
            .unwrap_or(lines.len() - 1);
        Some((i - 1, bottom))
    })
}

fn first_char_is_space(l: &str) -> bool {
    l.chars().next().is_some_and(is_space)
}

/// An open dialog in the last 25 rows below the box: `(norm, detail)`.
fn find_dialog(lines: &[String], bottom: Option<usize>) -> Option<(String, String)> {
    let mut start = lines.len().saturating_sub(25);
    if let Some(b) = bottom {
        start = start.max(b + 1);
    }
    let region = &lines[start.min(lines.len())..];
    let m = region
        .iter()
        .rposition(|l| is_option(l) || has_dialog_marker(l))?;
    let window = &region[m.saturating_sub(20)..=m];
    let body: Vec<&str> = window
        .iter()
        .filter(|l| !strip(l).is_empty() && !is_footer_noise(l) && !is_rule(l))
        .map(|l| strip(l))
        .collect();
    // The cursor position and timers change while the dialog itself does not.
    let norm = body
        .iter()
        .map(|l| strip(&strip_timers(&l.replace('❯', " "))).to_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let header = window.iter().position(|l| is_rule(l)).and_then(|r| {
        window[r + 1..]
            .iter()
            .map(|l| strip(l))
            .find(|l| !l.is_empty())
    });
    let ask = body.iter().rev().find(|l| l.ends_with('?')).copied();
    let mut parts: Vec<&str> = Vec::new();
    for p in [header, ask].into_iter().flatten() {
        if !p.is_empty() && !parts.contains(&p) {
            parts.push(p);
        }
    }
    // #9171: Python raises IndexError when `parts` and `body` are both empty
    // (every row is footer noise); the port keeps the dialog with no detail.
    let detail = if parts.is_empty() {
        body.first().copied().unwrap_or_default().to_owned()
    } else {
        parts.join(" | ")
    };
    Some((norm, detail))
}

/// The latest turn's spinner/status line (e.g. `✻ Worked for 3m`), or `""`.
fn current_status(lines: &[String], top: usize) -> String {
    lines[..top]
        .iter()
        .rev()
        .find(|l| !strip(l).is_empty() && !first_char_is_space(l))
        .filter(|l| is_spinner(l))
        .cloned()
        .unwrap_or_default()
}

/// True when the newest `Waiting for N background agent` row is of the current turn.
///
/// Why: the capture carries scrollback, so a stale waiting row made an idle
/// pane read busy (#9171: in `fleet-poll.py`, absent from `fleet-classify.py`).
/// What: the newest waiting row above the box counts only when no finished-turn
/// summary and no submitted `❯` row follows it.
/// Test: `test_stale_waiting_lines_are_not_busy`, `test_newest_waiting_line_is_busy`.
fn current_waiting(lines: &[String], top: Option<usize>) -> bool {
    let end = top.unwrap_or(lines.len());
    let Some(w) = lines[..end].iter().rposition(|l| has_waiting(l)) else {
        return false;
    };
    !lines[w + 1..end]
        .iter()
        .any(|l| is_completed(l) || lstrip(l).starts_with('❯'))
}

/// The last line of the latest PM turn when it ends in `?`.
fn last_question(lines: &[String], top: usize) -> Option<String> {
    let mut last: Option<String> = None;
    let mut seen_status = false;
    for l in lines[..top].iter().rev() {
        if strip(l).is_empty() {
            continue;
        }
        if first_char_is_space(l) {
            if last.is_none() {
                last = Some(strip(l).to_owned());
            }
            continue;
        }
        if is_spinner(l) && !seen_status {
            seen_status = true;
            last = None;
            continue;
        }
        if let Some(rest) = l.strip_prefix('⏺') {
            let q = last.unwrap_or_else(|| strip(rest).to_owned());
            return q.ends_with('?').then_some(q);
        }
        return None; // the latest turn is the user's (❯) or unrecognised
    }
    None
}

/// Classifies one capture taken with escape codes. Pure; no tmux calls.
///
/// Why: every alert the poller raises starts from these facts.
/// What: `fleet-poll.py`'s inline `parse_pane`, field for field.
/// Test: `test_input_box_classifier_is_input_states`,
/// `test_labeled_top_rule_is_still_recognized_as_the_box`,
/// `test_agent_rows_are_not_idle_and_reset_timer`, `test_rc_marker_detected`,
/// `test_account_field_parsed_and_truncation_ignored`,
/// `parse_pane_matches_python_goldens`.
pub fn parse_pane(capture: &str) -> PaneInfo {
    let lines = screen_lines(capture);
    let (box_state, box_text) = classify_input(capture);
    let found = find_box(&lines);
    let (top, bottom) = (found.map(|b| b.0), found.map(|b| b.1));
    let footer: &[String] = match bottom {
        Some(b) => &lines[b + 1..],
        None => &lines[lines.len().saturating_sub(10)..],
    };
    let dialog = find_dialog(&lines, bottom);
    let status = top.map(|t| current_status(&lines, t)).unwrap_or_default();
    // #9171: fleet-poll.py scans the whole capture, not the footer: a box with
    // no bottom rule leaves the footer empty and the pane read idle.
    let agents = lines.iter().filter(|l| is_agent_row(l)).count();
    let waiting = has_waiting(&status) || current_waiting(&lines, top);
    let spinning = !status.is_empty() && is_active(&status);
    let busy = agents > 0 || waiting || spinning;
    let ftext = footer.join("\n");
    let tail = &lines[lines.len().saturating_sub(30)..];
    let question = match top {
        Some(t) if !busy && dialog.is_none() => last_question(&lines, t),
        _ => None,
    };
    let box_reading = match (found, &dialog) {
        (None, Some(_)) => BoxReading::Dialog,
        _ => BoxReading::Input(box_state),
    };
    PaneInfo {
        box_reading,
        box_text,
        has_box: found.is_some(),
        dialog,
        status,
        agents,
        busy,
        turn_active: spinning || waiting,
        ctx: find_pct(&ftext, "ctx ", true, false),
        plan5h: find_pct(&ftext, "⏳", false, true),
        planwk: find_pct(&ftext, "📅", false, true),
        question,
        account: account_field(&ftext),
        rc_failed: lines[lines.len().saturating_sub(12)..]
            .iter()
            .any(|l| is_rc_failed(l)),
        limit: tail
            .iter()
            .rev()
            .find(|l| is_limit(l))
            .map(|l| strip(l).to_owned()),
        errors: tail
            .iter()
            .filter(|l| is_error(l))
            .map(|l| strip(l).to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}
