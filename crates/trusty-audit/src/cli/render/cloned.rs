//! The lines a clone report prints (#5669).
//!
//! Why: its own file for the reason `super::verify` has one — `super` sits at
//! the 500-SLOC production cap, and #5669's `OVER BUDGET` arm is what pushed it
//! over. The clone report is the cohesive piece to lift out: every arm here
//! answers one question, which repositories are in the audit and which are not,
//! and none of the other renderers name a [`CloneState`].
//!
//! What: [`render`], an exhaustive match over [`CloneState`] per repository,
//! then the disk total and the gap lines. Exhaustive so a state added without a
//! way to display it is a compile error rather than a repository that silently
//! prints nothing.
//! Test: `crate::cli::cli_tests::rendering_a_clone_report_names_every_exclusion`.

use super::{count_of, human_bytes};
use crate::clone::CloneState;

/// What acquisition put on disk, and what it could not.
///
/// Test: `crate::cli::cli_tests::rendering_a_clone_report_names_every_exclusion`.
pub(super) fn render(report: &crate::clone::CloneReport) -> String {
    let mut out = String::new();
    for repo in &report.repos {
        // #5215: a repository that is NOT in the audit has to read as
        // excluded, never as a blank line the recipient scrolls past.
        let state = match &repo.state {
            CloneState::Cloned => "cloned".to_string(),
            CloneState::Reused => "already present".to_string(),
            CloneState::Failed(why) => format!("FAILED — {why}"),
            CloneState::Empty(why) => format!("NOTHING CLONED — {why}"),
            CloneState::Skipped(why) => format!("SKIPPED — {why}"),
            // #5669: distinct from FAILED, because nothing is wrong with the
            // repository — the run hit the ceiling the operator set.
            CloneState::BudgetExceeded {
                staged_bytes,
                budget_bytes,
            } => format!(
                "OVER BUDGET — stopped mid-clone at {} against a {} ceiling",
                human_bytes(*staged_bytes),
                human_bytes(*budget_bytes)
            ),
        };
        out.push_str(&format!("  {:<40} {state}\n", repo.name_with_owner));
    }
    out.push_str(&format!(
        "{} on disk, using {}{}.\n",
        count_of(
            report.repos.iter().filter(|r| r.state.is_usable()).count(),
            "repository",
            "repositories"
        ),
        // #5215 review: a walk that hit something unreadable produces a
        // floor, and saying "using X" of a floor is a confident number
        // nothing measured.
        if report.total_bytes_complete {
            ""
        } else {
            "at least "
        },
        human_bytes(report.total_bytes)
    ));
    for gap in &report.gaps {
        out.push_str(&format!("Gap: {gap}\n"));
    }
    out
}
