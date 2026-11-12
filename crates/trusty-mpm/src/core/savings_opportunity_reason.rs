//! Why an opportunity row's result was not compressed, and the fold that
//! counts real opportunities per reason (#10973).
//!
//! Why: an opportunity row measured what compression could have saved but not
//! why the `PreToolUse` rewrite left the result alone, so a reader could not
//! tell a chained `cd X && …` from a tool with no filter. And most rows are no
//! opportunity at all: a report counting every row overstates the gap.
//! What: [`SkipReason`] is the `skip_reason` field of an
//! [`OpportunityRow`](crate::core::savings_opportunity::OpportunityRow).
//! [`fold_real_opportunities`] reads a day file's lines and tallies, per
//! reason, only the rows whose `would_compress_bytes < bytes_returned`.
//! Test: `a_legacy_row_without_a_skip_reason_parses_as_unknown`,
//! `the_fold_counts_only_real_opportunities_per_reason`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::savings_opportunity::OpportunityRow;

/// Why a tool result reached the model uncompressed, or that it was compressed.
///
/// Why (#10973): each variant is one decision of the `PreToolUse` rewrite
/// guards (`tm hook`), so a report can rank the refusals by bytes left behind.
/// What: serialized `snake_case`. A row written before the field existed, or
/// carrying a value this build does not know, reads as [`SkipReason::Unknown`].
/// Test: `a_legacy_row_without_a_skip_reason_parses_as_unknown`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The rewrite applied: the result went through `tm compress`.
    Compressed,
    /// A non-Bash result the dry run would not shrink.
    NoGain,
    /// A non-Bash result (subagent, MCP, Read, …): no hook compresses these.
    NonBash,
    /// The command chains, pipes, backgrounds or redirects (`&`, `|`, `;`, `<`, `>`).
    Chained,
    /// A `#` or a trailing `\` the brace-group wrap cannot carry (#7384).
    Unwrappable,
    /// An orchestrator program the rewrite never wraps (`make`, `terraform`, …).
    Orchestrator,
    /// The program word could not be resolved (`sudo -u root …`).
    UnresolvedProgram,
    /// A read verb on a source file (#7120).
    SourceRead,
    /// The derived tool name carries shell metacharacters.
    UnsafeToolName,
    /// No compression filter covers the derived tool name (#6566).
    NoFilter,
    /// The command was empty.
    EmptyCommand,
    /// The hook could not read a cwd, so no rule set could be honoured.
    NoCwd,
    /// A heavy build leased without compression, or refused by the lease.
    HeavyBuild,
    /// Isolation worktree: the first word is a `KEY=value` or a wrapper.
    WrapperPrefix,
    /// Isolation worktree: the program is not an executable file.
    NotExecutable,
    /// Isolation worktree: the original matches a `deny` permission rule.
    Denied,
    /// Not decidable: a legacy row, an unknown value, or a Bash call with no command.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Real opportunities for one [`SkipReason`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpportunityTally {
    /// Rows whose dry run would have shrunk the result.
    pub rows: u64,
    /// Sum of those rows' `bytes_returned`.
    pub bytes_returned: u64,
    /// Sum of `bytes_returned - would_compress_bytes` over those rows.
    pub bytes_saveable: u64,
}

/// Fold an opportunity day file's JSONL into per-reason tallies of real opportunities.
///
/// Why (#10973): a row is a real opportunity only when compression would have
/// shrunk it; counting every row reports ~99% of Bash rows as gaps that are not.
/// What: each line that parses as an [`OpportunityRow`] with
/// `would_compress_bytes < bytes_returned` adds to its `skip_reason`'s tally.
/// Lines that do not parse — the `day_full` sentinel, a torn write — are
/// skipped. Every event is counted, so a caller wanting one count per subagent
/// result filters `event` first (see `OpportunityRow`).
/// Test: `the_fold_counts_only_real_opportunities_per_reason`.
pub fn fold_real_opportunities(jsonl: &str) -> BTreeMap<SkipReason, OpportunityTally> {
    let mut tallies: BTreeMap<SkipReason, OpportunityTally> = BTreeMap::new();
    let rows = jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<OpportunityRow>(line).ok());
    for row in rows.filter(|r| r.would_compress_bytes < r.bytes_returned) {
        let tally = tallies.entry(row.skip_reason).or_default();
        tally.rows += 1;
        tally.bytes_returned += row.bytes_returned;
        tally.bytes_saveable += row.bytes_returned - row.would_compress_bytes;
    }
    tallies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_row_without_a_skip_reason_parses_as_unknown() {
        let legacy = r#"{"ts":"2026-11-10T00:00:00Z","session_id":"s1","event":"PostToolUse","tool_type":"bash","tool_name":"cd /x","bytes_returned":900,"bytes_scanned":900,"would_compress_bytes":900}"#;
        let row: OpportunityRow = serde_json::from_str(legacy).expect("a legacy row parses");
        assert_eq!(row.skip_reason, SkipReason::Unknown);
        assert_eq!(row.bytes_returned, 900);
        let future = legacy.replace('}', r#","skip_reason":"from_a_newer_build"}"#);
        let row: OpportunityRow = serde_json::from_str(&future).expect("parses");
        assert_eq!(row.skip_reason, SkipReason::Unknown);
    }

    #[test]
    fn the_fold_counts_only_real_opportunities_per_reason() {
        let line = |reason: &str, returned: u64, would: u64| {
            format!(
                r#"{{"ts":"t","session_id":"s","event":"PostToolUse","tool_type":"bash","tool_name":"x","bytes_returned":{returned},"bytes_scanned":{returned},"would_compress_bytes":{would},"skip_reason":"{reason}"}}"#
            )
        };
        let jsonl = [
            line("chained", 1_000, 400),
            line("chained", 500, 100),
            line("chained", 800, 800), // no gain: not an opportunity
            line("no_filter", 300, 300),
            line("non_bash", 2_000, 2_500), // grew: not an opportunity
            line("source_read", 900, 300),
            r#"{"ts":"t","event":"day_full","day_cap_bytes":8}"#.to_string(),
            "{torn".to_string(),
        ]
        .join("\n");
        let tallies = fold_real_opportunities(&jsonl);
        assert_eq!(
            tallies.get(&SkipReason::Chained),
            Some(&OpportunityTally {
                rows: 2,
                bytes_returned: 1_500,
                bytes_saveable: 1_000,
            })
        );
        assert_eq!(tallies[&SkipReason::SourceRead].rows, 1);
        assert_eq!(tallies.len(), 2, "{tallies:?}");
    }
}
