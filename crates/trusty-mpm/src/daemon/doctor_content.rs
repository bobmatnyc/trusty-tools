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
//! Test: `doctor_content_tests.rs`.

use std::path::Path;

use crate::content::BUILTIN_CONTENT_EMBEDDED;
use crate::content::status::{ContentStatus, content_status};
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "content";

/// Grades the content cache at `cache_dir`, and the checkout above
/// `project_dir`. `cache_dir` is `None` when no home directory resolves.
///
/// Test: `content_row_is_ok_for_a_verified_bundle`,
/// `content_row_is_info_when_nothing_is_installed_before_phase_1`,
/// `content_row_warns_when_nothing_is_installed_after_phase_1`,
/// `content_row_fails_on_a_tampered_bundle`.
pub(crate) fn check_content(project_dir: Option<&Path>, cache_dir: Option<&Path>) -> DoctorCheck {
    let Some(cache_dir) = cache_dir else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no home directory resolves, so the content cache cannot be located",
        );
    };
    grade(
        &content_status(cache_dir, project_dir),
        BUILTIN_CONTENT_EMBEDDED,
    )
}

/// The row for `status`; `builtin_embedded` is [`BUILTIN_CONTENT_EMBEDDED`].
fn grade(status: &ContentStatus, builtin_embedded: bool) -> DoctorCheck {
    let message = status.lines().join("; ");
    let grade = if status.installed.is_ok() {
        CheckStatus::Ok
    } else if status.not_installed() {
        if status.serves() {
            // A dev checkout serves.
            CheckStatus::Ok
        } else if let Some(info) = status.builtin_info(builtin_embedded) {
            return DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, format!("{info}; {message}"));
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
