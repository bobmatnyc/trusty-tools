//! Compression-opportunity rows: what each tool result COULD have saved.
//!
//! Why (#10973): the `compress` ledger records only the Bash calls `tm hook`
//! actually rewrote. Subagent results, MCP output, Read/Grep/WebFetch and every
//! uncovered Bash call reach the model untouched and wrote no row at all, so
//! nobody could say how much compression was left on the table — or notice when
//! the rewrite stopped firing. An `opportunity` row is the denominator: one per
//! tool result, measured by a dry run of the existing compressor.
//! What: [`OpportunityRow`] is a [`crate::core::savings::SavingsRow`]-shaped
//! line (so every reader parses it) with `tokens_saved` and `cost_saved_usd`
//! pinned at zero plus the measurement columns. The ledger's row walk skips the
//! technique outright, so no fold — the `💸` per-call fold (#7867) included —
//! ever counts it as a saving. [`tool_type_of`] names the category a tool name
//! belongs to; the `compress` producer tags its rows with the same names.
//! Test: `an_opportunity_row_round_trips_and_never_folds`,
//! `tool_type_of_names_each_category`,
//! `a_tagged_compress_row_still_folds_as_a_saving`.

use std::path::Path;

use serde::Serialize;

use crate::core::savings::{BYTES_PER_TOKEN, SavingsRow, append_row};

/// `technique` value of an opportunity row (#10973).
///
/// Test: `an_opportunity_row_round_trips_and_never_folds`.
pub const TECHNIQUE_OPPORTUNITY: &str = "opportunity";

/// `tool_type` of a Bash call, on both `compress` and `opportunity` rows.
pub const TOOL_TYPE_BASH: &str = "bash";

/// One tool result's measured compression opportunity.
///
/// Why: the same file the statusline folds, so one ledger answers "what was
/// saved" and "what could have been". The SavingsRow columns keep older `tm`
/// binaries parsing the line; their zero values make an older binary that does
/// not know the technique reject it rather than count it.
/// What: `bytes_returned` is the result's full text size; `bytes_scanned` the
/// prefix the dry run read (capped by the caller); `would_compress_bytes` the
/// size the result would have had — the dry run's output for the scanned prefix
/// plus the unscanned remainder at full size, so a capped scan can only
/// understate the opportunity. `event` is the hook event that observed it:
/// a subagent's result is seen both as the `Agent` call's `PostToolUse` and as
/// its own `SubagentStop`, so an aggregate picks one event per question.
/// Test: `an_opportunity_row_round_trips_and_never_folds`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpportunityRow {
    /// RFC 3339 UTC timestamp.
    pub ts: String,
    /// Claude Code session id from the hook payload; empty when absent.
    pub session_id: String,
    /// Always [`TECHNIQUE_OPPORTUNITY`].
    pub technique: &'static str,
    /// Always zero: an opportunity is not a saving.
    pub tokens_saved: i64,
    /// Estimated tokens the result cost as returned.
    pub tokens_before: u64,
    /// Always zero: an opportunity is not a saving.
    pub cost_saved_usd: f64,
    /// How the figures were measured.
    pub basis: String,
    /// Empty: an opportunity row is not priced.
    pub model_source: &'static str,
    /// `PostToolUse` or `SubagentStop`.
    pub event: String,
    /// Category from [`tool_type_of`].
    pub tool_type: String,
    /// The compressor's dispatch name: the tool name, or for Bash the derived
    /// program and subcommand (`git diff`).
    pub tool_name: String,
    /// Full size of the result text.
    pub bytes_returned: u64,
    /// Bytes the dry run read.
    pub bytes_scanned: u64,
    /// Size the result would have had after compression.
    pub would_compress_bytes: u64,
}

impl OpportunityRow {
    /// Build a row from a measurement, stamping the constant columns.
    ///
    /// What: `tokens_before` is `bytes_returned` at [`BYTES_PER_TOKEN`], floored.
    pub fn new(
        event: &str,
        session_id: &str,
        tool_type: &str,
        tool_name: &str,
        bytes_returned: u64,
        bytes_scanned: u64,
        would_compress_bytes: u64,
    ) -> Self {
        Self {
            ts: crate::core::savings::now_ts(),
            session_id: session_id.to_string(),
            technique: TECHNIQUE_OPPORTUNITY,
            tokens_saved: 0,
            tokens_before: (bytes_returned as f64 / BYTES_PER_TOKEN).floor() as u64,
            cost_saved_usd: 0.0,
            basis: format!(
                "dry run of the tool-output compressor over {bytes_scanned} of \
                 {bytes_returned} bytes; unscanned bytes counted uncompressed"
            ),
            model_source: "",
            event: event.to_string(),
            tool_type: tool_type.to_string(),
            tool_name: tool_name.to_string(),
            bytes_returned,
            bytes_scanned,
            would_compress_bytes,
        }
    }
}

/// Append one opportunity row to `ledger`.
///
/// What: [`append_row`]'s single-write append; the IO error is the caller's.
pub fn append_opportunity(ledger: &Path, row: &OpportunityRow) -> std::io::Result<()> {
    append_row(ledger, row)
}

/// A `compress` savings row tagged with the Bash command it compressed.
///
/// Why (#10973): the per-tool breakdown the issue asks for needs the tool on
/// the row; flattening keeps every existing reader's [`SavingsRow`] parse.
#[derive(Debug, Serialize)]
pub struct TaggedSavingsRow<'a> {
    /// The saving itself, unchanged.
    #[serde(flatten)]
    pub row: &'a SavingsRow,
    /// [`TOOL_TYPE_BASH`] for a `compress` row.
    pub tool_type: &'a str,
    /// The `tm compress --tool` value, e.g. `git diff`.
    pub tool_name: &'a str,
}

/// The category a Claude Code tool name belongs to.
///
/// What: `mcp` for `mcp__<server>__<tool>`; `subagent` for `Agent`/`Task` and
/// for a `SubagentStop`; `bash` for `Bash`; otherwise the name lowercased
/// (`read`, `grep`, `webfetch`, …).
/// Test: `tool_type_of_names_each_category`.
pub fn tool_type_of(tool_name: &str) -> String {
    match tool_name {
        name if name.starts_with("mcp__") => "mcp".to_string(),
        "Agent" | "Task" | "SubagentStop" => "subagent".to_string(),
        "Bash" => TOOL_TYPE_BASH.to_string(),
        name => name.to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::savings::{
        fold_all, fold_session, fold_session_per_call, savings_log_in, session_row_percents,
    };

    #[test]
    fn tool_type_of_names_each_category() {
        assert_eq!(tool_type_of("mcp__trusty-search__search"), "mcp");
        assert_eq!(tool_type_of("Agent"), "subagent");
        assert_eq!(tool_type_of("Task"), "subagent");
        assert_eq!(tool_type_of("SubagentStop"), "subagent");
        assert_eq!(tool_type_of("Bash"), "bash");
        assert_eq!(tool_type_of("WebFetch"), "webfetch");
        assert_eq!(tool_type_of("Read"), "read");
    }

    /// #10973: written, parsed by every reader, folded by none.
    #[test]
    fn an_opportunity_row_round_trips_and_never_folds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = savings_log_in(dir.path());
        let row = OpportunityRow::new("PostToolUse", "s1", "read", "Read", 40_000, 40_000, 9_000);
        append_opportunity(&ledger, &row).expect("append");
        let text = std::fs::read_to_string(&ledger).expect("read");
        let parsed: SavingsRow = serde_json::from_str(text.trim()).expect("a SavingsRow parse");
        assert_eq!(parsed.technique, TECHNIQUE_OPPORTUNITY);
        let value: serde_json::Value = serde_json::from_str(text.trim()).expect("json");
        assert_eq!(value["would_compress_bytes"], 9_000);
        assert_eq!(value["tool_type"], "read");
        assert!(fold_session(&ledger, "s1").is_zero());
        assert!(fold_session_per_call(&ledger, "s1").is_zero());
        assert!(fold_all(&ledger).is_zero());
        assert!(session_row_percents(&ledger, "s1").is_empty());
    }

    #[test]
    fn a_tagged_compress_row_still_folds_as_a_saving() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = savings_log_in(dir.path());
        let row = SavingsRow {
            ts: "2026-11-10T00:00:00Z".to_string(),
            session_id: "s1".to_string(),
            technique: crate::core::savings::TECHNIQUE_COMPRESS.to_string(),
            tokens_saved: 900,
            tokens_before: 1_000,
            cost_saved_usd: 0.01,
            basis: "b".to_string(),
            model_source: String::new(),
        };
        let tagged = TaggedSavingsRow {
            row: &row,
            tool_type: TOOL_TYPE_BASH,
            tool_name: "git diff",
        };
        append_row(&ledger, &tagged).expect("append");
        assert_eq!(fold_session_per_call(&ledger, "s1").tokens_saved, 900);
    }
}
