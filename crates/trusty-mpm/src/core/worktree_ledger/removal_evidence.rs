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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use super::fold::{LedgerState, fold};
use super::hook_ingest::record_ingest_failure;
use super::reconcile::{lenient_key, listed_keys};
use super::{EventKind, LedgerAppender, LedgerError, LedgerEvent, WorktreeLedger, ledger_key};

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

/// A repository's listing, memoized for one call.
type Listings = BTreeMap<PathBuf, Result<BTreeSet<PathBuf>, String>>;

/// Re-check each of `removed` and append `removed` for the confirmed ones.
///
/// Why: see the module doc — the route's word is a claim; the disk and git
/// are the evidence.
/// What: folds the ledger once. Per path, in order: no live entry (raw,
/// canonical or lenient key) is [`RemovalVerdict::NotLive`]; a stat that
/// finds the path is [`RemovalVerdict::StillOnDisk`]; a stat error other than
/// `NotFound` is [`RemovalVerdict::Unanswerable`]. Then the entry's repository
/// is listed (once per call): a listing that cannot be trusted is
/// `Unanswerable`, one that names the tree is [`RemovalVerdict::StillListed`],
/// and only one that answered without it appends `removed` for the entry's
/// key ([`RemovalVerdict::Recorded`]). A path recorded earlier in the same
/// call is `NotLive` the second time. A ledger read or write error is `Err`.
/// Test: `a_removal_gone_from_disk_and_git_appends_removed_once`,
/// `a_removal_that_left_the_tree_behind_appends_nothing`,
/// `an_unanswerable_recheck_appends_nothing_and_warns_with_its_cause`.
pub fn record_confirmed_removals(
    ledger: &WorktreeLedger,
    removed: &[PathBuf],
) -> Result<Vec<RemovalEvidence>, LedgerError> {
    let mut state = fold(&ledger.read()?.events);
    let mut listings = Listings::new();
    let mut appender: Option<LedgerAppender> = None;
    let mut out = Vec::with_capacity(removed.len());
    for path in removed {
        let verdict = match live_key(&state, path) {
            None => RemovalVerdict::NotLive,
            Some(key) => {
                let verdict = recheck(&state, &key, path, &mut listings);
                if verdict == RemovalVerdict::Recorded {
                    if appender.is_none() {
                        appender = Some(ledger.open_appender()?);
                    }
                    if let Some(writer) = appender.as_mut() {
                        writer.append(&LedgerEvent::now(key.clone(), EventKind::Removed))?;
                    }
                    state.live.remove(&key);
                }
                verdict
            }
        };
        out.push(RemovalEvidence {
            path: path.clone(),
            verdict,
        });
    }
    Ok(out)
}

/// The fold key of `path`'s live entry: raw, canonical or lenient spelling.
fn live_key(state: &LedgerState, path: &Path) -> Option<PathBuf> {
    [path.to_path_buf(), ledger_key(path), lenient_key(path)]
        .into_iter()
        .find(|k| state.live.contains_key(k))
}

/// The verdict for the live entry `key`, which a route reported as `path`.
///
/// `Recorded` here means the evidence allows the record; the caller appends.
fn recheck(
    state: &LedgerState,
    key: &Path,
    path: &Path,
    listings: &mut Listings,
) -> RemovalVerdict {
    match std::fs::symlink_metadata(path) {
        Ok(_) => return RemovalVerdict::StillOnDisk,
        // #8994 S2c: only `NotFound` is "gone"; any other stat error is unknown.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return RemovalVerdict::Unanswerable(format!("stat of {} failed: {e}", path.display()));
        }
        Err(_) => {}
    }
    let Some(repo) = state.live.get(key).map(|wt| wt.repo.clone()) else {
        return RemovalVerdict::NotLive;
    };
    let listed = listings
        .entry(repo.clone())
        .or_insert_with(|| listed_keys(&repo));
    match listed {
        Err(reason) => RemovalVerdict::Unanswerable(format!(
            "the worktree listing of {} cannot be trusted: {reason}",
            repo.display()
        )),
        Ok(keys) => {
            let spellings = [
                key.to_path_buf(),
                lenient_key(key),
                path.to_path_buf(),
                lenient_key(path),
            ];
            if spellings.iter().any(|k| keys.contains(k)) {
                RemovalVerdict::StillListed
            } else {
                RemovalVerdict::Recorded
            }
        }
    }
}

/// Log every verdict: a recorded removal at `info`, a tree that stays live
/// because the removal left it or could not be confirmed at `warn`, with why.
///
/// Test: `an_unanswerable_recheck_appends_nothing_and_warns_with_its_cause`.
pub fn log_evidence(route: &str, evidence: &[RemovalEvidence]) {
    for e in evidence {
        let path = e.path.display();
        match &e.verdict {
            RemovalVerdict::Recorded => tracing::info!(
                route,
                "worktree ledger: recorded {path} removed after {route} (#8994)"
            ),
            RemovalVerdict::NotLive => tracing::debug!(
                route,
                "worktree ledger: {path} is not live in the ledger; nothing to record (#8994)"
            ),
            RemovalVerdict::StillOnDisk => tracing::warn!(
                route,
                "worktree ledger: {route} reported {path} removed but it is still on disk; \
                 nothing recorded, the tree stays live (#8994)"
            ),
            RemovalVerdict::StillListed => tracing::warn!(
                route,
                "worktree ledger: {path} is gone from disk but `git worktree list` still names \
                 it after {route}; nothing recorded, the tree stays live (#8994)"
            ),
            RemovalVerdict::Unanswerable(why) => tracing::warn!(
                route,
                "worktree ledger: could not confirm {route}'s removal of {path}: {why}; nothing \
                 recorded, the tree stays live (#8994)"
            ),
        }
    }
}

/// A [`RemovalRecorder`] over `ledger`, serialized by `lock`.
///
/// Why: the daemon passes the hook ingest's write lock, so a reap's record
/// and the `SessionEnd` reconcile cannot both fold "live" and both append.
/// What: under `lock`, [`record_confirmed_removals`] then [`log_evidence`].
/// A ledger error appends nothing, is logged at `warn` and is written to the
/// ingest-failure sidecar the `worktree_registry` doctor row reads; the
/// recorder then returns no evidence. It never panics.
/// Test: `an_unanswerable_recheck_appends_nothing_and_warns_with_its_cause`,
/// `an_unreadable_ledger_is_warned_and_reaches_the_sidecar`.
pub fn ledger_recorder(ledger: WorktreeLedger, lock: Arc<Mutex<()>>) -> RemovalRecorder {
    Arc::new(move |route, removed| {
        let _held = lock.lock().unwrap_or_else(PoisonError::into_inner);
        match record_confirmed_removals(&ledger, removed) {
            Ok(evidence) => {
                log_evidence(route, &evidence);
                evidence
            }
            Err(e) => {
                let message = format!("could not record {route}'s removals: {e}");
                tracing::warn!(
                    route,
                    "worktree ledger: {message}; the trees stay live until `tm worktrees` \
                     reconciles them (#8994)"
                );
                if let Err(side) = record_ingest_failure(&ledger, &message) {
                    tracing::warn!(
                        "worktree ledger: could not record that failure: {side} (#8994)"
                    );
                }
                Vec::new()
            }
        }
    })
}

#[cfg(test)]
#[path = "removal_evidence_tests.rs"]
mod tests;
