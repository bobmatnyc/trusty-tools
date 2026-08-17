//! Whether a `tga audit` output directory is worth believing.
//!
//! Why: #5494 gave this check a second caller. It used to be a step inside
//! `run_one`, asked once, immediately after the child that wrote the directory
//! exited. Resume asks it again, hours or days later, about a directory no
//! child in this run produced — and the two must ask exactly the same question,
//! because a resumed repository is carried over on the strength of this answer
//! alone. One implementation is what keeps "audited" meaning one thing.
//!
//! What: [`verify_output`], and the manifest prose it matches on.
//! Test: `super::run_tests::a_child_that_exits_zero_having_written_nothing_fails`,
//! `super::run_tests::a_manifest_reporting_a_failed_collect_stage_fails`,
//! `super::run_tests::ordinary_gaps_do_not_fail_the_repository`,
//! `super::run_tests::a_deleted_output_is_re_audited_rather_than_reported_complete`.

use std::path::Path;

use crate::manifest::AuditManifest;

/// The gap line `tga` writes when a collection stage failed but the sweep
/// continued (`tga::audit::gaps::sweep_gap_lines`, DOC-67 §9).
///
/// Why: `tga audit` exits 0 whenever the sweep COMPLETED, failed stages
/// included — its own docs say so. The failure reaches the manifest as prose,
/// which is the only channel tga offers today, so matching that prose is the
/// only way this client can tell "assessed" from "assessed nothing".
const COLLECT_FAILED_MARKER: &str = "stage `collect` did not complete";

/// What a zero exit is allowed to mean.
///
/// Why: the finding that made this necessary. `tga audit` returns `Ok` whenever
/// the sweep completed even with failed stages, so exit 0 alone does not say
/// anything was assessed — a collect stage that failed on auth, a rate limit or
/// an empty clone still exits 0. Believing that status is how the recipient gets
/// a report assessing nothing with every signal green.
///
/// # Postconditions
/// On `Ok`, `<output>/manifest.toml` exists, parses, names at least one
/// repository, and states no failed COLLECT stage. The returned gap lines are
/// whatever else the manifest stated. On `Err`, the string is a one-line reason
/// safe to show the recipient.
///
/// What: two checks of different confidence, and the difference is deliberate.
///
/// - **Structural**, and reliable: the manifest is there, parses, and names a
///   repository. A child that wrote nothing cannot pass this whatever tga's
///   wording does. This is also what makes the check usable on resume, where
///   the directory may have been deleted, emptied, or partly copied away since.
/// - **Textual**, and brittle: a gap line naming a failed `collect` stage. tga
///   owns that prose and could reword it, at which point this check silently
///   stops firing. It is a second layer over the structural check, never the
///   only one — and every other gap is recorded on the [`super::RepoRun`] and
///   rendered, so a reworded marker still reaches the operator as a stated gap
///   rather than disappearing. The durable fix is structured per-stage status in
///   the manifest, which is tga's to add.
///
/// Other failed stages (jira, dora, pr-metrics) are NOT failures here: DOC-67
/// §9 makes them named gaps on a report that is still worth delivering, and
/// failing on any gap would fail nearly every real engagement.
/// Test: `super::run_tests::a_child_that_exits_zero_having_written_nothing_fails`,
/// `super::run_tests::a_manifest_reporting_a_failed_collect_stage_fails`,
/// `super::run_tests::ordinary_gaps_do_not_fail_the_repository`.
pub(super) fn verify_output(output: &Path) -> Result<Vec<String>, String> {
    let manifest_path = output.join(AuditManifest::FILE_NAME);
    let manifest = match AuditManifest::load_if_present(&manifest_path) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return Err(format!(
                "`tga audit` exited 0 but wrote no manifest to {} — nothing was assessed",
                output.display()
            ));
        }
        Err(e) => {
            return Err(format!(
                "`tga audit` exited 0 but its manifest at {} cannot be read: {e}",
                manifest_path.display()
            ));
        }
    };
    if manifest.repositories.is_empty() {
        return Err(format!(
            "`tga audit` exited 0 but its manifest at {} names no repository — nothing was assessed",
            manifest_path.display()
        ));
    }
    if let Some(gap) = manifest
        .report
        .gaps
        .iter()
        .find(|g| g.contains(COLLECT_FAILED_MARKER))
    {
        return Err(format!(
            "`tga audit` exited 0 but collection did not complete: {gap}"
        ));
    }
    Ok(manifest.report.gaps)
}
