//! Unit tests for [`super`] — the `startup_context` doctor row (#7424).

use super::*;
use crate::core::startup_context::{DEFAULT_CEILING_TOKENS, DEFAULT_SESSION_SAMPLE};

fn settings(enabled: bool, ceiling: u64) -> ResolvedStartupContext {
    ResolvedStartupContext {
        enabled,
        ceiling_tokens: ceiling,
        sessions: DEFAULT_SESSION_SAMPLE,
    }
}

/// Why: the daemon serves the same battery from a process whose cwd names no
/// project. Sampling the whole machine there would report another project's
/// numbers under this row's name.
/// Test: itself.
#[test]
fn no_project_dir_reports_no_sample() {
    let check = check_startup_context(None);
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(check.message.contains("no project directory"));
}

/// Why: a project nothing has measured has not passed. Reporting `Ok` there
/// would certify a budget no reading was ever compared against.
/// Test: itself.
#[test]
fn an_unmeasured_project_is_unknown() {
    let check = verdict(&settings(true, DEFAULT_CEILING_TOKENS), &[]);
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(check.message.contains("no turn-1 startup reading"));
    assert!(check.message.contains("50000"));
}

/// Why: the green path, and the one that must say what it measured rather than
/// only that it passed.
/// Test: itself.
#[test]
fn a_project_inside_its_budget_passes() {
    let check = verdict(&settings(true, 50_000), &[30_000, 28_000, 31_000]);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("median 30000"));
    assert!(check.message.contains("latest 30000"));
    assert!(check.message.contains("3 session(s)"));
}

/// Why (#7424): the ceiling is an operator budget, so exceeding it is a warning
/// about a preference, never a failed health check. A `Fail` here would drag
/// `tm doctor`'s overall verdict red on every machine that has not yet cut its
/// prompt down.
/// Test: itself.
#[test]
fn a_project_over_its_budget_warns_and_never_fails() {
    let check = verdict(&settings(true, 50_000), &[98_000, 107_000, 101_000]);
    assert_eq!(check.status, CheckStatus::Warn);
    assert_ne!(check.status, CheckStatus::Fail);
    assert!(check.message.contains("median 101000"));
    assert!(check.message.contains("50000-token ceiling"));
}

/// Why: the warning has one line, and the useful next step is the breakdown of
/// what the prompt is made of — which lives in a reference, not in this row.
/// Test: itself.
#[test]
fn the_warning_points_at_the_breakdown() {
    let check = verdict(&settings(true, 50_000), &[98_000]);
    assert!(
        check.message.contains(BREAKDOWN_DOC),
        "the warning must name the breakdown doc: {}",
        check.message
    );
}

/// Why: an operator who has decided the budget does not apply to their project
/// gets a row that says the check is off, not silence — a missing row reads as
/// a bug.
/// Test: itself.
#[test]
fn a_disabled_config_reports_the_check_off() {
    let check = verdict(&settings(false, 50_000), &[98_000]);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("disabled"));
}
