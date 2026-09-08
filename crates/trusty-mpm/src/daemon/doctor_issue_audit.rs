//! The `tm doctor` `issue_audit_recent` check — ticket hygiene on this week's
//! issues (#7097).
//!
//! Why: `tm issue audit <N>` catches a violation only when someone runs it on
//! that issue. This is the sweep that runs without being asked, over the issues
//! recent enough that fixing them is still cheap.
//!
//! What: [`check_issue_audit_recent`] audits every OPEN issue created in the
//! last [`AUDIT_WINDOW_DAYS`] days off the async executor;
//! [`build_issue_audit_check`] folds the outcome into a [`DoctorCheck`].
//!
//! # Never FAIL
//!
//! Ticket hygiene is not a broken stack, so the worst verdict this check can
//! reach is `Warn`, naming the failing issue numbers. A red `tm doctor` is a
//! signal an operator acts on immediately; a missing milestone is not that.
//!
//! # The Fail-Open Check
//!
//! A `gh` that is absent, unauthenticated, or erroring means the audit did not
//! run — which is NOT the same as running and finding nothing. That folds to
//! [`CheckStatus::Unknown`] with the error quoted, never to `Ok`.
//! `issue_audit_gh_failure_is_not_a_pass` is the test that holds this.
//!
//! Test: `doctor_issue_audit_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::gh_identity::GhEnv;
use crate::core::issue_audit::{IssueAudit, audit_issue};
use crate::core::issue_audit_gh::{AuditWindow, list_open_issues};
use crate::core::trusty_tools_config::{TrustyToolsConfig, resolve_ticketing};

/// The check's stable name.
pub(super) const CHECK_NAME: &str = "issue_audit_recent";

/// How far back the sweep reaches.
///
/// Why: an issue filed this week is still being worked, so a missing milestone
/// or project is cheap to add. Older issues are backlog archaeology, and
/// reporting them every run would train an operator to ignore the row.
pub const AUDIT_WINDOW_DAYS: i64 = 7;

/// How many failing issues the warning names before it starts counting.
///
/// Why: a busy week can leave dozens of violations, and one doctor row that
/// lists all of them scrolls the rest of the report away. `tm issue audit
/// --recent` is where the full table belongs.
/// Test: `issue_audit_caps_the_named_failures`.
const NAMED_LIMIT: usize = 8;

/// Audit the recently-opened issues and report the result.
///
/// Why: see the module doc — the unprompted half of #7097.
/// What: off-loads the blocking `gh issue list` to `spawn_blocking` (the
/// pattern `check_gh_account` established, so the async executor is not
/// stalled), then folds via [`build_issue_audit_check`]. `project_dir` is the
/// directory `gh` runs in, which is what selects the repository.
/// Test: `build_issue_audit_check` covers every branch of the fold.
pub(super) async fn check_issue_audit_recent(project_dir: Option<&Path>) -> DoctorCheck {
    let dir: Option<PathBuf> = project_dir.map(Path::to_path_buf);
    let probe = tokio::task::spawn_blocking(move || audit_recent_window(dir.as_deref()))
        .await
        .unwrap_or_else(|e| Err(format!("issue-audit task failed: {e}")));
    build_issue_audit_check(probe)
}

/// Audit every OPEN issue created within [`AUDIT_WINDOW_DAYS`] days.
///
/// Why: the whole blocking half in one place, so the async wrapper above holds
/// no logic of its own.
/// What: resolves the operator's `agents.ticketing` standard, computes the
/// window's start date in UTC, lists the matching open issues with an ambient
/// `gh` identity, and audits each. Every failure — a malformed config block, a
/// missing `gh`, a parse error — becomes an `Err(String)`, which the fold turns
/// into UNDETERMINED rather than a pass.
/// Test: exercised live; the fold's branches are unit-tested.
fn audit_recent_window(project_dir: Option<&Path>) -> Result<Vec<IssueAudit>, String> {
    let ticketing = resolve_ticketing(&TrustyToolsConfig::load())
        .map_err(|e| format!("the agents.ticketing block did not resolve: {e}"))?;
    let since = (chrono::Utc::now() - chrono::Duration::days(AUDIT_WINDOW_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let window = AuditWindow::Since(since);
    let facts =
        list_open_issues(&window, project_dir, &GhEnv::default()).map_err(|e| e.to_string())?;
    Ok(facts.iter().map(|f| audit_issue(f, &ticketing)).collect())
}

/// Fold an audit outcome into a [`DoctorCheck`] (pure).
///
/// Why: keeping the verdict pure makes all four branches — clean sweep, empty
/// window, violations found, and could-not-run — testable with no `gh`.
/// What: `Err` → [`CheckStatus::Unknown`] quoting the reason (see "The
/// Fail-Open Check" in the module doc); violations → `Warn` naming every
/// failing issue number and the requirement each missed; otherwise `Ok` with
/// the audited count. Never [`CheckStatus::Fail`].
/// Test: `issue_audit_clean_sweep_is_ok`, `issue_audit_empty_window_is_ok`,
/// `issue_audit_violations_warn_with_the_numbers`,
/// `issue_audit_gh_failure_is_not_a_pass`,
/// `issue_audit_check_never_fails`.
pub(super) fn build_issue_audit_check(probe: Result<Vec<IssueAudit>, String>) -> DoctorCheck {
    let audits = match probe {
        Ok(audits) => audits,
        // #7097: the audit did not run. That is not "ran and found nothing".
        Err(reason) => {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Unknown,
                format!(
                    "could not audit recent issues — ticket hygiene UNKNOWN, not clean \
                     ({reason}). `gh` must be installed and authenticated in the project \
                     directory; run `tm issue audit --recent 20` by hand to see the real state."
                ),
            );
        }
    };
    let failing: Vec<&IssueAudit> = audits.iter().filter(|a| a.failed()).collect();
    if failing.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "{} open issue(s) from the last {AUDIT_WINDOW_DAYS} days meet the ticketing \
                 standard",
                audits.len()
            ),
        );
    }
    // #7097: one doctor row, not a backlog dump. The first `NAMED_LIMIT`
    // failures name their missed requirements; the rest are counted, because a
    // 42-issue list scrolls the whole report off the screen and `tm issue audit
    // --recent` prints the full table anyway.
    let mut detail = failing
        .iter()
        .take(NAMED_LIMIT)
        .map(|a| format!("#{} ({})", a.number, a.failing_requirements().join(", ")))
        .collect::<Vec<_>>()
        .join("; ");
    if let Some(rest) = failing.len().checked_sub(NAMED_LIMIT).filter(|n| *n > 0) {
        detail.push_str(&format!("; and {rest} more"));
    }
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "{} of {} open issue(s) from the last {AUDIT_WINDOW_DAYS} days violate the \
             ticketing standard: {detail}. Fix with `gh issue edit <N> --milestone … \
             --add-project …`, or run `tm issue audit <N>` for the detail.",
            failing.len(),
            audits.len()
        ),
    )
}

#[cfg(test)]
#[path = "doctor_issue_audit_tests.rs"]
mod tests;
