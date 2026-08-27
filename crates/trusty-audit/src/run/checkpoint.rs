//! The run checkpoint: what a sweep has already done, and what a re-run skips.
//!
//! Why: #5494. The progress record used to be written ONCE, after every
//! repository had finished, so a crash, a timeout or a Ctrl-C mid-sweep left no
//! record of partial progress at all — and a re-run redid every repository,
//! including ones that had already spent four hours inside `tga audit`. This
//! module makes the record incremental and makes the sweep re-entrant against
//! it.
//!
//! What: [`RunProgress`], the `state/`[`PROGRESS_FILE`] document, and [`plan`],
//! which decides per selected repository whether an earlier sweep's result may
//! be carried over. [`write_progress`] publishes atomically;
//! [`read_progress`] reads what is there.
//! Test: `super::run_tests`.
//!
//! ## Why the checkpoint is per repository, not per stage
//!
//! `tga audit` runs nine stages per repository and, since #5823, relays each
//! one — so recording stage granularity would be nearly free. It would also be
//! unusable: `tga audit`'s arguments are `--org --title --analyst --client
//! --output --weeks` (`tga::commands::audit::AuditArgs`) and there is no way to
//! ask it to start at stage 5. A checkpoint records a position only so a later
//! run can re-enter there, and the only re-entry point this crate has is a
//! fresh `tga audit` child over one repository. Stage events remain what they
//! are — live display, through [`crate::progress`] — and the checkpoint records
//! the unit that can actually be resumed.
//!
//! ## What is not here: a lock
//!
//! `trusty_common::file_lock::with_exclusive_lock` guards load-mutate-save
//! against a second PROCESS, and this record is not exposed to that race. One
//! sweep holds its results in memory and republishes the whole document, so
//! there is no read-modify-write window for another writer to land inside, and
//! every other reader (`package`, the guided flow) only reads. Two concurrent
//! `trusty-audit run` invocations against one working directory would collide
//! long before the checkpoint: they share `out/<stem>/`, `logs/<stem>.log`,
//! `extract/<stem>.db` and the generated tga config, all of which truncate. A
//! lock on this file would serialise the one write that is already atomic while
//! leaving that collision untouched, so the shared working directory stays the
//! open question `crate::workdir` records rather than something answered here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::github_issues::GithubCredentialRecord;
use super::verify::is_complete;
use super::{RepoRun, RunReport, RunStatus, SelectedRepo, stem, verify_output};
use crate::error::AuditError;
use crate::workdir::{self, Area, WorkDir};

/// File under `state/` recording what the sweep has done, per repository.
pub const PROGRESS_FILE: &str = "run-progress.toml";

/// Where the run-progress record is written.
pub fn progress_path(work: &WorkDir) -> PathBuf {
    work.path(Area::State).join(PROGRESS_FILE)
}

/// The `state/run-progress.toml` document.
///
/// Why: the file has to answer two different questions, and before #5494 it
/// answered only one. "What happened to each repository" is [`RunReport`], and
/// it is what `package` assembles from. "Did the sweep that wrote this actually
/// finish" is [`RunProgress::complete`], and without it an incremental record
/// is indistinguishable from a finished short one — which is how a crashed
/// sweep would otherwise be packaged and sent as a whole engagement.
///
/// What: the report's fields plus the completion flag. `complete` defaults to
/// `false` when absent, so a record written before this field existed reads as
/// unfinished: the resume path then re-verifies every entry against what is on
/// disk and re-collects whatever does not hold up, which is the safe direction.
/// Test: `super::run_tests::the_checkpoint_advances_with_the_sweep_and_completes_only_at_the_end`,
/// `crate::session::session_tests::packaging_an_unfinished_sweep_is_refused`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunProgress {
    /// Whether the sweep that wrote this reached the end of its selection.
    ///
    /// `false` means the record is a checkpoint of a sweep still running, or of
    /// one that died. It is never a reason to discard the entries — they are
    /// what a re-run resumes from.
    #[serde(default)]
    pub complete: bool,
    /// The verdict over the entries recorded SO FAR.
    pub status: RunStatus,
    /// One entry per repository that has finished, in selection order.
    pub repos: Vec<RepoRun>,
    /// What this sweep recorded about the `gh`-derived GitHub credential it
    /// resolved (#5980 — the re-resolution/account-switch gap).
    ///
    /// Why: packaging can run as a separate process, long after the sweep and
    /// possibly under a different `gh` account — see
    /// `super::github_issues::GithubAccess::fingerprint`'s docs. `None` is
    /// reserved for a checkpoint written before this field existed;
    /// `super::github_issues::verify_unchanged` treats that as unverifiable
    /// rather than as either recorded state, and proceeds with a stated gap
    /// rather than refusing every pre-existing engagement outright.
    /// What: never the raw token — only [`GithubCredentialRecord`]'s
    /// non-reversible fingerprint, or the explicit "no token" state.
    /// Test: `super::run_tests::child_output_scrubber_includes_the_github_token`
    /// (sweep-side), `github_issues_tests` (the verification itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_credential: Option<GithubCredentialRecord>,
}

impl RunProgress {
    /// A checkpoint of a sweep that is still working through its selection.
    pub(crate) fn checkpoint(repos: &[RepoRun], github_credential: GithubCredentialRecord) -> Self {
        Self::from_report(RunReport::of(repos.to_vec()), false, github_credential)
    }

    /// The record of a sweep that reached the end of its selection.
    pub(crate) fn finished(report: &RunReport, github_credential: GithubCredentialRecord) -> Self {
        Self::from_report(report.clone(), true, github_credential)
    }

    fn from_report(
        report: RunReport,
        complete: bool,
        github_credential: GithubCredentialRecord,
    ) -> Self {
        Self {
            complete,
            status: report.status,
            repos: report.repos,
            github_credential: Some(github_credential),
        }
    }

    /// The per-repository results, without the completion flag.
    pub fn report(&self) -> RunReport {
        RunReport::of(self.repos.clone())
    }
}

/// Publish the checkpoint, refusing rather than downgrading a failure.
///
/// Why: this runs after EVERY repository now, so it is the branch #5494's
/// fail-open check names. A write that failed and was warned about would leave
/// the sweep continuing for hours against a record it cannot update, and the
/// repositories it finished meanwhile would be reported as audited by a run
/// nothing on disk describes. So the error propagates and the sweep stops at
/// the first repository whose completion could not be recorded.
/// What: renders the document and hands it to
/// [`workdir::write_atomically`] — a `kill -9` between two repositories finds
/// either the previous whole checkpoint or the new one, never a prefix.
/// Test: `super::run_tests::a_checkpoint_that_cannot_be_written_stops_the_sweep`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when the record cannot be rendered or published.
pub(crate) fn write_progress(work: &WorkDir, progress: &RunProgress) -> Result<(), AuditError> {
    let path = progress_path(work);
    let text = toml::to_string_pretty(progress).map_err(|e| AuditError::WorkDir {
        path: path.clone(),
        source: std::io::Error::other(e),
    })?;
    workdir::write_atomically(&path, &text)
}

/// Read the recorded progress, or nothing when no sweep has run here.
///
/// # Errors
///
/// [`AuditError::Read`] when the record exists but cannot be read, and
/// [`AuditError::Parse`] when it is malformed. A record that cannot be parsed
/// is NOT treated as absent: silently starting over would be defensible, but
/// silently PACKAGING over it would not, and both callers read through here.
pub fn read_progress(work: &WorkDir) -> Result<Option<RunProgress>, AuditError> {
    let path = progress_path(work);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(AuditError::Read { path, source }),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|source| AuditError::Parse {
            path,
            what: "run progress record",
            source: Box::new(source),
        })
}

/// Why one selected repository is being audited again rather than carried over.
///
/// Why: silent skipping is the defect #5494 names, and so is a silent re-run —
/// an operator watching a resumed sweep spend another four hours on a
/// repository it already audited must be able to see what made it ineligible.
/// What: the four states [`plan`] distinguishes, each rendered as one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recollect {
    /// No record of this repository, at this position in the selection.
    NotRecorded,
    /// It is recorded, and recorded as failed. Re-runs retry failures.
    Failed,
    /// It is recorded as audited, but its output no longer holds up.
    OutputGone(String),
    /// The operator asked for a full re-collection.
    Forced,
    /// The recorded output has no completion marker: an earlier run wrote it and
    /// did not get to the end of that repository (#6141).
    Unfinished,
}

impl Recollect {
    /// One line naming why, safe to show the recipient.
    pub fn reason(&self) -> String {
        match self {
            Self::NotRecorded => "not recorded by an earlier run".to_owned(),
            Self::Failed => "the earlier run recorded it as failed — retrying".to_owned(),
            Self::OutputGone(why) => format!("its recorded output is no longer usable: {why}"),
            Self::Forced => "`--fresh` was asked for".to_owned(),
            Self::Unfinished => {
                "an earlier run did not finish this repository — re-collecting".to_owned()
            }
        }
    }
}

/// What a re-run may carry over, decided per selected repository.
///
/// Why: the resume decision has to be made against the CURRENT selection, not
/// against whatever the record happens to hold. Two things go wrong otherwise,
/// and both end with a repository reported as audited when it was not. A record
/// naming a repository the operator has since dropped, or moved to a different
/// position, describes a different `out/<stem>/` than this run will use — so the
/// entry is matched on the repository AND on the output path this run computes
/// for it, which the selection index is part of. And a record naming an output
/// that has since been deleted describes a directory that is not there — so the
/// output is re-verified through [`verify_output`], the same check that decided
/// the entry was a success in the first place, rather than trusted because the
/// file says `Succeeded`.
///
/// # Postconditions
/// The returned vector has one entry per selected repository, in selection
/// order. `Ok(run)` may be reported without running anything; `Err(reason)`
/// names why this repository is being audited again.
///
/// What: reads the record, then answers per index. `fresh` short-circuits every
/// entry to [`Recollect::Forced`], which is the operator's way to force a full
/// re-collection.
/// Test: `super::run_tests::a_re_run_skips_what_succeeded_and_retries_what_failed`,
/// `super::run_tests::a_deleted_output_is_re_audited_rather_than_reported_complete`,
/// `super::run_tests::a_reordered_selection_does_not_reuse_the_wrong_output`,
/// `super::run_tests::a_fresh_run_re_audits_everything`.
///
/// # Errors
///
/// [`AuditError::Read`] or [`AuditError::Parse`] when a record exists and is
/// unusable. A checkpoint that cannot be read is a refusal, not an empty plan:
/// starting over would redo hours of work the operator was told were saved.
pub(super) fn plan(
    work: &WorkDir,
    selected: &[SelectedRepo],
    fresh: bool,
) -> Result<Vec<Result<RepoRun, Recollect>>, AuditError> {
    let recorded = if fresh {
        Vec::new()
    } else {
        read_progress(work)?.map(|p| p.repos).unwrap_or_default()
    };
    Ok(selected
        .iter()
        .enumerate()
        .map(|(index, repo)| {
            if fresh {
                return Err(Recollect::Forced);
            }
            let output = work.path(Area::Output).join(stem(index, &repo.name));
            let Some(entry) = recorded
                .iter()
                .find(|e| &e.repo == repo && e.output == output)
            else {
                return Err(Recollect::NotRecorded);
            };
            if !entry.result.succeeded() {
                return Err(Recollect::Failed);
            }
            // #5494: the record says it succeeded; the disk decides whether it
            // still has. Re-verifying re-reads the gaps too, so a carried-over
            // entry states what its manifest states today.
            match verify_output(&entry.output) {
                // #6141: the manifest alone is not completion. The grounding
                // pass and the inference stamp both run after `tga audit` wrote
                // it, so a run killed between them leaves a directory this check
                // accepts and no finished report. The marker is written last,
                // and its absence is what says so. Asked AFTER `verify_output`
                // so a deleted or unreadable output still gets that check's
                // specific reason rather than this one's. A record from before
                // the marker existed has none either, and is re-collected once —
                // the same direction `RunProgress::complete` takes, for the same
                // reason.
                Ok(_) if !is_complete(&entry.output) => Err(Recollect::Unfinished),
                Ok(gaps) => Ok(RepoRun {
                    gaps,
                    resumed: true,
                    ..entry.clone()
                }),
                Err(why) => Err(Recollect::OutputGone(why)),
            }
        })
        .collect())
}
