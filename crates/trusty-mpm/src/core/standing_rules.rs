//! The Architect's standing-rules block (#10960).
//!
//! Why: after a `/clear` and resume, an Architect session stopped applying the
//! owner's standing rules. They reached it as one line inside a ~98 KB state
//! file, which the model reads once and does not keep in view. A short block
//! the owner maintains, injected at every session start and on every user turn,
//! keeps those rules in resident context.
//! What: [`load`] reads `<project>/`[`STANDING_RULES_REL_PATH`] under the
//! [`STANDING_RULES_CAP_BYTES`] cap, which bounds the whole injected context
//! ([`STANDING_RULES_HEADER`] plus the rules), and returns a [`Load`];
//! [`hook_output`] turns it into the `SessionStart` / `UserPromptSubmit` hook
//! response. `tm hook --standing-rules` is the only caller; it is registered
//! for Architect sessions only (`session_launch::architect_hooks`).
//!
//! FAIL-OPEN: nothing here returns an error. An absent or blank file is a
//! no-op. An unreadable file (permissions, invalid UTF-8, a directory) is a
//! warning and injects nothing. A file over the cap injects its whole lines
//! plus an in-band cut notice, together within the cap, and warns the
//! operator; it is never cut silently, to the model or to the operator.
//! Test: `standing_rules_tests.rs`; the binary end to end in
//! `tests/tm_hook_standing_rules_10960.rs`.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The rules file, relative to the Architect project directory.
///
/// Why: the Architect keeps its owner-maintained, local-only files under
/// `local/` (`state.md`, `owner/`, `mandates/`); the rules belong beside them.
/// What: `local/standing-rules.md`, plain Markdown, injected verbatim.
pub const STANDING_RULES_REL_PATH: &str = "local/standing-rules.md";

/// The injected-size cap, in bytes (owner-approved fix, #10960): the whole
/// `additionalContext` — [`STANDING_RULES_HEADER`], the rules and any cut
/// notice — is never longer than this.
pub const STANDING_RULES_CAP_BYTES: usize = 2048;

/// The line [`hook_output`] puts ahead of the rules; it names
/// [`STANDING_RULES_REL_PATH`] and counts inside [`STANDING_RULES_CAP_BYTES`].
pub const STANDING_RULES_HEADER: &str = "Standing owner rules (local/standing-rules.md):\n";

/// The two hook events the block is injected on.
pub const STANDING_RULES_EVENTS: [&str; 2] = ["SessionStart", "UserPromptSubmit"];

/// What reading the rules file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    /// No file, or a file holding only whitespace: inject nothing, say nothing.
    Absent,
    /// Rules to inject; `warning` is set when the file was over the cap.
    Rules {
        /// The text injected after [`STANDING_RULES_HEADER`]: verbatim, or
        /// over the cap its whole lines plus the cut notice; header and text
        /// together are never longer than the cap.
        text: String,
        /// The over-cap warning the operator sees, if any.
        warning: Option<String>,
    },
    /// The file exists but could not be read; inject nothing, warn.
    Unreadable(String),
}

/// The rules file for an Architect project at `project_dir`.
pub fn rules_path(project_dir: &Path) -> PathBuf {
    project_dir.join(STANDING_RULES_REL_PATH)
}

/// Read the standing rules for `project_dir`.
///
/// Why: see the module doc.
/// What: [`load_from`] over [`rules_path`] with [`STANDING_RULES_CAP_BYTES`].
/// Test: `an_absent_file_is_a_silent_no_op`, `rules_under_the_cap_are_injected_verbatim`.
pub fn load(project_dir: &Path) -> Load {
    load_from(&rules_path(project_dir), STANDING_RULES_CAP_BYTES)
}

/// Read `path` under a `cap`-byte limit on the whole injected context.
///
/// Why: the hook runs on every user turn, so it reads a bounded prefix
/// whatever the file's size, and it must never hand the model a partial or
/// undecodable block. A cut block must say so in band, or the model reads an
/// incomplete rule set as the whole one (#10960, Architect cap ruling).
/// What: the text's room is `cap` minus [`STANDING_RULES_HEADER`] (#10960:
/// the header counts inside the cap); at most `room + 1` bytes are read.
/// `NotFound` or a whitespace-only file → [`Load::Absent`]. Any other
/// open/read error, or bytes that are not UTF-8 → [`Load::Unreadable`]. At most
/// `room` bytes → [`Load::Rules`] with the text verbatim and no notice. More
/// than `room` bytes → [`Load::Rules`] whose text is the whole lines that fit
/// in `room` minus the [`cut_notice`] length, followed by that notice, and a
/// warning naming the file, its size and the cap. A first line too long to fit
/// beside the notice yields the notice alone; a room smaller than the notice
/// yields an empty text (the operator warning still fires).
/// Postcondition: a `Rules` text, notice included, is never longer than
/// `room`, so header plus text is never longer than `cap` when the text is
/// non-empty.
/// Test: `an_over_cap_file_keeps_whole_lines_and_an_in_band_notice`,
/// `the_cap_boundary_is_verbatim_at_the_cap_and_noticed_one_byte_over`,
/// `an_over_long_first_line_yields_the_notice_alone`,
/// `an_over_cap_file_keeps_whole_lines_and_warns`,
/// `invalid_utf8_is_unreadable_and_injects_nothing`,
/// `a_directory_at_the_path_is_unreadable`, `a_blank_file_is_a_silent_no_op`.
pub fn load_from(path: &Path, cap: usize) -> Load {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Load::Absent,
        Err(err) => return unreadable(path, &err.to_string()),
    };
    let size = file.metadata().map(|m| m.len()).ok();
    let room = cap.saturating_sub(STANDING_RULES_HEADER.len());
    let mut bytes = Vec::with_capacity(room.saturating_add(1));
    // `room + 1` bytes are enough to tell "at the cap" from "over it".
    let limit = u64::try_from(room).unwrap_or(u64::MAX).saturating_add(1);
    if let Err(err) = file.take(limit).read_to_end(&mut bytes) {
        return unreadable(path, &err.to_string());
    }
    let over = bytes.len() > room;
    // #10960: the notice's bytes count inside the cap.
    let notice = over.then(|| cut_notice(path, cap));
    if let Some(notice) = &notice {
        // A newline is one ASCII byte, so cutting just after it is always a
        // UTF-8 boundary; no newline within the budget keeps no rule lines.
        let budget = room.saturating_sub(notice.len());
        let keep = bytes[..budget]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        bytes.truncate(keep);
    }
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => return unreadable(path, &format!("not valid UTF-8 ({})", err.utf8_error())),
    };
    if !over && text.trim().is_empty() {
        return Load::Absent;
    }
    let kept = text.len();
    if let Some(notice) = notice.filter(|n| n.len() <= room) {
        text.push_str(&notice);
    }
    let warning = over.then(|| {
        let size = size.map_or_else(|| "more than".to_owned(), |n| format!("{n}"));
        format!(
            "tm: standing rules {} is {size} bytes; with its header that is \
             over the {cap}-byte cap; injected {kept} bytes of whole lines plus \
             a cut notice and dropped the rest — shorten the file",
            path.display(),
        )
    });
    debug_assert!(
        text.len() <= room,
        "header plus injected text never exceeds the cap"
    );
    Load::Rules { text, warning }
}

/// The in-band notice appended to an over-cap block, naming `path` and `cap`.
///
/// Why: the model reading the block must know the rules are incomplete; the
/// operator warning alone never reaches it (#10960).
/// What: one bracketed line ending in a newline; its length counts inside
/// the cap in [`load_from`].
/// Test: `an_over_cap_file_keeps_whole_lines_and_an_in_band_notice`.
fn cut_notice(path: &Path, cap: usize) -> String {
    format!(
        "[tm: standing rules cut — {} does not fit the {cap}-byte cap; \
         the rules above are incomplete]\n",
        path.display()
    )
}

/// The [`Load::Unreadable`] warning for `path`.
fn unreadable(path: &Path, reason: &str) -> Load {
    Load::Unreadable(format!(
        "tm: standing rules {} could not be read ({reason}); no rules injected this turn",
        path.display()
    ))
}

/// The hook response for `load` on `event`, or `None` to print nothing.
///
/// Why: Claude Code adds `hookSpecificOutput.additionalContext` to the model's
/// context on `SessionStart` and `UserPromptSubmit`, and shows `systemMessage`
/// to the operator; a warning there is visible without blocking the turn.
/// What: an event outside [`STANDING_RULES_EVENTS`] or [`Load::Absent`] →
/// `None`. [`Load::Rules`] → `additionalContext` holding
/// [`STANDING_RULES_HEADER`] and the text verbatim (within the cap when the
/// text came from [`load_from`]), plus `systemMessage` when the load
/// warned (a text cut to nothing injects no context). [`Load::Unreadable`] →
/// `systemMessage` only. No response carries `decision` or `continue`, so
/// none can block the turn.
/// Test: `the_response_injects_on_both_events`,
/// `the_cap_boundary_is_verbatim_at_the_cap_and_noticed_one_byte_over`,
/// `an_unreadable_file_warns_and_injects_nothing`,
/// `an_unknown_event_prints_nothing`.
pub fn hook_output(event: &str, load: &Load) -> Option<Value> {
    if !STANDING_RULES_EVENTS.contains(&event) {
        return None;
    }
    match load {
        Load::Absent => None,
        Load::Unreadable(warning) => Some(json!({ "systemMessage": warning })),
        Load::Rules { text, warning } => {
            let mut out = serde_json::Map::new();
            if !text.trim().is_empty() {
                let context = format!("{STANDING_RULES_HEADER}{text}");
                out.insert(
                    "hookSpecificOutput".to_owned(),
                    json!({ "hookEventName": event, "additionalContext": context }),
                );
            }
            if let Some(warning) = warning {
                out.insert("systemMessage".to_owned(), Value::from(warning.as_str()));
            }
            (!out.is_empty()).then_some(Value::Object(out))
        }
    }
}

#[cfg(test)]
#[path = "standing_rules_tests.rs"]
mod tests;
