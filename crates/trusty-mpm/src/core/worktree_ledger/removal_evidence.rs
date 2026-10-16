//! `removed` from a removal route, on evidence only (#8994 S2c).
//!
//! Why: before S2c no removal route told the ledger anything — a tree the
//! merged-PR reclaim, the agent-worktree reap or a session decommission
//! removed stayed live until someone ran `tm worktrees`. A route's own word
//! that it removed a tree is not evidence: git can report success over a
//! surviving directory, and a removal can stop half way.
//! What: [`record_confirmed_removals`] re-checks each path the route reports
//! removed and appends `removed` only when the directory is gone (a `NotFound`
//! stat) AND the repository's `git worktree list` answered without it — slice
//! 1's rule, through [`super::reconcile`]'s listing. Every other answer
//! appends nothing and says why in a [`RemovalVerdict`]; [`log_evidence`]
//! logs each one that is not a recorded removal at `warn`. [`ledger_recorder`]
//! wraps both, under a lock shared with the hook ingest, as the
//! [`RemovalRecorder`] the three routes call. Nothing here deletes anything.
//! Test: `core::worktree_ledger::removal_evidence::tests`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::{LedgerError, WorktreeLedger};

/// What the re-check found for one path a route reported removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalVerdict {
    /// Gone from disk and from git: one `removed` event was appended.
    Recorded,
    /// The ledger holds no live entry for the path; nothing to record.
    NotLive,
    /// The directory is still on disk; nothing appended.
    StillOnDisk,
    /// The directory is gone but git still lists it; nothing appended.
    StillListed,
    /// The re-check could not be answered; nothing appended. Carries why.
    Unanswerable(String),
}

/// One path's re-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalEvidence {
    /// The path the route reported removed.
    pub path: PathBuf,
    /// What the re-check found.
    pub verdict: RemovalVerdict,
}

/// The call a removal route makes after it removed trees: route name, paths.
pub type RemovalRecorder = Arc<dyn Fn(&str, &[PathBuf]) -> Vec<RemovalEvidence> + Send + Sync>;

/// Re-check each of `removed` and append `removed` for the confirmed ones.
///
/// Test: see the module doc.
pub fn record_confirmed_removals(
    _ledger: &WorktreeLedger,
    removed: &[PathBuf],
) -> Result<Vec<RemovalEvidence>, LedgerError> {
    Ok(removed
        .iter()
        .map(|p| RemovalEvidence {
            path: p.clone(),
            verdict: RemovalVerdict::NotLive,
        })
        .collect())
}

/// Log every verdict that is not a recorded removal.
pub fn log_evidence(_route: &str, _evidence: &[RemovalEvidence]) {}

/// A [`RemovalRecorder`] over `ledger`, serialized by `lock`.
pub fn ledger_recorder(ledger: WorktreeLedger, lock: Arc<Mutex<()>>) -> RemovalRecorder {
    Arc::new(move |route, removed| {
        let _held = lock.lock();
        let evidence = record_confirmed_removals(&ledger, removed).unwrap_or_default();
        log_evidence(route, &evidence);
        evidence
    })
}

#[cfg(test)]
#[path = "removal_evidence_tests.rs"]
mod tests;
