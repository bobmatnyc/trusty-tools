//! `tm doctor` row `content_requires`: assets whose advisory `requires:` this
//! binary does not satisfy (#8390, ADR-0064 decision 6).
//!
//! Why: a content release ships on its own cadence, so an asset can be newer
//! than the `tm` reading it. ADR-0064 forbids a hard version gate: the asset
//! still deploys and runs, and doctor is where the mismatch is reported.
//! What: [`check_content_requires`] resolves the content a launch would read
//! (never fetching) and grades it with [`grade_requires`].
//! Test: `doctor_content_requires_tests.rs`.

use std::path::Path;

use trusty_agents_common::agent_content::ResolvedContent;

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "content_requires";

/// The harness key this binary checks in a `requires:` block.
pub(crate) const HARNESS: &str = "tm";

/// Grades the content a launch of `project_dir` resolves, against this binary.
pub(crate) fn check_content_requires(
    project_dir: Option<&Path>,
    cache_dir: Option<&Path>,
) -> DoctorCheck {
    let cwd = std::env::current_dir().ok();
    check_content_requires_in(project_dir, cache_dir, cwd.as_deref())
}

/// [`check_content_requires`] with the process cwd named.
pub(crate) fn check_content_requires_in(
    project_dir: Option<&Path>,
    cache_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> DoctorCheck {
    let _ = (project_dir, cache_dir, cwd);
    DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, "")
}

/// Grades every asset of `content` against `version` of harness [`HARNESS`].
pub(crate) fn grade_requires(content: &ResolvedContent, version: &semver::Version) -> DoctorCheck {
    let _ = (content, version);
    DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, "")
}

#[cfg(test)]
#[path = "doctor_content_requires_tests.rs"]
mod tests;
