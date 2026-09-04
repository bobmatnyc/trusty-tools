//! L1-L3 bake-off milestone exit gate (#5441).
//!
//! Why: every Trusty Code milestone is supposed to close only after a real
//! coding-harness bake-off completes levels 1-3 with no unexplained regression
//! against the previous accepted baseline. The first independent qualification
//! attempt showed why prose alone cannot enforce that: three levels ran and
//! every verifier check passed, yet the evidence was still disqualified because
//! the retained metadata named no candidate commit, no binary hash, no
//! runner/challenge revision and no source digests, nothing mechanically
//! rejected that, and L3's +27% wall-clock / +50% turns had no recorded
//! disposition. This module is the missing mechanism.
//!
//! What: a pure, offline evidence gate over a retained bundle. [`preflight`]
//! rejects the five conditions #5441 names — incomplete L1-L3 coverage, missing
//! artifacts or provenance, mock-only evidence, a stale runner, and results
//! produced by a different `tcode` build — and [`compare`] diffs a candidate
//! bundle against the previous accepted baseline, blocking correctness or
//! completion regressions outright while requiring a written disposition for
//! cost/token/turn/duration changes outside tolerance. Both produce
//! [`Violation`]s collected into one [`GateReport`]. Nothing here runs a model,
//! spawns the runner, or touches the network: the expensive real-model L1-L3
//! run happens once after a milestone candidate is frozen, and this gate reads
//! only what that run retained. The operator surface is `tcode bakeoff-gate`
//! (wired in `src/cli/bakeoff.rs`); the bundle layout and a worked invocation
//! are in `docs/reference/bakeoff-exit-gate.md`.
//!
//! Test: `bakeoff::tests::*`, plus the binary-level
//! `tests/bakeoff_gate_e2e.rs`.
//!
//! [`preflight`]: crate::bakeoff::preflight()
//! [`compare`]: crate::bakeoff::compare
//! [`Violation`]: crate::bakeoff::Violation
//! [`GateReport`]: crate::bakeoff::GateReport

use std::fmt;

use serde::Serialize;

pub mod compare;
pub mod metadata;
pub mod preflight;

pub use compare::{Baseline, ComparisonInputs, compare_against_baseline};
pub use metadata::LevelMetadata;
pub use preflight::{Bundle, Pins, load_bundle, preflight};

/// The three levels a milestone exit gate must cover.
///
/// Why: #5441 requires all three; "we ran L1 and L2" is incomplete coverage,
/// not partial credit.
/// What: the level numbers, in the order the report lists them.
/// Test: `bakeoff::tests::a_missing_level_is_incomplete_coverage`.
pub const LEVELS: [u8; 3] = [1, 2, 3];

/// Files every level directory must contain, non-empty.
///
/// Why: #5441 requires the raw `tcode_report.json`, stderr, prompt, solution,
/// verifier output, and `metadata.json` to be preserved per level. A retained
/// bundle missing any of them cannot be re-examined later, which is the whole
/// point of retaining it.
/// What: the fixed filenames the gate looks for inside `<bundle>/L<n>/`.
/// Test: `bakeoff::tests::a_missing_artifact_is_rejected`,
/// `bakeoff::tests::an_empty_artifact_is_rejected`.
pub const REQUIRED_ARTIFACTS: [&str; 6] = [
    "metadata.json",
    "tcode_report.json",
    "prompt.txt",
    "stderr.log",
    "solution.diff",
    "verifier.json",
];

/// Optional bundle-level file recording accepted performance changes.
///
/// Why: #5441 lets a performance change outside tolerance pass with "a
/// documented disposition" — so the disposition has to live somewhere the gate
/// can read, not in a PR comment.
/// What: a JSON object at the bundle root mapping a metric key (`L3.turns`) to
/// the operator's written acceptance.
/// Test: `bakeoff::tests::a_documented_disposition_clears_a_performance_change`.
pub const DISPOSITIONS_FILE: &str = "dispositions.json";

/// The single terminal status a bake-off level may finish with.
///
/// Why: `partial`, `deadline_exceeded` and `no_changes` are all real, useful
/// distinctions for a `run-task` caller, but none of them is a milestone-grade
/// completion. Treating anything but `success` as clean would let a
/// completion regression through the gate that #5441 exists to block.
/// What: the literal `run_task::report` status string for [`crate::run_task::ExitCode::Success`].
/// Test: `bakeoff::tests::a_status_regression_blocks_closure`.
pub const CLEAN_STATUS: &str = "success";

/// Why a bundle failed the gate.
///
/// Why: the gate's output is read by an operator deciding whether to close a
/// milestone, so each finding needs a stable machine-readable key as well as
/// prose. A single opaque "gate failed" string would send them back to diffing
/// JSON by hand — the exact cost the first qualification attempt paid.
/// What: one variant per rejection reason; [`Rule::as_str`] is the stable key
/// the JSON report emits.
/// Test: `bakeoff::tests::rule_keys_are_distinct`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Rule {
    /// A required level (1, 2, or 3) is absent from the bundle.
    IncompleteCoverage,
    /// A required per-level artifact is missing or empty.
    MissingArtifact,
    /// `metadata.json` did not parse, or contradicts its own level's
    /// `tcode_report.json`.
    MalformedMetadata,
    /// The level declares mock/offline evidence, or its verifier ran nothing.
    MockEvidence,
    /// A provenance field is empty, `"unknown"`, or zero.
    MissingProvenance,
    /// The runner or candidate checkout had uncommitted changes, so its
    /// recorded revision does not identify what ran.
    DirtyCheckout,
    /// The runner path/revision is inconsistent across levels or differs from
    /// the pinned revision.
    StaleRunner,
    /// Levels disagree about which `tcode` build produced them, or the build
    /// differs from the pinned candidate.
    BuildMismatch,
    /// Verifier pass rate or terminal status regressed against the baseline.
    CorrectnessRegression,
    /// A level the baseline covered is absent from the candidate.
    MissingDeliverable,
    /// A cost/token/turn/duration change exceeded tolerance with no recorded
    /// disposition.
    UndispositionedChange,
}

impl Rule {
    /// The stable snake_case key for this rule.
    ///
    /// Why: the JSON report and the human report must name a rule identically,
    /// and a caller grepping CI output needs a key that survives rewording.
    /// What: the same literal the `Serialize` impl emits.
    /// Test: `bakeoff::tests::rule_keys_are_distinct`.
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::IncompleteCoverage => "incomplete_coverage",
            Rule::MissingArtifact => "missing_artifact",
            Rule::MalformedMetadata => "malformed_metadata",
            Rule::MockEvidence => "mock_evidence",
            Rule::MissingProvenance => "missing_provenance",
            Rule::DirtyCheckout => "dirty_checkout",
            Rule::StaleRunner => "stale_runner",
            Rule::BuildMismatch => "build_mismatch",
            Rule::CorrectnessRegression => "correctness_regression",
            Rule::MissingDeliverable => "missing_deliverable",
            Rule::UndispositionedChange => "undispositioned_change",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One reason the gate is refusing to pass.
///
/// Why: every finding must say which level it came from, so an operator fixes
/// the right rerun rather than all three.
/// What: the rule, the level it applies to (`None` for bundle-wide findings),
/// and a one-line detail naming the concrete field or number.
/// Test: `bakeoff::tests::a_missing_artifact_is_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Violation {
    /// Which rule fired.
    pub rule: Rule,
    /// The bake-off level, when the finding is level-scoped.
    pub level: Option<u8>,
    /// What specifically was wrong.
    pub detail: String,
}

impl Violation {
    /// Build a level-scoped violation.
    ///
    /// Why: the two constructors keep call sites to one line, which matters in
    /// [`preflight()`] where nearly every branch produces one.
    /// What: sets `level` to `Some(level)`.
    /// Test: `bakeoff::tests::a_missing_artifact_is_rejected`.
    pub fn level(rule: Rule, level: u8, detail: impl Into<String>) -> Self {
        Self {
            rule,
            level: Some(level),
            detail: detail.into(),
        }
    }

    /// Build a bundle-wide violation.
    ///
    /// Why: cross-level findings (build drift, runner drift) belong to no
    /// single level.
    /// What: sets `level` to `None`.
    /// Test: `bakeoff::tests::build_drift_across_levels_is_rejected`.
    pub fn bundle(rule: Rule, detail: impl Into<String>) -> Self {
        Self {
            rule,
            level: None,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.level {
            Some(level) => write!(f, "[{}] L{}: {}", self.rule, level, self.detail),
            None => write!(f, "[{}] bundle: {}", self.rule, self.detail),
        }
    }
}

/// The gate's verdict over one bundle, optionally compared to a baseline.
///
/// Why: preflight and comparison findings land in one document so the operator
/// sees the whole picture in one run instead of fixing provenance, rerunning,
/// and only then discovering a correctness regression.
/// What: the levels that were readable, every violation, and the informational
/// notes (accepted dispositions, within-tolerance deltas) that explain what the
/// gate looked at and let pass.
/// Test: `bakeoff::tests::a_complete_bundle_passes_preflight`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct GateReport {
    /// Levels whose `metadata.json` parsed, ascending.
    pub levels: Vec<u8>,
    /// Every reason the gate is refusing to pass.
    pub violations: Vec<Violation>,
    /// Findings that did not block: accepted dispositions, deltas inside
    /// tolerance, and what the gate compared against.
    pub notes: Vec<String>,
}

impl GateReport {
    /// Whether the milestone may close on this evidence.
    ///
    /// Why: one predicate, so the CLI exit code and every test agree.
    /// What: true when no violation was recorded.
    /// Test: `bakeoff::tests::a_complete_bundle_passes_preflight`.
    pub fn passed(&self) -> bool {
        self.violations.is_empty()
    }

    /// Fold another report's findings into this one.
    ///
    /// Why: the comparison stage runs after preflight and must not discard what
    /// preflight already found.
    /// What: appends levels-preserving; violations and notes are concatenated.
    /// Test: `bakeoff::tests::comparison_findings_join_preflight_findings`.
    pub fn absorb(&mut self, other: GateReport) {
        self.violations.extend(other.violations);
        self.notes.extend(other.notes);
    }

    /// Render the verdict for a terminal.
    ///
    /// Why: an operator reading CI output wants the verdict on line one and the
    /// concrete findings under it, not a JSON blob.
    /// What: a `PASS`/`FAIL` header naming the covered levels, then one line per
    /// violation, then the notes.
    /// Test: `bakeoff::tests::human_render_names_every_violation`.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        let levels = if self.levels.is_empty() {
            "none".to_string()
        } else {
            self.levels
                .iter()
                .map(|l| format!("L{l}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        out.push_str(&format!(
            "bakeoff-gate: {} ({} violation(s); levels read: {levels})\n",
            if self.passed() { "PASS" } else { "FAIL" },
            self.violations.len(),
        ));
        for violation in &self.violations {
            out.push_str(&format!("  {violation}\n"));
        }
        for note in &self.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
        out
    }

    /// Render the verdict as one machine-readable JSON document.
    ///
    /// Why: the gate is meant to be callable from the external runner and from
    /// CI, both of which want to branch on structure rather than parse prose.
    /// What: `{"passed", "levels", "violations", "notes"}`, pretty-printed.
    /// Test: `bakeoff::tests::json_render_carries_the_verdict_and_rules`.
    pub fn render_json(&self) -> String {
        let value = serde_json::json!({
            "passed": self.passed(),
            "levels": self.levels,
            "violations": self.violations,
            "notes": self.notes,
        });
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests;
