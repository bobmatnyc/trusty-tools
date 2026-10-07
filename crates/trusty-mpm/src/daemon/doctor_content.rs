//! `tm doctor` row `content`: the instructional-content source and the
//! installed pin's health (ADR-0064, #8378 PR-C).
//!
//! Why: #8389 — doctor shows the content version and the binary version as two
//! facts that may differ without either being an error.
//! What: [`check_content`] grades [`content_status`]: a dev checkout or a
//! verified bundle is OK. No bundle installed is INFO (an `Ok` row whose
//! message starts `info:`) while this binary still compiles its content in,
//! and WARN once ADR-0064 PHASE_1 removes it; either way the message names
//! `tm content update` and the offline `tm content install --from`. A lock or
//! bundle that fails verification is FAIL, or WARN when a dev checkout still
//! serves. The message carries the source (`dev`/`bundle`/`none`), the tag,
//! the sha256 and the binary version. Read-only.
//! A source whose PM package this binary cannot parse is FAIL (#9012).
//! #9396: never fetches; a missing lock is WARN naming the same remedy as
//! every not-installed error.
//! Test: `doctor_content_tests.rs`.

use std::path::Path;

use trusty_agents_common::agent_content::AgentContentError;

use crate::content::status::{ContentStatus, content_status};
use crate::core::content_source::{Fetch, FrameworkContent, resolve_for_in};
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "content";

/// Grades the content cache at `cache_dir`, and the checkout above
/// `project_dir`. `cache_dir` is `None` when no home directory resolves.
///
/// Test: `content_row_is_ok_for_a_verified_bundle`,
/// `content_row_warns_when_nothing_is_installed_after_phase_1`,
/// `content_row_fails_on_a_tampered_bundle`.
pub(crate) fn check_content(project_dir: Option<&Path>, cache_dir: Option<&Path>) -> DoctorCheck {
    let cwd = std::env::current_dir().ok();
    check_content_in(project_dir, cache_dir, cwd.as_deref())
}

/// [`check_content`] with the process cwd named, which the launch resolver
/// falls back to.
///
/// Test: `the_content_row_never_fetches`.
pub(crate) fn check_content_in(
    project_dir: Option<&Path>,
    cache_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> DoctorCheck {
    let Some(cache_dir) = cache_dir else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no home directory resolves, so the content cache cannot be located",
        );
    };
    let row = grade(&content_status(cache_dir, project_dir));
    if row.status == CheckStatus::Fail {
        return row;
    }
    match unusable_package(project_dir, cache_dir, cwd) {
        Some(message) => DoctorCheck::new(CHECK_NAME, CheckStatus::Fail, message),
        None => row,
    }
}

/// The refusal message when the content a launch of `project_dir` resolves has
/// a PM package this binary cannot parse (#9012), else `None`.
///
/// Why: bundle integrity alone grades a verified bundle Ok while every launch
/// is refused with `AgentContentError::Invalid`.
/// What: loads the content through the launch path's resolver, with
/// [`Fetch::Never`] (#9396: doctor is read-only). Only `Invalid` counts; a
/// missing or unreadable source is already graded by [`grade`].
/// Test: `content_row_fails_when_the_pm_package_does_not_parse`,
/// `the_content_row_never_fetches`.
fn unusable_package(
    project_dir: Option<&Path>,
    cache_dir: &Path,
    cwd: Option<&Path>,
) -> Option<String> {
    let loaded = resolve_for_in(project_dir, cwd, Some(cache_dir), Fetch::Never)
        .and_then(|content| FrameworkContent::load(&content));
    match loaded {
        Err(err @ AgentContentError::Invalid { .. }) => Some(err.to_string()),
        _ => None,
    }
}

/// The row for `status`. Nothing installed and no checkout is WARN: since
/// #9012 the binary compiles in no instructional content.
fn grade(status: &ContentStatus) -> DoctorCheck {
    let message = status.lines().join("; ");
    let grade = if status.installed.is_ok() {
        CheckStatus::Ok
    } else if status.not_installed() {
        if status.serves() {
            // A dev checkout serves.
            CheckStatus::Ok
        } else {
            CheckStatus::Warn
        }
    } else if status.serves() {
        CheckStatus::Warn
    } else {
        CheckStatus::Fail
    };
    DoctorCheck::new(CHECK_NAME, grade, message)
}

#[cfg(test)]
#[path = "doctor_content_tests.rs"]
mod tests;
