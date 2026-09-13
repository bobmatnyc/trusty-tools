//! The prompt-feedback ledger: extract, append, read back (#7688).
//!
//! Why: the addendum
//! [`prompt_self_improvement`](crate::core::prompt_self_improvement) asks for is
//! worth nothing if it dies in a transcript nobody reads. This module is the
//! durable side — one append-only JSON-Lines file that a `Stop`/`SubagentStop`
//! hook writes and `tm prompt-feedback` reads. JSON Lines rather than a
//! database for the reason the savings ledger chose it: an append is one
//! `write`, concurrent writers from several sessions interleave whole lines
//! rather than corrupting a shared structure, and a malformed line costs that
//! line alone.
//!
//! What: [`extract_feedback`] pulls the `## Prompt feedback` section out of a
//! final assistant message; [`append_row`] writes one [`FeedbackRow`];
//! [`read_rows`] reads them newest-first with the filters the CLI exposes; and
//! [`summarize`] groups them by agent type.
//!
//! 🔴 EVERY WRITE PATH HERE FAILS OPEN. A hook that cannot read a transcript or
//! cannot write this ledger logs a `warn` and returns `Ok`-shaped nothing —
//! never an error that could reach Claude Code's hook exit status. A prompt
//! critique is not worth blocking a session over, and a non-zero `Stop` hook is
//! visible to the operator as a broken session. The arm is pinned by
//! `append_row_on_an_unwritable_ledger_is_not_an_error`.
//!
//! Test: `prompt_feedback_tests.rs`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::prompt_self_improvement::FEEDBACK_HEADING;

/// Basename of the ledger under the framework root.
///
/// What: `prompt-feedback.jsonl`, i.e. `~/.trusty-mpm/prompt-feedback.jsonl`.
/// Test: `ledger_path_is_a_root_level_jsonl_file`.
pub const LEDGER_FILE: &str = "prompt-feedback.jsonl";

/// The `agent_type` recorded for the PM's own `Stop` event.
///
/// Why: a `Stop` carries no `agent_type` — it is the main session ending — so
/// the column would be null for exactly the rows an operator most wants to
/// group. A fixed sentinel keeps [`summarize`] a single grouping over one
/// non-null column.
///
/// 🔴 A `Stop`, and ONLY a `Stop`. An absent `agent_type` on a `SubagentStop`
/// is not evidence that the PM spoke — see [`UNKNOWN_SUBAGENT_TYPE`].
/// What: `"pm"`.
/// Test: `a_stop_payload_writes_a_pm_row`.
pub const PM_AGENT_TYPE: &str = "pm";

/// The `agent_type` recorded for a subagent stop that named no type (#7702).
///
/// Why: `SubagentStop` normally carries `agent_type`, but the daemon's own
/// delegation tracker already treats a stop without one as evidence about
/// nothing rather than as the PM's
/// (`a_stop_without_an_agent_type_reconciles_nothing`). Folding such a row into
/// [`PM_AGENT_TYPE`] would book a subagent's critique against the PM and
/// corrupt [`summarize`], whose entire job is naming which agent type
/// complains most. A distinct sentinel keeps the count honest and stays
/// visible in the output as a thing to investigate.
/// What: `"unknown-subagent"`.
/// Test: `a_subagent_stop_without_an_agent_type_is_not_the_pm`.
pub const UNKNOWN_SUBAGENT_TYPE: &str = "unknown-subagent";

/// How many bytes of one feedback section are stored.
///
/// Why: the addendum is specified as at most five lines, but nothing enforces
/// that on the model, and an unbounded capture would let one runaway response
/// write a megabyte into a file every later read must stream. The cap is far
/// above any honest five lines, so a well-formed row is never touched.
/// What: 4 KiB, applied on a char boundary so the stored text stays valid UTF-8.
/// Test: `an_oversized_feedback_section_is_truncated`.
const MAX_FEEDBACK_BYTES: usize = 4096;

/// One captured prompt critique.
///
/// Why: the row is the whole product of this feature, so its shape is the
/// contract the follow-up optimization pass reads. Every field is either
/// present or explicitly `null`; none is inferred later.
/// What: `ts` is RFC 3339 UTC; `session_id` is Claude Code's own; `agent_type`
/// is the subagent's type or [`PM_AGENT_TYPE`]; `prompt_digest` is a sha256 of
/// the composed prompt when one could be read, else `null`; `feedback` is the
/// extracted section body.
/// Test: `append_then_read_round_trips_a_row`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackRow {
    /// When the hook fired, RFC 3339 UTC.
    pub ts: String,
    /// Claude Code's session id, or `null` when the payload carried none.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The subagent's type, [`PM_AGENT_TYPE`] for the session itself, or
    /// [`UNKNOWN_SUBAGENT_TYPE`] for a subagent stop that named none.
    pub agent_type: String,
    /// sha256 of the composed prompt this feedback is about, when known.
    #[serde(default)]
    pub prompt_digest: Option<String>,
    /// The extracted `## Prompt feedback` body.
    pub feedback: String,
}

/// Path of the ledger under `framework_root`.
///
/// Why: taken as an argument rather than resolved from `$HOME` so a test names
/// a tempdir instead of writing the operator's real ledger — and without the
/// PROCESS-GLOBAL `$HOME` write #5544 forbids in this binary.
/// What: `<framework_root>/prompt-feedback.jsonl`.
/// Test: `ledger_path_is_a_root_level_jsonl_file`.
pub fn ledger_path(framework_root: &Path) -> PathBuf {
    framework_root.join(LEDGER_FILE)
}

/// Pull the `## Prompt feedback` section out of a final assistant message.
///
/// Why: the model is asked to end its response with this section, but it ends a
/// response that also contains the actual work — so the extractor must find the
/// heading rather than assume the whole message is feedback. It stops at the
/// next same-or-higher-level Markdown heading, because the addendum is
/// specified as the LAST thing in a response and anything after a following
/// `##` belongs to something else.
///
/// What: `Some(body)` for the LAST occurrence of [`FEEDBACK_HEADING`] at the
/// start of a line, trimmed, and truncated to [`MAX_FEEDBACK_BYTES`] on a char
/// boundary. `None` when the heading is absent or its body is empty — a
/// response that did not answer is not a row.
///
/// The LAST occurrence, not the first: a response that quotes the instruction
/// ("end with a `## Prompt feedback` addendum") before complying would
/// otherwise capture the quote and stop at the real section's heading.
/// Test: `extracts_the_section_body`, `extracts_the_last_section`,
/// `stops_at_the_next_heading`, `absent_heading_extracts_nothing`,
/// `an_empty_section_extracts_nothing`,
/// `an_oversized_feedback_section_is_truncated`.
pub fn extract_feedback(message: &str) -> Option<String> {
    let start = last_heading_offset(message)?;
    let after = &message[start + FEEDBACK_HEADING.len()..];

    let mut body = String::new();
    for line in after.lines().skip_while(|l| l.trim().is_empty()) {
        // A same-or-higher-level heading ends the section. `###` is deeper, so
        // it is part of the feedback and does not terminate it.
        let trimmed = line.trim_start();
        if (trimmed.starts_with("## ") && !trimmed.starts_with("### ")) || trimmed.starts_with("# ")
        {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }

    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    Some(truncate_on_char_boundary(body, MAX_FEEDBACK_BYTES).to_string())
}

/// Byte offset of the last line-initial [`FEEDBACK_HEADING`] in `message`.
///
/// What: `None` when the heading never starts a line. Requires the heading to
/// begin a line so a mention inside a sentence is not mistaken for the section.
/// Test: via [`extract_feedback`]'s tests.
fn last_heading_offset(message: &str) -> Option<usize> {
    let mut found = None;
    let mut offset = 0;
    for line in message.split_inclusive('\n') {
        let indent = line.len() - line.trim_start().len();
        // The line must BE the heading — not merely contain it — so a sentence
        // quoting the instruction never opens a section.
        if line.trim() == FEEDBACK_HEADING {
            found = Some(offset + indent);
        }
        offset += line.len();
    }
    found
}

/// Truncate `s` to at most `max` bytes without splitting a character.
///
/// Test: `an_oversized_feedback_section_is_truncated`.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Append one row to the ledger under `framework_root`.
///
/// Why: the hook's only write. It is `O_APPEND` on one open file handle, which
/// on every platform tm supports makes a single `write` of a line smaller than
/// the pipe buffer atomic against other appenders — so two sessions ending at
/// once interleave whole lines rather than shredding each other's.
///
/// 🔴 FAILS OPEN. Returns `false` and logs a `warn` when the directory cannot
/// be created, the file cannot be opened, or the write fails. It NEVER returns
/// an error, because its one caller is a Claude Code hook whose non-zero exit
/// the operator sees as a broken session.
/// What: serialises `row` as one JSON line. Returns whether it landed.
/// Test: `append_then_read_round_trips_a_row`,
/// `append_row_on_an_unwritable_ledger_is_not_an_error`.
pub fn append_row(framework_root: &Path, row: &FeedbackRow) -> bool {
    let Ok(line) = serde_json::to_string(row) else {
        tracing::warn!("prompt-feedback row could not be serialised; dropping it");
        return false;
    };
    if let Err(err) = std::fs::create_dir_all(framework_root) {
        tracing::warn!(
            "prompt-feedback ledger directory {} unavailable: {err}; dropping the row",
            framework_root.display()
        );
        return false;
    }
    let path = ledger_path(framework_root);
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    match opened {
        Ok(mut file) => match writeln!(file, "{line}") {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(
                    "prompt-feedback ledger {} could not be appended to: {err}; dropping the row",
                    path.display()
                );
                false
            }
        },
        Err(err) => {
            tracing::warn!(
                "prompt-feedback ledger {} could not be opened: {err}; dropping the row",
                path.display()
            );
            false
        }
    }
}

/// Filters `tm prompt-feedback` applies to a ledger read.
///
/// What: every field is optional and ANDed; an all-`None` filter matches every
/// row. `limit` caps the NEWEST rows returned, after the other filters.
/// Test: `read_rows_filters_by_session`, `read_rows_filters_by_agent`,
/// `read_rows_applies_the_limit_after_filtering`.
#[derive(Debug, Clone, Default)]
pub struct ReadFilter {
    /// Keep only rows from this session id, or from one linked to it — see
    /// `session_and_siblings` (private, so a plain span rather than a link).
    pub session: Option<String>,
    /// Keep only rows from this agent type.
    pub agent: Option<String>,
    /// Keep at most this many of the newest matching rows.
    pub limit: Option<usize>,
}

/// Read the ledger newest-first, applying `filter`.
///
/// Why: newest-first because the question an operator asks of this file is
/// always "what did the last few runs say", never "what did it say in March".
/// What: parses every line, SKIPPING one that does not deserialise with a
/// `warn` — a partial line from a crashed writer must not hide the rest of the
/// file. An absent ledger reads as an empty list, not an error: nothing has
/// been captured yet is a normal state.
///
/// `--session` folds linked ids (#7702): see `session_and_siblings` below.
/// Test: `append_then_read_round_trips_a_row`, `read_rows_is_newest_first`,
/// `read_rows_skips_a_malformed_line`, `read_of_an_absent_ledger_is_empty`,
/// `read_rows_folds_a_linked_sibling_session`.
pub fn read_rows(framework_root: &Path, filter: &ReadFilter) -> Vec<FeedbackRow> {
    let path = ledger_path(framework_root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(
                "prompt-feedback ledger {} could not be read: {err}",
                path.display()
            );
            return Vec::new();
        }
    };

    // Resolved once, before the scan: the link store is a directory read, and
    // doing it per row would turn one small scan into one per line.
    let sessions = filter
        .session
        .as_deref()
        .map(|want| session_and_siblings(framework_root, want));

    let mut rows: Vec<FeedbackRow> = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| match serde_json::from_str::<FeedbackRow>(line) {
            Ok(row) => Some(row),
            Err(err) => {
                tracing::warn!("skipping a malformed prompt-feedback line: {err}");
                None
            }
        })
        .filter(|row| {
            sessions.as_ref().is_none_or(|want| {
                row.session_id
                    .as_deref()
                    .is_some_and(|id| want.iter().any(|w| w == id))
            })
        })
        .filter(|row| {
            filter
                .agent
                .as_ref()
                .is_none_or(|want| &row.agent_type == want)
        })
        .collect();

    rows.reverse();
    if let Some(limit) = filter.limit {
        rows.truncate(limit);
    }
    rows
}

/// Every session id `--session <id>` accepts: `session` and its linked siblings.
///
/// Why (#7702, the #7617 lesson): Claude Code mints a NEW `session_id` on every
/// restart — a relaunch, a `/login` switch, a crash — so an operator who reads
/// back the id their session is running under today gets an EMPTY result for
/// feedback the same managed session captured an hour ago, with nothing saying
/// rows were excluded. That is exactly how the `💸` segment disappeared in
/// #7617, and the fix is the same fix: fold across the managed session's ids.
///
/// ONE IMPLEMENTATION. The sibling lookup is
/// [`linked_claude_ids`](crate::core::session_links::linked_claude_ids), the
/// same call the statusline fold makes; nothing here re-derives a link. Where
/// the statusline takes the FIRST sibling with rows — it renders one number —
/// this returns the union, because a listing shows every matching row.
/// What: the ids linked to `session`, which already include `session` itself;
/// `[session]` alone when nothing links it, which is every unmanaged session
/// and every read failure. Never empty, so an unlinked id still matches its own
/// rows.
/// Test: `read_rows_folds_a_linked_sibling_session`,
/// `read_rows_filters_by_session`.
fn session_and_siblings(framework_root: &Path, session: &str) -> Vec<String> {
    let mut ids = crate::core::session_links::linked_claude_ids(framework_root, session);
    if !ids.iter().any(|id| id == session) {
        ids.push(session.to_string());
    }
    ids
}

/// Row counts per agent type, most frequent first.
///
/// Why: the `--summary` read. "Which agent type produces the most prompt
/// complaints" is the one question that points at which prompt to fix next.
/// What: `(agent_type, count)` descending by count, then by name so the output
/// is stable for equal counts.
/// Test: `summarize_counts_by_agent_type`, `summarize_orders_by_count_then_name`.
pub fn summarize(rows: &[FeedbackRow]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for row in rows {
        *counts.entry(row.agent_type.clone()).or_insert(0) += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
#[path = "prompt_feedback_tests.rs"]
mod tests;
