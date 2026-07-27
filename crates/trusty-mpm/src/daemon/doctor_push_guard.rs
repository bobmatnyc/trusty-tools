//! Doctor probe: is the #2867 cross-branch push guard installed here? (#2867)
//!
//! Why: the guard is installed on the CLONE path only, so every base clone
//! that already existed when the guard shipped — including this repo's own
//! `.base`, shared by ~95 worktrees — gets nothing from merging it. Silent
//! non-coverage is the worst outcome: an operator who read the CHANGELOG
//! reasonably believes they are protected. This check is the discovery half of
//! the retrofit path; `tm repair push-guard` is the action half.
//! What: classifies the current project's `pre-push` slot with the read-only
//! [`inspect_pre_push_guard`] and maps it to a `DoctorCheck`. Warn-only by
//! convention — doctor reports, it never writes into a repository. A `Foreign`
//! slot is also only a Warn: another hook manager owning `pre-push` is a
//! legitimate configuration, not a fault.
//! Test: `crates/trusty-mpm/src/daemon/doctor_push_guard_tests.rs`.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::push_guard::{GuardState, inspect_pre_push_guard};

/// Probe the current project for the #2867 cross-branch push guard.
///
/// Why/What: see the module doc. The remediation string names the exact
/// command, because the whole point of this check is that the gap was
/// previously undiscoverable.
/// Test: `ok_when_no_project_dir`, `ok_outside_a_git_repo`,
/// `warns_when_missing_and_names_the_retrofit_command`, `ok_once_installed`,
/// `warns_when_an_older_revision_is_installed`,
/// `warns_on_a_foreign_hook_without_claiming_protection`.
pub(super) fn check_push_guard(project_dir: Option<&Path>) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            "push_guard",
            CheckStatus::Ok,
            "no project directory supplied — push-guard check not applicable",
        );
    };
    if !project.join(".git").exists() {
        return DoctorCheck::new(
            "push_guard",
            CheckStatus::Ok,
            "project is not a git working tree — push-guard check not applicable",
        );
    }

    match inspect_pre_push_guard(project) {
        GuardState::Current(path) => DoctorCheck::new(
            "push_guard",
            CheckStatus::Ok,
            format!(
                "cross-branch push guard active at {} (covers every worktree of this clone)",
                path.display()
            ),
        ),
        GuardState::Missing(path) => DoctorCheck::new(
            "push_guard",
            CheckStatus::Warn,
            format!(
                "no cross-branch push guard at {} — a worktree tracking a foreign branch can \
                 force-push over that branch's reviewed lineage (#2867). Install it (idempotent, \
                 covers every worktree of this clone at once): `tm repair push-guard`",
                path.display()
            ),
        ),
        GuardState::Outdated(path) => DoctorCheck::new(
            "push_guard",
            CheckStatus::Warn,
            format!(
                "the cross-branch push guard at {} is an older revision (#2867). Upgrade it: \
                 `tm repair push-guard`",
                path.display()
            ),
        ),
        GuardState::Foreign(reason) => DoctorCheck::new(
            "push_guard",
            CheckStatus::Warn,
            format!(
                "cross-branch push guard NOT installed — {reason}. trusty-mpm never overwrites a \
                 pre-push hook it does not own (#2867); to get the protection, chain the guard \
                 from your own hook or resolve the conflict by hand"
            ),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_push_guard_tests.rs"]
mod tests;
