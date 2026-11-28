//! Post-placement version checks for `tctl upgrade` (#8642).
//!
//! Why: moved out of `upgrade.rs` unchanged (#11587) to keep it under the
//! SLOC cap when the sign-before-restart gate landed.
//! What: [`verify_applied`] and [`placed_state_error`].
//! Test: `upgrade_tests.rs` (`verify_applied_*`, `placed_state_error_*`).

use super::UpdateCandidate;

/// Check that a placed binary is the release expected and advances the install.
///
/// Why (#8642): `tctl upgrade` placed tga 8.0.0 over 8.0.0 and reported it
/// upgraded; neither branch compared what landed against what was installed.
/// What: parses the version from the `--version` line. With `tag_version`
/// (the prebuilt branch) it must equal the tag. With an installed version it
/// must be strictly newer (semver; string inequality when either does not
/// parse). Returns the applied version.
/// Test: `tests::verify_applied_rejects_a_version_that_does_not_match_the_tag`,
/// `tests::verify_applied_rejects_a_non_advancing_upgrade`,
/// `tests::verify_applied_accepts_an_advance_that_lags_latest`.
pub(super) fn verify_applied(
    c: &UpdateCandidate,
    tag_version: Option<&str>,
    reported_line: &str,
) -> anyhow::Result<String> {
    let name = &c.crate_name;
    let reported = super::super::update_engine::extract_version_from_line(reported_line)
        .ok_or_else(|| anyhow::anyhow!("{name}: `--version` printed {reported_line:?}"))?;
    if let Some(tag) = tag_version {
        anyhow::ensure!(
            reported == tag,
            "{name}: placed binary reports {reported}, but the release tag is {tag}"
        );
    }
    if let Some(installed) = c.installed.as_deref() {
        let advanced = match (
            semver::Version::parse(&reported),
            semver::Version::parse(installed),
        ) {
            (Ok(now), Ok(was)) => now > was,
            _ => reported != installed,
        };
        anyhow::ensure!(
            advanced,
            "{name} did not advance: installed {installed}, now reports {reported} \
             (crates.io latest {}); no prebuilt or cargo build newer than installed",
            c.latest
        );
    }
    Ok(reported)
}

/// Name the on-disk state a failed post-placement check leaves behind.
///
/// Why (#8642 review): both install branches replace the binary BEFORE
/// [`verify_applied`] runs, and neither keeps the previous copy. An error that
/// said only "did not advance" or "tag mismatch" implied nothing changed on
/// disk, when the old binary is already gone.
/// What: appends the replaced path and the version that binary now reports
/// (or its raw `--version` line when none parses) to the verification error.
/// Test: `tests::placed_state_error_names_the_replaced_path_and_version`.
pub(super) fn placed_state_error(
    err: anyhow::Error,
    bin_path: &std::path::Path,
    reported_line: &str,
) -> anyhow::Error {
    let now = super::super::update_engine::extract_version_from_line(reported_line)
        .unwrap_or_else(|| format!("{reported_line:?}"));
    anyhow::anyhow!(
        "{err}. {} was already replaced and now holds {now}; the previous \
         binary was not kept",
        bin_path.display()
    )
}
