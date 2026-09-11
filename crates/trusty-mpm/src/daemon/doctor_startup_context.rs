//! Doctor probe: is this project's startup context inside its budget? (#7424)
//!
//! Why: #4513's audits are manual (2026-08-01, 2026-09-11) and the creep
//! between them is many small additions, so nothing catches it in between. The
//! 2026-09-11 pass measured turn-1 totals of 98k–107k tokens against a 50k
//! target. This row makes that measurement standing rather than occasional.
//!
//! What: reads this project's newest stored turn-1 readings from
//! [`crate::core::startup_context`] and renders the verdict. It opens NO
//! transcript — every number was measured by the session that owned it — so it
//! cannot read another project's session data, and it costs a handful of small
//! file reads.
//!
//! **Warn, never Fail.** The ceiling is a budget the operator sets in
//! `~/.trusty-tools/trusty-mpm/config.yaml`; a prompt the operator deliberately
//! grew is a preference, not a defect, and a doctor that hard-failed on one
//! would be reporting the wrong thing. A project with no reading yet is
//! reported as such rather than as a pass — nothing has been measured, so
//! nothing has passed.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_startup_context_tests.rs`.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::startup_context::{
    ResolvedStartupContext, StartupContextVerdict, evaluate_startup_context,
    resolve_startup_context, startup_context_for_project,
};

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "startup_context";

/// Where the breakdown of what the startup prompt is made of lives.
///
/// Why: the warning has one line and the useful next step is "which sources are
/// biggest" — a list this row has no room for and no business re-deriving.
/// Pointing at the reference keeps the warning one line and the breakdown in
/// one place.
/// What: the repo-relative path of the #4513 breakdown.
/// Test: `the_warning_points_at_the_breakdown`.
const BREAKDOWN_DOC: &str = "docs/reference/startup-context-budget.md";

/// Probe this project's startup-context budget.
///
/// Why/What: see the module doc. `project_dir` is the directory `tm doctor` was
/// run in; with none — the daemon's own `GET /api/v1/doctor` in a process whose
/// cwd names no project — there is nothing to scope a sample to and the row
/// reports that rather than sampling the whole machine.
/// Test: `no_project_dir_reports_no_sample`, `an_unmeasured_project_is_unknown`,
/// `a_project_inside_its_budget_passes`,
/// `a_project_over_its_budget_warns_and_never_fails`,
/// `a_disabled_config_reports_the_check_off`.
pub(super) fn check_startup_context(project_dir: Option<&Path>) -> DoctorCheck {
    let settings = resolve_startup_context(
        crate::core::trusty_tools_config::TrustyToolsConfig::load()
            .startup_context
            .as_ref(),
    );
    let Some(project_dir) = project_dir else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no project directory to scope a startup-context sample to; run `tm doctor` from \
             inside a project checkout"
                .to_owned(),
        );
    };
    let root = crate::core::paths::FrameworkPaths::default().root;
    let samples: Vec<u64> =
        startup_context_for_project(&root, project_dir, settings.sessions.max(1))
            .iter()
            .map(|record| record.tokens)
            .collect();
    verdict(&settings, &samples)
}

/// Pure verdict over an already-read sample.
///
/// Why: separating the reading from the judgement is what makes every arm
/// testable without a framework root or a live session — the split
/// [`crate::core::agent_cost::evaluate_cost`] uses for the same reason.
/// What: `Ok` when the check is off or the sample is inside the ceiling,
/// `Unknown` when nothing has been measured (an unmeasured project has not
/// passed), and `Warn` — never `Fail` — when the median or the latest reading
/// reaches it.
/// Test: `an_unmeasured_project_is_unknown`, `a_project_inside_its_budget_passes`,
/// `a_project_over_its_budget_warns_and_never_fails`,
/// `a_disabled_config_reports_the_check_off`,
/// `the_warning_points_at_the_breakdown`.
fn verdict(settings: &ResolvedStartupContext, samples: &[u64]) -> DoctorCheck {
    if !settings.enabled {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "startup-context budget disabled by `startup_context.enabled: false`".to_owned(),
        );
    }
    match evaluate_startup_context(samples, settings.ceiling_tokens) {
        StartupContextVerdict::NoSamples => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "no turn-1 startup reading recorded for this project yet — one lands after a \
                 managed session's first assistant turn; ceiling {} tokens",
                settings.ceiling_tokens
            ),
        ),
        StartupContextVerdict::Within {
            median,
            latest,
            ceiling,
        } => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "turn-1 startup context median {median} / latest {latest} tokens over {} \
                 session(s), under the {ceiling}-token ceiling",
                samples.len()
            ),
        ),
        StartupContextVerdict::Over {
            median,
            latest,
            ceiling,
        } => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "turn-1 startup context median {median} / latest {latest} tokens over {} \
                 session(s) reaches the {ceiling}-token ceiling — what the prompt is made of, \
                 and what to cut: {BREAKDOWN_DOC}",
                samples.len()
            ),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_startup_context_tests.rs"]
mod tests;
