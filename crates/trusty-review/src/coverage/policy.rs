//! Coverage policy — configurable thresholds and grade/verdict contribution.
//!
//! Why: today the system prompt says "do not block on test coverage"; this
//! module makes coverage a first-class, OPTIONAL gating signal (issue #1014).
//! When `enabled = false` (the default), this module is a no-op and the
//! existing verdict pipeline is unchanged.  When enabled, low or zero new-code
//! coverage can lower the grade and/or force REQUEST_CHANGES.
//!
//! What: `CoveragePolicy` holds the opt-in flag and two configurable thresholds:
//!   - `min_new_code_pct` — minimum acceptable coverage for new/changed lines.
//!     When new-code coverage falls below this, verdict floors to REQUEST_CHANGES
//!     and the grade is clamped to D+ or lower.
//!   - `max_net_drop_pct` — maximum allowed drop in net project coverage (in
//!     percentage points).  A drop exceeding this also floors to REQUEST_CHANGES.
//!
//! `CoverageVerdictContrib` is returned by `evaluate_coverage` and carries the
//! recommended floor and a human-readable reason for inclusion in the review body.
//!
//! Test: `coverage_policy_off_is_noop`, `coverage_policy_zero_new_code`,
//! `coverage_policy_below_threshold`, `coverage_policy_net_drop`,
//! `coverage_policy_pass`.

use serde::Deserialize;

use crate::{coverage::lcov::CoverageReport, models::Verdict, pipeline::letter_grade::Grade};

// ─── Policy defaults ──────────────────────────────────────────────────────────

/// Default minimum new-code coverage (percentage points).
///
/// Why: an 80% floor is common in industry and matches cargo-tarpaulin's
/// default output threshold; operators can lower it via config.
/// What: `min_new_code_pct` defaults to 80.0.
pub const DEFAULT_MIN_NEW_CODE_PCT: f64 = 80.0;

/// Default maximum net coverage drop (percentage points).
///
/// Why: a 1-point drop in overall coverage is a clear regression signal;
/// tighter than 5% to catch subtle regressions without over-triggering.
/// What: `max_net_drop_pct` defaults to 1.0 (one percentage point).
pub const DEFAULT_MAX_NET_DROP_PCT: f64 = 1.0;

// ─── TOML file config ─────────────────────────────────────────────────────────

/// `[coverage]` section of the review config TOML file.
///
/// Why: operators who want persistent coverage gating can set `enabled = true`
/// in their `config.toml` rather than exporting an env var on every machine.
/// What: all fields have `#[serde(default)]` so missing keys fall back to their
/// coded defaults without erroring.
/// Test: `coverage_file_config_defaults`.
#[derive(Debug, Default, Deserialize)]
pub struct CoverageFileConfig {
    /// `enabled` — whether coverage gating is active.  Default: false (opt-in).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `min_new_code_pct` — minimum coverage on new/changed lines.
    #[serde(default)]
    pub min_new_code_pct: Option<f64>,
    /// `max_net_drop_pct` — maximum allowed net coverage drop (pct-points).
    #[serde(default)]
    pub max_net_drop_pct: Option<f64>,
    /// `lcov_path` — path to the LCOV report file produced by CI.
    #[serde(default)]
    pub lcov_path: Option<String>,
}

// ─── Resolved policy ──────────────────────────────────────────────────────────

/// Resolved coverage policy, combining env vars + TOML config.
///
/// Why: mirrors the pattern used by `VerificationConfig` and `ContextConfig`
/// in this codebase — resolve once at startup, pass around as a plain struct.
/// What: `enabled` gates the entire feature; when false all fields are ignored.
/// Test: `coverage_policy_from_env`, `coverage_policy_from_file`.
#[derive(Debug, Clone)]
pub struct CoveragePolicy {
    /// Whether coverage gating is active.  `false` = no-op (default).
    pub enabled: bool,
    /// Minimum coverage on new/changed instrumented lines (percentage, 0–100).
    pub min_new_code_pct: f64,
    /// Maximum allowed drop in net line coverage (percentage points).
    pub max_net_drop_pct: f64,
    /// Optional path to an LCOV report file.  When set, the pipeline loads it
    /// before the LLM call.  When absent, callers must supply the report externally
    /// or coverage is skipped.
    pub lcov_path: Option<std::path::PathBuf>,
}

impl Default for CoveragePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            min_new_code_pct: DEFAULT_MIN_NEW_CODE_PCT,
            max_net_drop_pct: DEFAULT_MAX_NET_DROP_PCT,
            lcov_path: None,
        }
    }
}

impl CoveragePolicy {
    /// Resolve from env vars merged over an optional TOML file config.
    ///
    /// Why: follows the same two-layer resolution used by other config types
    /// in this codebase (env vars win; TOML is the fallback).
    /// What: reads `TRUSTY_REVIEW_COVERAGE_ENABLED`, `TRUSTY_REVIEW_MIN_NEW_CODE_PCT`,
    /// `TRUSTY_REVIEW_MAX_NET_DROP_PCT`, `TRUSTY_REVIEW_LCOV_PATH`.  Absent vars
    /// fall back to the TOML file, then to the coded defaults.
    /// Test: `coverage_policy_from_env`.
    pub fn from_env_and_file(file: Option<&CoverageFileConfig>) -> Self {
        let enabled = load_env_bool("TRUSTY_REVIEW_COVERAGE_ENABLED")
            .or_else(|| file.and_then(|f| f.enabled))
            .unwrap_or(false);

        let min_new_code_pct = load_env_f64("TRUSTY_REVIEW_MIN_NEW_CODE_PCT")
            .or_else(|| file.and_then(|f| f.min_new_code_pct))
            .unwrap_or(DEFAULT_MIN_NEW_CODE_PCT)
            .clamp(0.0, 100.0);

        let max_net_drop_pct = load_env_f64("TRUSTY_REVIEW_MAX_NET_DROP_PCT")
            .or_else(|| file.and_then(|f| f.max_net_drop_pct))
            .unwrap_or(DEFAULT_MAX_NET_DROP_PCT)
            .clamp(0.0, 100.0);

        let lcov_path = std::env::var("TRUSTY_REVIEW_LCOV_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                file.and_then(|f| f.lcov_path.as_ref())
                    .filter(|s| !s.trim().is_empty())
                    .map(std::path::PathBuf::from)
            });

        Self {
            enabled,
            min_new_code_pct,
            max_net_drop_pct,
            lcov_path,
        }
    }
}

// ─── Evaluation ───────────────────────────────────────────────────────────────

/// The coverage evaluation verdict contribution.
///
/// Why: the runner applies this AFTER the LLM verdict, so it needs both the
/// recommended floor and a human-readable reason to inject into the review body.
/// What: `floor` is `None` when coverage passes (no downgrade needed).  `reason`
/// is always set for diagnostics; it is only injected into the body when `floor`
/// is `Some`.
/// Test: `coverage_policy_zero_new_code`, `coverage_policy_pass`.
#[derive(Debug, Clone)]
pub struct CoverageVerdictContrib {
    /// Recommended minimum verdict floor (None = pass, no floor applied).
    pub floor: Option<Verdict>,
    /// Recommended minimum grade ceiling when a floor is triggered.
    /// `None` = no grade clamp.
    pub grade_ceiling: Option<Grade>,
    /// Human-readable summary for the review body (always populated).
    pub summary: String,
}

/// Evaluate the coverage report against the policy and return a verdict contribution.
///
/// Why: this is the single place that maps coverage numbers to review outcomes.
/// It is intentionally pure (no side-effects, no I/O) so it is trivially testable.
/// What: checks two conditions:
///   1. `new_code_pct` (if present) vs `policy.min_new_code_pct` — new code with
///      zero or below-threshold coverage floors to REQUEST_CHANGES with grade D+.
///   2. Net coverage drop (current `net_pct` vs `baseline_net_pct`) vs
///      `policy.max_net_drop_pct` — a drop exceeding the threshold floors to
///      REQUEST_CHANGES with grade D+.
///
/// When `policy.enabled` is false, returns a pass contrib (no floor).
/// When both conditions trigger, the most severe floor wins (both are REQUEST_CHANGES).
///
/// Test: `coverage_policy_off_is_noop`, `coverage_policy_zero_new_code`,
/// `coverage_policy_below_threshold`, `coverage_policy_net_drop`,
/// `coverage_policy_pass`.
pub fn evaluate_coverage(
    policy: &CoveragePolicy,
    report: &CoverageReport,
    new_code_pct: Option<f64>,
    baseline_net_pct: Option<f64>,
) -> CoverageVerdictContrib {
    // OFF by default: when disabled this is a strict no-op.
    if !policy.enabled {
        return CoverageVerdictContrib {
            floor: None,
            grade_ceiling: None,
            summary: format!(
                "Coverage: {:.1}% ({}/{} lines hit) — gating disabled (opt-in).",
                report.net_pct, report.lines_hit, report.lines_instrumented
            ),
        };
    }

    let mut reasons: Vec<String> = Vec::new();
    let mut needs_floor = false;

    // Condition 1: new-code coverage below threshold.
    if let Some(nc_pct) = new_code_pct
        && nc_pct < policy.min_new_code_pct
    {
        needs_floor = true;
        if nc_pct < 0.1 {
            // Treat effectively-zero as zero to avoid floating-point noise.
            reasons.push(format!(
                "new-code coverage is 0% (threshold: {:.0}%)",
                policy.min_new_code_pct
            ));
        } else {
            reasons.push(format!(
                "new-code coverage is {nc_pct:.1}% (below threshold: {:.0}%)",
                policy.min_new_code_pct
            ));
        }
    }

    // Condition 2: net coverage drop exceeds threshold.
    if let Some(baseline) = baseline_net_pct {
        let drop = baseline - report.net_pct;
        if drop > policy.max_net_drop_pct {
            needs_floor = true;
            reasons.push(format!(
                "net coverage dropped {drop:.1}pp ({:.1}% → {:.1}%; max allowed: {:.1}pp)",
                baseline, report.net_pct, policy.max_net_drop_pct
            ));
        }
    }

    // Build summary line.
    let new_code_part = new_code_pct
        .map(|p| format!(", new-code: {p:.1}%"))
        .unwrap_or_default();
    let summary_base = format!(
        "Coverage: {:.1}%{new_code_part} ({}/{} lines hit).",
        report.net_pct, report.lines_hit, report.lines_instrumented
    );

    if needs_floor {
        let reason_text = reasons.join("; ");
        CoverageVerdictContrib {
            floor: Some(Verdict::RequestChanges),
            grade_ceiling: Some(Grade::DPlus),
            summary: format!("{summary_base} COVERAGE GATE TRIGGERED: {reason_text}."),
        }
    } else {
        let pass_note = if new_code_pct.is_some() || baseline_net_pct.is_some() {
            " Coverage thresholds met."
        } else {
            ""
        };
        CoverageVerdictContrib {
            floor: None,
            grade_ceiling: None,
            summary: format!("{summary_base}{pass_note}"),
        }
    }
}

// ─── Env-var helpers ──────────────────────────────────────────────────────────

/// Parse a boolean env var: "true"/"1"/"yes" → Some(true); "false"/"0"/"no" → Some(false).
///
/// Why: follows the same convention used by `load_voice_principles` in this codebase.
/// What: absent or unrecognised values return None so the TOML / coded default wins.
/// Test: covered transitively by `coverage_policy_from_env`.
fn load_env_bool(var: &str) -> Option<bool> {
    let val = std::env::var(var).ok()?;
    match val.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Parse an f64 env var.
///
/// Why: avoids `unwrap` in config loading code.
/// What: absent or unparseable values return None.
/// Test: covered transitively.
fn load_env_f64(var: &str) -> Option<f64> {
    std::env::var(var).ok()?.trim().parse().ok()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::lcov::parse_lcov;

    fn make_policy(enabled: bool) -> CoveragePolicy {
        CoveragePolicy {
            enabled,
            min_new_code_pct: 80.0,
            max_net_drop_pct: 1.0,
            lcov_path: None,
        }
    }

    fn zero_report() -> CoverageReport {
        parse_lcov("").expect("empty")
    }

    fn report_with_pct(pct: f64) -> CoverageReport {
        // Construct a synthetic report with the given net percentage.
        let mut report = zero_report();
        report.lines_instrumented = 100;
        report.lines_hit = pct.round() as u64;
        report.net_pct = pct;
        report
    }

    /// When policy is disabled, evaluate_coverage is a no-op regardless of values.
    ///
    /// Why: the default (off) behaviour must be strictly unchanged from today.
    /// Test: passes zero coverage with policy.enabled=false; expects no floor.
    #[test]
    fn coverage_policy_off_is_noop() {
        let policy = make_policy(false);
        let report = report_with_pct(0.0);
        let contrib = evaluate_coverage(&policy, &report, Some(0.0), Some(90.0));
        assert!(
            contrib.floor.is_none(),
            "disabled policy must never produce a floor"
        );
        assert!(
            contrib.grade_ceiling.is_none(),
            "disabled policy must never produce a grade ceiling"
        );
    }

    /// New-code coverage of zero triggers REQUEST_CHANGES floor.
    ///
    /// Why: adding public code with 0% coverage must pull the verdict down.
    /// Test: new_code_pct=0.0, threshold=80% → floor=REQUEST_CHANGES.
    #[test]
    fn coverage_policy_zero_new_code() {
        let policy = make_policy(true);
        let report = report_with_pct(85.0); // Net is fine.
        let contrib = evaluate_coverage(&policy, &report, Some(0.0), None);
        assert_eq!(
            contrib.floor,
            Some(Verdict::RequestChanges),
            "0% new-code coverage must floor to REQUEST_CHANGES"
        );
        assert_eq!(contrib.grade_ceiling, Some(Grade::DPlus));
        assert!(
            contrib.summary.contains("0%"),
            "summary must mention 0%: {}",
            contrib.summary
        );
    }

    /// New-code coverage below threshold (but not zero) triggers floor.
    ///
    /// Why: partial coverage of new code below the threshold is still a gate violation.
    /// Test: new_code_pct=50%, threshold=80% → floor=REQUEST_CHANGES.
    #[test]
    fn coverage_policy_below_threshold() {
        let policy = make_policy(true);
        let report = report_with_pct(90.0);
        let contrib = evaluate_coverage(&policy, &report, Some(50.0), None);
        assert_eq!(
            contrib.floor,
            Some(Verdict::RequestChanges),
            "50% new-code coverage below 80% threshold must floor"
        );
        assert!(
            contrib.summary.contains("50.0%"),
            "summary must mention 50.0%: {}",
            contrib.summary
        );
    }

    /// Net coverage drop exceeding max_net_drop_pct triggers floor.
    ///
    /// Why: a PR that causes a large regression in overall coverage must be flagged.
    /// Test: net=80%, baseline=85%, drop=5pp, max=1pp → floor=REQUEST_CHANGES.
    #[test]
    fn coverage_policy_net_drop() {
        let policy = make_policy(true);
        let report = report_with_pct(80.0);
        let contrib = evaluate_coverage(&policy, &report, None, Some(85.0));
        assert_eq!(
            contrib.floor,
            Some(Verdict::RequestChanges),
            "5pp drop exceeding 1pp threshold must floor"
        );
        assert!(
            contrib.summary.contains("COVERAGE GATE TRIGGERED"),
            "summary must contain trigger marker: {}",
            contrib.summary
        );
    }

    /// All conditions pass — no floor, no grade ceiling.
    ///
    /// Why: the happy path must produce no floor so existing-passing PRs are
    /// unaffected even when coverage gating is enabled.
    /// Test: good new-code pct + no net drop → floor=None.
    #[test]
    fn coverage_policy_pass() {
        let policy = make_policy(true);
        let report = report_with_pct(90.0);
        let contrib = evaluate_coverage(&policy, &report, Some(90.0), Some(89.5));
        assert!(
            contrib.floor.is_none(),
            "all conditions pass → no floor: {:?}",
            contrib
        );
        assert!(contrib.grade_ceiling.is_none());
        assert!(
            contrib.summary.contains("Coverage thresholds met"),
            "summary should confirm pass: {}",
            contrib.summary
        );
    }

    /// When no new-code lines are instrumented (new_code_pct=None) and no baseline,
    /// the policy still produces no floor (cannot gate on data we don't have).
    ///
    /// Why: when the diff has no testable lines, we must not penalise the PR.
    /// Test: both optional inputs are None → floor=None even with policy enabled.
    #[test]
    fn coverage_policy_no_data_no_floor() {
        let policy = make_policy(true);
        let report = report_with_pct(80.0);
        let contrib = evaluate_coverage(&policy, &report, None, None);
        assert!(
            contrib.floor.is_none(),
            "no new-code data and no baseline → no floor"
        );
    }

    /// CoveragePolicy::default() produces enabled=false.
    ///
    /// Why: the default must be off so existing deployments are unaffected.
    /// Test: Default::default().enabled == false.
    #[test]
    fn coverage_policy_default_is_off() {
        let policy = CoveragePolicy::default();
        assert!(!policy.enabled, "default policy must be disabled (opt-in)");
    }
}
