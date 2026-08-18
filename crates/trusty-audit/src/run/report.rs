//! What a sweep produced, and how it was asked to run.
//!
//! Why: #5982 added [`RunReport::board_gaps`] and pushed `run.rs` past the
//! 500-SLOC production cap — the fourth split for that reason, after
//! `selection`, `pins` and `approve`. It separates on the same line those did:
//! `run.rs` drives `tga audit` children, and these types are the values that
//! cross the boundary out of it. Nothing here spawns, reads or writes anything.
//!
//! What: [`RepoResult`] and [`RepoRun`] per repository, [`RunStatus`] and
//! [`RunReport`] over the sweep, and [`RunOptions`] going the other way.
//!
//! Test: `super::run_tests`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::SelectedRepo;

/// What happened to one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RepoResult {
    /// `tga audit` exited 0 for this repository.
    Succeeded,
    /// It did not. The reason is the child's status, or why it never started.
    Failed {
        /// One line naming what went wrong, safe to show the recipient.
        reason: String,
    },
}

impl RepoResult {
    /// Whether this repository was audited successfully.
    pub fn succeeded(&self) -> bool {
        matches!(self, RepoResult::Succeeded)
    }
}

/// One repository's run: where its output and log landed, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RepoRun {
    /// The repository, as selected.
    pub repo: SelectedRepo,
    /// Directory under `out/` holding this repository's audit output.
    pub output: PathBuf,
    /// File under `logs/` holding the child's combined stdout and stderr.
    pub log: PathBuf,
    /// Gaps `tga audit` stated in this repository's manifest (DOC-67 §9).
    ///
    /// A gap is a dimension the sweep could not assess — an unconfigured JIRA
    /// project, a repository that could not be fetched from its remote. It is
    /// ordinary for a successful run to state some, so a gap does not by itself
    /// fail the repository; see `super::verify_output` for which ones do.
    #[serde(default)]
    pub gaps: Vec<String>,
    /// Whether this entry was carried over from an earlier sweep (#5494).
    ///
    /// `true` means no `tga audit` child ran for this repository in the sweep
    /// that produced this report — an earlier run audited it, and the output it
    /// left is still on disk and still passes `super::verify_output`. It is
    /// recorded so a re-run reads as a resume rather than as work that went
    /// suspiciously fast.
    #[serde(default)]
    pub resumed: bool,
    /// How it ended.
    pub result: RepoResult,
}

/// Whether the sweep succeeded, partly succeeded, or failed outright.
///
/// Why: closure condition 3 of #5555, and DOC-67 §9's failed-but-continuing
/// model. "One repository of six failed" and "every repository failed" call for
/// different actions from the recipient, and collapsing them into a boolean is
/// how a partial run gets delivered as a whole one.
/// What: three states over the per-repo results. An empty sweep never reaches
/// here — no selection is an error, not an [`AllSucceeded`](Self::AllSucceeded).
/// Test: `super::run_tests::status_distinguishes_partial_from_total_failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RunStatus {
    /// Every selected repository was audited.
    AllSucceeded,
    /// Some repositories were audited and some were not.
    Partial,
    /// No repository was audited.
    AllFailed,
}

/// The result of one sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunReport {
    /// One entry per selected repository, in selection order.
    pub repos: Vec<RepoRun>,
    /// The sweep's overall verdict.
    pub status: RunStatus,
    /// One line per registered board this sweep resolved and will not collect.
    ///
    /// Why: #5982. `super::boards::Boards::gaps` had exactly one reader,
    /// `crate::chain`, so a board gap stated on `taudit audit` was discarded on
    /// `taudit run` — a legacy `linear:<team-id>` produced exit 0,
    /// [`RunStatus::AllSucceeded`], no `linear:` section, and no message. That
    /// is the silent-empty shape `super::boards::resolve`'s gap lines exist to
    /// replace, and replacing it on one of the two commands that run a sweep is
    /// not replacing it.
    ///
    /// What: empty unless the sweep resolved the boards itself, which is the
    /// `taudit run` path. `super::sweep_with_boards` takes a resolution its
    /// caller already stated, and repeating it here would print each gap twice
    /// on one screen — the same duplication `crate::cli::render::render_chain`
    /// filters for the acquisition phase.
    ///
    /// Test: `super::run_tests::a_board_the_sweep_cannot_collect_is_stated_on_the_run_path`,
    /// `super::run_tests::a_sweep_handed_its_boards_leaves_the_gaps_to_the_caller`,
    /// `crate::cli::cli_tests::a_sweep_that_skipped_a_board_does_not_exit_zero`.
    #[serde(default)]
    pub board_gaps: Vec<String>,
}

impl RunReport {
    /// Build a report and derive its status from the per-repo results.
    ///
    /// Why: the status is DERIVED, never passed in, so no caller can construct
    /// a report claiming a status its per-repo results do not support.
    /// Test: `super::run_tests::status_distinguishes_partial_from_total_failure`.
    pub fn of(repos: Vec<RepoRun>) -> Self {
        let succeeded = repos.iter().filter(|r| r.result.succeeded()).count();
        let status = if succeeded == repos.len() {
            RunStatus::AllSucceeded
        } else if succeeded == 0 {
            RunStatus::AllFailed
        } else {
            RunStatus::Partial
        };
        Self {
            repos,
            status,
            board_gaps: Vec::new(),
        }
    }

    /// The same report, also stating the boards the sweep will not collect.
    ///
    /// Separate from [`Self::of`] because the status stays derived from the
    /// per-repo results alone: a board the sweep never attempted is not a
    /// repository that failed, and folding it into [`RunStatus`] would report a
    /// clean sweep as [`RunStatus::Partial`]. `crate::session::Outcome` is where
    /// the two meet, and it answers `EXIT_INCOMPLETE` rather than
    /// `EXIT_PARTIAL`. See [`Self::board_gaps`] (#5982).
    #[must_use]
    pub fn stating(mut self, board_gaps: Vec<String>) -> Self {
        self.board_gaps = board_gaps;
        self
    }

    /// The repositories that failed.
    pub fn failures(&self) -> impl Iterator<Item = &RepoRun> {
        self.repos.iter().filter(|r| !r.result.succeeded())
    }

    /// The repositories an earlier sweep audited and this one carried over.
    pub fn resumed(&self) -> impl Iterator<Item = &RepoRun> {
        self.repos.iter().filter(|r| r.resumed)
    }
}

/// How to run the sweep.
///
/// Why: a struct rather than a bare `bool` argument, matching
/// [`crate::clone::CloneOptions`] — the flag is operator-facing and reaches
/// this crate through [`crate::session::Command::Run`], so it will grow
/// neighbours (an audit window, a repository subset) and a positional boolean
/// at every call site is where those go wrong.
/// What: one field today. [`RunOptions::default`] is the resuming behaviour.
/// Test: `super::run_tests::a_fresh_run_re_audits_everything`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunOptions {
    /// Ignore the recorded progress and audit every selected repository again.
    ///
    /// Why: resume is a judgement about what is still valid, and an operator
    /// who disagrees with that judgement needs a way to overrule it — a
    /// recipient who has re-cloned their repositories has outputs that verify
    /// fine and describe source that has moved on. Defaults to `false`, because
    /// the expensive direction has to be the one asked for by name.
    pub fresh: bool,
}
