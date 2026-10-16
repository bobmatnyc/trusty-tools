//! Recording what a Bash `git worktree` call did (#8994 S2b).
//!
//! RED STUB.

use std::path::PathBuf;

use super::hook_ingest::ObserveOutcome;
use super::reconcile::ReconcileReport;
use super::{LedgerError, WorktreeLedger};

/// Why a Bash-command record could not be decided.
#[derive(Debug, thiserror::Error)]
pub enum CommandIngestError {
    /// The ledger could not be read or written.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

/// RED STUB.
pub fn observe_added_tree(
    _ledger: &WorktreeLedger,
    _candidates: &[PathBuf],
    _checkouts: &[PathBuf],
    _session: Option<&str>,
) -> Result<ObserveOutcome, CommandIngestError> {
    Ok(ObserveOutcome::NotADirectory)
}

/// RED STUB.
pub fn reconcile_command_removal(
    _ledger: &WorktreeLedger,
    _targets: Option<&[PathBuf]>,
) -> Result<ReconcileReport, LedgerError> {
    Ok(ReconcileReport::default())
}
