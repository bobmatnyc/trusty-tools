//! Severity-tagged non-fatal faults recorded by the Stage 1 collection pipeline.
//!
//! Why: #5655 — every provider pushed a bare `String` into
//! [`crate::collect::CollectionStats::errors`] and the command printed the vec
//! as warnings, so a failed database write and a single skipped record were the
//! same value and `tga collect` exited 0 for both. Automation reads the exit
//! code, so a half-persisted run reported success.
//! What: [`CollectionFault`] pairs the existing message with a
//! [`FaultSeverity`] set at the push site — `StageFailed` when a whole stage's
//! fetch or write path failed, `ItemSkipped` when one record was dropped and
//! the stage carried on. Nothing here aborts a run; the severity only decides
//! what the caller does with the finished [`crate::collect::CollectionStats`].
//! Test: this module's own `tests`, plus
//! `crate::commands::collect::tests::a_failed_stage_makes_collect_exit_non_zero`.

use std::fmt;

/// How much of a collection stage a recorded fault cost.
///
/// Why: #5655's third closure condition — a long sweep must not be aborted or
/// failed by one malformed record, but a write path that failed wholesale has
/// to reach the exit code.
/// What: two variants, ordered by blast radius. `StageFailed` is the one that
/// makes `tga collect` exit non-zero; `ItemSkipped` is reported and otherwise
/// left alone.
/// Test: `stage_failed_and_item_skipped_carry_distinct_labels`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultSeverity {
    /// One record was dropped; the rest of the stage ran and persisted normally.
    ItemSkipped,
    /// A whole stage's fetch or write path failed, so its data is absent.
    StageFailed,
}

impl FaultSeverity {
    /// Operator-facing prefix for this severity in the end-of-run fault list.
    pub fn label(self) -> &'static str {
        match self {
            Self::ItemSkipped => "warning",
            Self::StageFailed => "error",
        }
    }
}

/// One non-fatal problem encountered during collection, with its blast radius.
///
/// Why: see the module header — the severity is what lets the exit code
/// distinguish an incomplete database from a run that merely skipped a record.
/// What: a [`FaultSeverity`] plus the message the provider already built.
/// [`fmt::Display`] renders the message alone, so every existing `{e}` call
/// site prints exactly what it printed before.
/// Test: `display_renders_the_message_alone`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionFault {
    /// How much of the stage this fault cost.
    pub severity: FaultSeverity,
    /// Operator-facing description, built by the provider that failed.
    pub message: String,
}

impl CollectionFault {
    /// Record a dropped record: the stage continued and persisted the rest.
    pub fn item_skipped(message: impl Into<String>) -> Self {
        Self {
            severity: FaultSeverity::ItemSkipped,
            message: message.into(),
        }
    }

    /// Record a stage whose fetch or write path failed, leaving its data absent.
    pub fn stage_failed(message: impl Into<String>) -> Self {
        Self {
            severity: FaultSeverity::StageFailed,
            message: message.into(),
        }
    }

    /// Whether this fault means a stage's data is missing from the database.
    pub fn is_stage_failure(&self) -> bool {
        self.severity == FaultSeverity::StageFailed
    }
}

impl fmt::Display for CollectionFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Recording and querying faults.
///
/// The impl lives here rather than beside the struct so the recording verbs and
/// the severity they set stay in one file — and because
/// [`crate::collect::collector`] is on a frozen SLOC budget.
impl crate::collect::collector::CollectionStats {
    /// Record a stage whose fetch or write path failed, leaving its data absent.
    ///
    /// Why: this is the fault that reaches the process exit code, so the push
    /// sites say which one they mean rather than passing a severity argument.
    /// What: appends a [`FaultSeverity::StageFailed`] fault. Nothing aborts —
    /// the pipeline runs its remaining stages either way.
    /// Test: `crate::collect::linear_pipeline::tests::linear_stage_faults_are_recorded_as_stage_failures`.
    pub fn fail_stage(&mut self, message: impl Into<String>) {
        self.errors.push(CollectionFault::stage_failed(message));
    }

    /// Record one dropped record; the stage persisted everything else.
    ///
    /// Why: #5655's third closure condition — one malformed ticket must not
    /// fail a long sweep, so this severity never reaches the exit code.
    /// What: appends a [`FaultSeverity::ItemSkipped`] fault.
    /// Test: `crate::commands::collect::tests::skipped_records_alone_keep_collect_at_exit_zero`.
    pub fn skip_item(&mut self, message: impl Into<String>) {
        self.errors.push(CollectionFault::item_skipped(message));
    }

    /// The faults whose stage never persisted its data.
    ///
    /// Why: #5655 — `tga collect` printed every recorded fault as a warning and
    /// returned success, so a failed `work_items` write and a skipped record
    /// both left the process exiting 0. Automation reads the exit code, so a
    /// half-persisted run was indistinguishable from a clean one.
    /// What: filters the recorded faults to [`FaultSeverity::StageFailed`], in
    /// the order the providers recorded them.
    /// Test: `crate::commands::collect::tests::a_failed_stage_makes_collect_exit_non_zero`.
    pub fn stage_failures(&self) -> Vec<&CollectionFault> {
        self.errors
            .iter()
            .filter(|e| e.is_stage_failure())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders_the_message_alone() {
        let fault = CollectionFault::stage_failed("Linear: store work_items failed: disk full");
        assert_eq!(
            fault.to_string(),
            "Linear: store work_items failed: disk full",
            "existing `{{e}}` call sites must print what they always printed"
        );
    }

    #[test]
    fn stage_failed_and_item_skipped_carry_distinct_labels() {
        assert!(CollectionFault::stage_failed("x").is_stage_failure());
        assert!(!CollectionFault::item_skipped("x").is_stage_failure());
        assert_eq!(FaultSeverity::StageFailed.label(), "error");
        assert_eq!(FaultSeverity::ItemSkipped.label(), "warning");
    }
}
