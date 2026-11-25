//! `tm savings` command group — read the tool-output savings ledgers (#11033).
//!
//! Why: `compress` and opportunity rows were written but never shown, so the
//! owner could not see how much tool output was compressed.
//! What: [`SavingsAction`] — currently just `status`.
//! Test: `cli_parses_savings_status` in `divert_cli_tests.rs`.

use clap::{Subcommand, ValueEnum};

/// Grouping for `tm savings status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SavingsGroupArg {
    /// Group by tool type (bash, mcp, read, …).
    Tool,
    /// Group by event class (SubagentStop, Agent PostToolUse, other) with a
    /// per-group session count. Rows carry no subagent type yet.
    Subagent,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SavingsAction {
    /// Per-day tool-output tokens seen, compressed and saved, and why the rest
    /// was skipped.
    ///
    /// Reads `usage/savings.jsonl` (`compress` rows) and
    /// `usage/opportunity/<day>.jsonl`. Tokens are bytes/4. The would-save
    /// column is an upper bound: opportunity rows are a dry run. A day whose
    /// `<day>.dropped` marker exists is flagged UNDERCOUNTED. `--by subagent`
    /// groups by event (SubagentStop, Agent PostToolUse) and counts sessions
    /// per group, because rows carry no subagent type today.
    Status {
        /// How to group each day's rows.
        #[arg(long, value_enum, default_value_t = SavingsGroupArg::Tool)]
        by: SavingsGroupArg,
        /// Days to show, ending today (UTC).
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}
