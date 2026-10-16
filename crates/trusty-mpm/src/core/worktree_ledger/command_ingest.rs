//! Recording what a Bash `git worktree` call did (#8994 S2b).
//!
//! Why: the parser ([`super::bash_worktree`]) knows what a command asked git
//! to do, not what git did — the relay forwards no exit code, and a call can
//! fail behind `|| true`. So every record is decided on evidence: the
//! directory on disk and `git worktree list`. Observe-only: nothing here
//! creates, deletes, moves, locks or unlocks a tree.
//! What: [`observe_added_tree`] appends one `observed` (origin `git_command`)
//! for a new tree that exists, that git lists, that was created within the
//! Bash tool's longest run, and whose repository is a registered project; [`reconcile_command_removal`] applies slice 1's
//! `removed` rule after `git worktree remove` / `prune`.
//! Test: `daemon::services::worktree_command_ingest::tests`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::fold::fold;
use super::hook_ingest::{ObserveOutcome, observe_key};
use super::reconcile::{ReconcileReport, lenient_key, reconcile_removed};
use super::{LedgerError, Origin, WorktreeLedger, ledger_key};
use crate::session_manager::worktree_registry::probe_registered_worktrees;

/// How `probe_registered_worktrees` words a git that ran and exited non-zero.
const GIT_EXITED: &str = "`git worktree list` exited";

/// How long before the hook's receipt a Bash call may have created its tree.
///
/// Why: the relay forwards no exit code, so a refused `git worktree add` of a
/// path that is already a registered worktree looks like a successful add.
/// Claude Code's Bash tool times out at 10 minutes at most; a tree whose
/// gitfile is older than that (plus 5 s of timestamp slack) cannot be this
/// call's.
pub const NEW_TREE_WINDOW: Duration = Duration::from_secs(10 * 60 + 5);

/// Why a Bash-command record could not be decided.
#[derive(Debug, thiserror::Error)]
pub enum CommandIngestError {
    /// The ledger could not be read or written.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    /// `git worktree list` could not be run or timed out, so the tree is unknown.
    #[error("`git worktree list` in {path} gave no answer: {source}")]
    Git {
        /// The directory git was run in.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// Record the tree a `git worktree add` made, if it made one.
///
/// Why: see the module doc; a failed add must record nothing.
/// What: tries `candidates` in order. A candidate that is not a directory is
/// [`ObserveOutcome::NotADirectory`]; one that `git worktree list` (run in it)
/// does not name, or where git exits non-zero (no repository), is
/// [`ObserveOutcome::NotListed`]; one whose main checkout is not among
/// `checkouts` is [`ObserveOutcome::Unregistered`]. A listed tree whose
/// `.git` file is older than `not_before` (or has no readable time) was not
/// made by this call — git refused a duplicate add — so it is
/// [`ObserveOutcome::NotNew`] and left for the backfill. The first verified
/// candidate goes to [`observe_key`] (origin `git_command`), which dedups a
/// live entry. Otherwise the last candidate's outcome is returned. A git
/// spawn failure or timeout, or a ledger error, is an `Err`.
/// Test: `each_worktree_add_form_records_one_entry`,
/// `a_failed_worktree_add_records_nothing`,
/// `an_add_refused_for_an_existing_worktree_records_nothing`,
/// `a_failed_record_is_never_silent_and_never_blocks`.
pub fn observe_added_tree(
    ledger: &WorktreeLedger,
    candidates: &[PathBuf],
    checkouts: &[PathBuf],
    session: Option<&str>,
    not_before: SystemTime,
) -> Result<ObserveOutcome, CommandIngestError> {
    let mut outcome = ObserveOutcome::NotADirectory;
    for candidate in candidates {
        if !candidate.is_dir() {
            outcome = ObserveOutcome::NotADirectory;
            continue;
        }
        let key = ledger_key(candidate);
        let listed = match probe_registered_worktrees(&key) {
            Ok(listed) => listed,
            // Git ran and exited non-zero: the directory is in no repository.
            Err(e) if e.to_string().starts_with(GIT_EXITED) => Vec::new(),
            Err(source) => return Err(CommandIngestError::Git { path: key, source }),
        };
        let Some(tree) = listed
            .iter()
            .find(|w| !w.is_main && !w.bare && ledger_key(&w.path) == key)
        else {
            outcome = ObserveOutcome::NotListed;
            continue;
        };
        if !made_since(&key, not_before) {
            outcome = ObserveOutcome::NotNew;
            continue;
        }
        let repo = listed
            .iter()
            .find(|w| w.is_main)
            .map(|w| ledger_key(&w.path));
        let Some(repo) = repo.filter(|r| checkouts.iter().any(|c| ledger_key(c) == *r)) else {
            outcome = ObserveOutcome::Unregistered;
            continue;
        };
        let branch = tree.branch.clone();
        return Ok(observe_key(
            ledger,
            &key,
            &repo,
            Origin::GitCommand,
            session,
            || branch,
        )?);
    }
    Ok(outcome)
}

/// `true` when `tree`'s `.git` file was written at or after `not_before`.
///
/// Why: `git worktree add` writes that file when it creates the tree.
/// Unreadable time or a missing file counts as not new: the backfill is the
/// safe route, a wrong owner is not.
fn made_since(tree: &Path, not_before: SystemTime) -> bool {
    std::fs::metadata(tree.join(".git"))
        .and_then(|m| m.modified())
        .is_ok_and(|modified| modified >= not_before)
}

/// Slice 1's `removed` rule after a `git worktree remove` or `prune`.
///
/// Why: the same evidence S2a uses — directory gone AND `git worktree list`
/// no longer naming it — so a remove that failed, or a tree deleted from disk
/// that git still lists, stays live.
/// What: `targets` `Some` (remove) keeps the live entries whose path matches
/// a target (compared with the deepest existing ancestor canonicalized, as
/// the tree itself is gone); `None` (prune) keeps every live entry. Hands
/// them to [`reconcile_removed`]; appends nothing and runs no git when none
/// match.
/// Test: `remove_and_prune_record_removed_only_when_gone_from_disk_and_git`.
pub fn reconcile_command_removal(
    ledger: &WorktreeLedger,
    targets: Option<&[PathBuf]>,
) -> Result<ReconcileReport, LedgerError> {
    let mut state = fold(&ledger.read()?.events);
    if let Some(targets) = targets {
        let keys: Vec<PathBuf> = targets.iter().map(|t| lenient_key(t)).collect();
        state
            .live
            .retain(|path, _| keys.contains(path) || keys.contains(&lenient_key(path)));
    }
    if state.live.is_empty() {
        return Ok(ReconcileReport::default());
    }
    reconcile_removed(ledger, &state)
}
