//! Candidate-versus-baseline comparison (#5441).
//!
//! Why: #5441's gate contract asks for two different verdicts from one diff.
//! A correctness or completion regression — a verifier check that used to pass
//! and no longer does, a level that used to finish and now times out, a level
//! that is simply absent — blocks milestone closure outright. A cost, token,
//! turn, or duration change outside an explicit tolerance does not block, but
//! it does require a documented disposition; the first qualification attempt
//! stalled precisely because L3's +27% wall-clock and +50% turns had none.
//! What: [`compare_against_baseline`] walks every level the baseline covered,
//! blocks on missing deliverables, pass-rate drops, and status regressions, and
//! for each of the four performance metrics either records the delta as a note
//! (inside tolerance, or dispositioned) or blocks on it.
//! Test: `bakeoff::tests::a_pass_rate_drop_blocks_closure`,
//! `bakeoff::tests::a_documented_disposition_clears_a_performance_change`.

use std::collections::BTreeMap;

use crate::bakeoff::metadata::LevelMetadata;
use crate::bakeoff::{Bundle, CLEAN_STATUS, DISPOSITIONS_FILE, GateReport, Rule, Violation};

/// Default percentage change a metric may move before it needs a disposition.
///
/// Why: run-to-run variance on a real model is real; blocking on every
/// percentage point would make the gate noise. 20% is wide enough to absorb
/// that variance and narrow enough that the L3 change which triggered this
/// ticket (+27% wall-clock, +50% turns, +22% cost) would have been caught.
/// What: a percentage, overridable per invocation.
/// Test: `bakeoff::tests::a_small_change_stays_inside_the_default_tolerance`.
pub const DEFAULT_TOLERANCE_PCT: f64 = 20.0;

/// A previously accepted bundle to measure the candidate against.
///
/// Why: naming the type makes the argument order at the call site unambiguous —
/// a candidate and a baseline are the same shape, and swapping them silently
/// inverts every verdict.
/// What: a newtype over the loaded baseline bundle.
/// Test: `bakeoff::tests::a_pass_rate_drop_blocks_closure`.
#[derive(Debug, Clone)]
pub struct Baseline(pub Bundle);

/// Everything the comparison stage needs beyond the two bundles.
///
/// Why: tolerance and dispositions arrive from different places (a flag and a
/// file in the candidate bundle) and both are optional; bundling them keeps
/// [`compare_against_baseline`] to three arguments.
/// What: the tolerance percentage and the disposition map read from
/// [`DISPOSITIONS_FILE`].
/// Test: `bakeoff::tests::a_documented_disposition_clears_a_performance_change`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ComparisonInputs {
    /// Percentage change allowed before a metric needs a disposition.
    pub tolerance_pct: f64,
    /// Metric key (`L3.turns`) to the operator's written acceptance.
    pub dispositions: BTreeMap<String, String>,
}

impl Default for ComparisonInputs {
    fn default() -> Self {
        Self {
            tolerance_pct: DEFAULT_TOLERANCE_PCT,
            dispositions: BTreeMap::new(),
        }
    }
}

impl ComparisonInputs {
    /// Load `<bundle>/dispositions.json`, treating an absent file as none.
    ///
    /// Why: most bundles have no accepted regression, so requiring the file
    /// would be ceremony. A file that exists but does not parse is an error,
    /// not silently-none: an operator who wrote a disposition and typoed the
    /// JSON must not be told their regression is undispositioned.
    /// What: returns the parsed map, an empty map when the file is absent, or
    /// the parse error.
    /// Test: `bakeoff::tests::a_documented_disposition_clears_a_performance_change`,
    /// `bakeoff::tests::a_malformed_dispositions_file_is_an_error`.
    pub fn load(bundle: &Bundle, tolerance_pct: f64) -> Result<Self, String> {
        let path = bundle.root.join(DISPOSITIONS_FILE);
        let dispositions = match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| format!("{} is not valid: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("{} unreadable: {e}", path.display())),
        };
        Ok(Self {
            tolerance_pct,
            dispositions,
        })
    }
}

/// One comparable scalar, named the way its disposition key is written.
///
/// Why: four metrics compared by four near-identical code paths is where a
/// copy-paste bug lives. One table, one loop.
/// What: the metric's key suffix and its value for a given level.
/// Test: `bakeoff::tests::token_regression_beyond_tolerance_needs_a_disposition`.
fn metrics(meta: &LevelMetadata) -> [(&'static str, f64); 4] {
    [
        ("turns", meta.run.turns as f64),
        ("duration_secs", meta.run.duration_secs),
        ("tokens", meta.run.tokens.total() as f64),
        ("cost_usd", meta.run.cost_usd.unwrap_or(0.0)),
    ]
}

/// Diff a candidate bundle against the previously accepted baseline.
///
/// Why: #5441 closes a milestone on "no unexplained regression against the
/// previous accepted milestone baseline", which is a comparison, not a
/// threshold — a level that has always taken 40 minutes is fine; one that
/// suddenly takes 40 is the finding.
/// What: for every level the baseline covers, blocks when the candidate omits
/// it, when its verifier pass rate falls, or when its terminal status stops
/// being [`CLEAN_STATUS`]; then measures the four `metrics` and blocks on a
/// change beyond `tolerance_pct` unless the candidate carries a disposition for
/// that metric key. Levels the candidate adds are noted, never blocked.
/// Test: `bakeoff::tests::a_pass_rate_drop_blocks_closure`,
/// `bakeoff::tests::a_status_regression_blocks_closure`,
/// `bakeoff::tests::a_missing_candidate_level_is_a_missing_deliverable`.
pub fn compare_against_baseline(
    candidate: &Bundle,
    baseline: &Baseline,
    inputs: &ComparisonInputs,
) -> GateReport {
    let mut report = GateReport::default();
    report.notes.push(format!(
        "compared against baseline {}",
        baseline.0.root.display()
    ));

    for (&level, base) in &baseline.0.levels {
        let Some(cand) = candidate.levels.get(&level) else {
            report.violations.push(Violation::level(
                Rule::MissingDeliverable,
                level,
                "covered by the baseline but absent from the candidate".to_string(),
            ));
            continue;
        };

        let base_meta = &base.metadata;
        let cand_meta = &cand.metadata;

        if cand_meta.verifier.checks_passed < base_meta.verifier.checks_passed {
            report.violations.push(Violation::level(
                Rule::CorrectnessRegression,
                level,
                format!(
                    "verifier passed {}/{} against the baseline's {}/{}",
                    cand_meta.verifier.checks_passed,
                    cand_meta.verifier.checks_total,
                    base_meta.verifier.checks_passed,
                    base_meta.verifier.checks_total
                ),
            ));
        }

        if base_meta.run.status == CLEAN_STATUS && cand_meta.run.status != CLEAN_STATUS {
            report.violations.push(Violation::level(
                Rule::CorrectnessRegression,
                level,
                format!(
                    "terminal status is {} against the baseline's {CLEAN_STATUS}",
                    cand_meta.run.status
                ),
            ));
        }

        compare_metrics(level, base_meta, cand_meta, inputs, &mut report);
    }

    for &level in candidate.levels.keys() {
        if !baseline.0.levels.contains_key(&level) {
            report
                .notes
                .push(format!("L{level} has no baseline to compare against"));
        }
    }

    report
}

/// Measure one level's four performance metrics against the baseline's.
///
/// Why: the tolerance/disposition rule is identical for all four, and #5441
/// wants the accepted ones recorded rather than silently dropped — a note is
/// how the next milestone learns that this delta was already looked at.
/// What: computes the signed percentage change, ignores an improvement or a
/// change inside tolerance (noting the latter), records a dispositioned
/// regression as a note, and blocks on an undispositioned one.
/// Test: `bakeoff::tests::token_regression_beyond_tolerance_needs_a_disposition`,
/// `bakeoff::tests::an_improvement_never_needs_a_disposition`.
fn compare_metrics(
    level: u8,
    base: &LevelMetadata,
    cand: &LevelMetadata,
    inputs: &ComparisonInputs,
    report: &mut GateReport,
) {
    for ((name, base_value), (_, cand_value)) in metrics(base).into_iter().zip(metrics(cand)) {
        let Some(delta_pct) = percent_change(base_value, cand_value) else {
            continue;
        };
        if delta_pct <= inputs.tolerance_pct {
            if delta_pct.abs() >= 1.0 {
                report.notes.push(format!(
                    "L{level} {name}: {base_value:.3} -> {cand_value:.3} ({delta_pct:+.1}%), within tolerance"
                ));
            }
            continue;
        }

        let key = format!("L{level}.{name}");
        match inputs.dispositions.get(&key) {
            Some(disposition) => report.notes.push(format!(
                "L{level} {name}: {base_value:.3} -> {cand_value:.3} ({delta_pct:+.1}%), dispositioned: {disposition}"
            )),
            None => report.violations.push(Violation::level(
                Rule::UndispositionedChange,
                level,
                format!(
                    "{name} moved {base_value:.3} -> {cand_value:.3} ({delta_pct:+.1}%), beyond the {:.1}% tolerance; record an accepted disposition under \"{key}\" in {DISPOSITIONS_FILE}",
                    inputs.tolerance_pct
                ),
            )),
        }
    }
}

/// Signed percentage change from `base` to `cand`, when one is meaningful.
///
/// Why: a baseline of zero has no percentage change (every increase is
/// infinite), and a metric that is zero in both runs has not moved. Returning
/// `None` for those keeps the caller from emitting `inf%` findings on a level
/// whose cost was simply never priced.
/// What: `None` when `base` is zero or either value is not finite; otherwise
/// `(cand - base) / base * 100`. A negative result is an improvement.
/// Test: `bakeoff::tests::an_unpriced_baseline_metric_reports_no_change`.
fn percent_change(base: f64, cand: f64) -> Option<f64> {
    if !base.is_finite() || !cand.is_finite() || base == 0.0 {
        return None;
    }
    Some((cand - base) / base * 100.0)
}
