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

use crate::error::AuditError;
use crate::manifest::AuditManifest;

/// The gap line `tga` writes when a collection stage failed but the sweep
/// continued (`tga::audit::gaps::sweep_gap_lines`, DOC-67 §9).
///
/// Why: `tga audit` exits 0 whenever the sweep COMPLETED, failed stages
/// included — its own docs say so. The failure reaches the manifest as prose,
/// which is the only channel tga offers today, so matching that prose is the
/// only way this client can tell "assessed" from "assessed nothing".
const COLLECT_FAILED_MARKER: &str = "stage `collect` did not complete";

/// The words `tga` opens a stale-refs gap line with (`tga::audit::gaps`'s
/// `STALE_FETCH_HEADLINE`, #6782).
///
/// Why: a repository fetched from nothing still produces a full report — every
/// section populated, every figure describing whatever the clone held when it
/// was made. That is worse than an empty section, because nothing on the page
/// looks wrong. The index states it per repository so a reader sees it before
/// they open the report.
/// What: matched against the gap lines [`verify_output`] returns, the same
/// textual seam and the same brittleness as [`COLLECT_FAILED_MARKER`] — this
/// crate does not depend on `tga`, so its prose is the only channel. A reworded
/// marker stops the headline, never the gap line itself, which is rendered
/// either way.
pub(crate) const STALE_FETCH_MARKER: &str = "git history is stale: fetch failed";

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

/// The file a repository's output directory carries once its audit finished.
///
/// Why (#6141): `manifest.toml` says a `tga audit` child wrote something, not
/// that the run got through the rest of the repository — the grounding pass and
/// the inference stamp both run after it, and both edit that same manifest. A
/// run killed in between leaves a directory holding real collection data and no
/// finished report, which at a glance is what a completed repository looks
/// like. Completion was recorded only in `state/run-progress.toml`, one document
/// for the whole sweep, so nothing about the DIRECTORY said which it was.
/// What: a marker file inside the output directory itself, so the answer travels
/// with the data — a directory copied, inspected or re-rendered on its own still
/// says whether it is finished.
pub(super) const COMPLETION_FILE: &str = "audit-complete.toml";

/// Record that this repository's output directory is finished.
///
/// Why/What: see [`COMPLETION_FILE`]. Written LAST, after the child, the
/// grounding pass and the inference stamp, so its presence means every writer of
/// this directory has run. Written atomically, because a half-written marker on
/// a killed run is the ambiguity this removes.
/// Test: `super::run_tests::an_interrupted_repository_is_re_audited_not_carried_over`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] naming the marker that could not be written. This is
/// a sweep-level failure rather than a per-repository one: a run that cannot
/// record completion would carry the repository over next time on the strength
/// of a marker that is not there, which is the defect in reverse.
pub(super) fn mark_complete(output: &Path, repo: &str) -> Result<(), AuditError> {
    let text = format!(
        "repository = {repo:?}\nfinished = {finished:?}\ntrusty_audit = {version:?}\n",
        finished = crate::index_report::local_now(),
        version = env!("CARGO_PKG_VERSION"),
    );
    crate::workdir::write_atomically(&output.join(COMPLETION_FILE), &text)
}

/// Whether this output directory carries the completion marker.
///
/// A directory with no marker is not necessarily broken — one written before
/// this file existed has none — so the caller decides what to do about it.
/// [`super::checkpoint::plan`] re-audits it, which is the safe direction: it
/// costs one repository's collection once, where believing it costs a report
/// that was never rendered.
/// Test: `super::run_tests::an_interrupted_repository_is_re_audited_not_carried_over`.
pub(super) fn is_complete(output: &Path) -> bool {
    output.join(COMPLETION_FILE).is_file()
}
