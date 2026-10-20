//! `tm doctor` row `content_requires`: assets whose advisory `requires:` this
//! binary does not satisfy (#8390, ADR-0064 decision 6).
//!
//! Why: a content release ships on its own cadence, so an asset can be newer
//! than the `tm` reading it. ADR-0064 forbids a hard version gate: the asset
//! still deploys and runs, and doctor is where the mismatch is reported.
//! What: [`check_content_requires`] resolves the content a launch would read
//! (never fetching) and grades it with [`grade_requires`]. The row is WARN at
//! worst — never FAIL — so it cannot turn an advisory field into a gate.
//! Test: `doctor_content_requires_tests.rs`.

use std::path::Path;

use trusty_agents_common::agent_content::ResolvedContent;

use crate::content::requires::parse_requires;
use crate::core::content_source::{Fetch, resolve_for_in};
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "content_requires";

/// The harness key this binary checks in a `requires:` block.
pub(crate) const HARNESS: &str = "tm";

/// The content classes whose files may carry `requires:`.
const CLASSES: [&str; 3] = ["agents", "skills", "instructions"];

/// How many findings the message names before it summarises the rest.
const SHOWN: usize = 8;

/// Grades the content a launch of `project_dir` resolves, against this binary.
///
/// Test: `the_row_never_fails_when_no_content_resolves`.
pub(crate) fn check_content_requires(
    project_dir: Option<&Path>,
    cache_dir: Option<&Path>,
) -> DoctorCheck {
    let cwd = std::env::current_dir().ok();
    check_content_requires_in(project_dir, cache_dir, cwd.as_deref())
}

/// [`check_content_requires`] with the process cwd named, which the launch
/// resolver falls back to.
///
/// What: resolves with [`Fetch::Never`] (doctor is read-only). No content, or
/// no parseable binary version, is WARN naming why the row did not check —
/// the `content` row grades the source itself.
/// Test: `the_row_never_fails_when_no_content_resolves`.
pub(crate) fn check_content_requires_in(
    project_dir: Option<&Path>,
    cache_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> DoctorCheck {
    let not_checked = |why: String| {
        DoctorCheck::new(CHECK_NAME, CheckStatus::Warn, format!("not checked: {why}"))
    };
    let Some(cache_dir) = cache_dir else {
        return not_checked("no home directory resolves, so no content does".to_owned());
    };
    let version = match semver::Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(version) => version,
        Err(e) => return not_checked(format!("this binary's version is not SemVer: {e}")),
    };
    match resolve_for_in(project_dir, cwd, Some(cache_dir), Fetch::Never) {
        Ok(content) => grade_requires(&content, &version),
        Err(e) => not_checked(format!(
            "no content source resolves ({e}); see the `content` row"
        )),
    }
}

/// Grades every asset of `content` against `version` of harness [`HARNESS`].
///
/// Why: acceptance 1 of #8390 — a mismatch is reported, never enforced.
/// What: reads `requires:` from every `.md` under [`CLASSES`]. An entry for
/// [`HARNESS`] that `version` does not satisfy, or a block that does not
/// parse, makes the row WARN naming the asset; entries for other harnesses are
/// counted and left to that harness. Otherwise OK with the counts. An asset
/// the source lists but cannot read is WARN too. Never FAIL.
/// Test: `an_unmet_requires_still_deploys_and_runs_and_doctor_only_warns`,
/// `a_satisfied_or_foreign_requires_is_ok`,
/// `a_malformed_requires_warns_naming_the_asset`.
pub(crate) fn grade_requires(content: &ResolvedContent, version: &semver::Version) -> DoctorCheck {
    let (mut satisfied, mut foreign) = (0usize, 0usize);
    let mut findings = Vec::new();
    for class in CLASSES {
        let keys = match content.list(class) {
            Ok(keys) => keys,
            Err(e) => {
                findings.push(format!("{class}/ not listed: {e}"));
                continue;
            }
        };
        for key in keys.into_iter().filter(|k| k.ends_with(".md")) {
            let requires = match content.read_to_string(&key) {
                Ok(text) => parse_requires(&text),
                Err(e) => {
                    findings.push(format!("{key} not read: {e}"));
                    continue;
                }
            };
            match requires {
                Ok(entries) => {
                    for entry in entries {
                        if entry.harness != HARNESS {
                            foreign += 1;
                        } else if entry.satisfied_by(version) {
                            satisfied += 1;
                        } else {
                            findings.push(format!("{key} ({HARNESS} {})", entry.requirement));
                        }
                    }
                }
                Err(e) => findings.push(format!("{key} (unreadable `requires:`: {e})")),
            }
        }
    }
    let counts = format!("{satisfied} satisfied, {foreign} for another harness");
    if findings.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!("{HARNESS} {version}: every `requires:` met ({counts})"),
        );
    }
    let total = findings.len();
    let mut shown = findings
        .into_iter()
        .take(SHOWN)
        .collect::<Vec<_>>()
        .join("; ");
    if total > SHOWN {
        shown.push_str(&format!("; and {} more", total - SHOWN));
    }
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "advisory: {total} asset requirement(s) {HARNESS} {version} does not meet; \
             each asset still deploys and runs (ADR-0064): {shown} ({counts}). \
             Upgrade {HARNESS}, or pin an older release with `tm content update --content-ref <tag>`"
        ),
    )
}

#[cfg(test)]
#[path = "doctor_content_requires_tests.rs"]
mod tests;
