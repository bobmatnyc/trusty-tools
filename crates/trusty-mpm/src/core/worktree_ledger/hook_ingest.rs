//! Recording Claude Code agent-isolation trees from hook payloads (#8994 S2a).
//!
//! Why: slice 1 learned of an isolation tree only when someone ran
//! `tm worktrees`, so the ledger lagged the harness by hours. Every hook event
//! a subagent emits carries its working directory as `cwd`, and for an
//! isolated agent that is `<repo>/.claude/worktrees/agent-<id>`. Reading it
//! records the tree the moment the agent first acts. tm subscribes to no
//! `WorktreeCreate` / `WorktreeRemove` hook (that hook REPLACES the harness's
//! own creation), so the harness's behaviour is unchanged and nothing here
//! creates or deletes a tree.
//! What: [`agent_tree_of`] maps a payload `cwd` to its isolation tree;
//! [`observe_agent_tree`] appends one `observed` event for a tree under a
//! registered project that the ledger does not already hold live;
//! [`reconcile_agent_trees`] runs slice 1's removal rule over the live
//! agent-isolation trees. The ingest-failure sidecar
//! ([`record_ingest_failure`], [`read_ingest_failure`],
//! [`clear_ingest_failure`]) is how a failed write stays visible to the
//! `worktree_registry` doctor row.
//! Test: `core::worktree_ledger::hook_ingest::tests`.

use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::fold::fold;
use super::reconcile::{ReconcileReport, reconcile_removed};
use super::{EventKind, LedgerError, LedgerEvent, Origin, WorktreeLedger, ledger_key};
use crate::session_manager::worktree_ownership::is_harness_agent_worktree;
use crate::session_manager::worktree_registry::list_registered_worktrees;

/// Sidecar beside the ledger that holds the last hook-ingest write failure.
pub const INGEST_FAILURE_FILE: &str = "worktrees.ingest-failure.json";

/// Name prefix Claude Code gives an agent-isolation tree.
const AGENT_TREE_PREFIX: &str = "agent-";

/// The agent-isolation tree a hook payload's `cwd` sits in, if any.
///
/// Why: the payload is input from another process; a relative path or one
/// with `..` components could name any directory once resolved, so it is
/// refused rather than normalized.
/// What: `Some(tree)` for the nearest ancestor-or-self of an absolute,
/// `..`-free `cwd` shaped `<repo>/.claude/worktrees/agent-<id>`; else `None`.
/// Touches no filesystem.
/// Test: `agent_tree_of_accepts_the_tree_and_its_subdirectories`,
/// `agent_tree_of_refuses_odd_cwds`.
pub fn agent_tree_of(cwd: &Path) -> Option<PathBuf> {
    if !cwd.is_absolute() || cwd.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    cwd.ancestors()
        .find(|a| {
            a.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.len() > AGENT_TREE_PREFIX.len() && n.starts_with(AGENT_TREE_PREFIX)
            }) && is_harness_agent_worktree(a)
                && repo_of(a).is_some()
        })
        .map(Path::to_path_buf)
}

/// `<repo>` of `<repo>/.claude/worktrees/<name>`.
fn repo_of(tree: &Path) -> Option<&Path> {
    tree.parent()?.parent()?.parent()
}

/// What [`observe_agent_tree`] did with one tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// One `observed` event was appended.
    Recorded {
        /// Branch `git worktree list` reported for the tree, when it did.
        branch: Option<String>,
    },
    /// The ledger already holds the tree live; nothing appended.
    AlreadyLive,
    /// The path is not a directory now; nothing appended.
    NotADirectory,
    /// The tree resolves outside every registered project; nothing appended.
    Unregistered,
    /// The directory exists but `git worktree list` does not name it as a
    /// worktree (#8994 S2b: a `git worktree add` that failed); nothing appended.
    NotListed,
}

/// Append one `observed` event for `tree`, unless it is already live.
///
/// Why: the ledger must name each live isolation tree once, whether the
/// event comes from a first tool call, a repeat, or a daemon restart that lost
/// its in-memory dedup.
/// What: refuses (appending nothing) a path that is not a directory, or whose
/// canonical form is not an agent-isolation tree of one of `checkouts`. Folds
/// the ledger and returns [`ObserveOutcome::AlreadyLive`] for a live key.
/// Otherwise appends `observed` (origin `agent_isolation`, the branch git
/// lists for the tree, `session`). A ledger read or write error is returned.
/// Test: `observe_records_a_registered_tree_once`,
/// `observe_refuses_an_unregistered_or_absent_tree`,
/// `observe_after_removed_records_the_tree_again`.
pub fn observe_agent_tree(
    ledger: &WorktreeLedger,
    tree: &Path,
    checkouts: &[PathBuf],
    session: Option<&str>,
) -> Result<ObserveOutcome, LedgerError> {
    if !tree.is_dir() {
        return Ok(ObserveOutcome::NotADirectory);
    }
    let key = ledger_key(tree);
    // #8994: decide on the canonical path, so a symlinked tree that resolves
    // elsewhere is never keyed under a registered project.
    let registered = agent_tree_of(&key).as_deref() == Some(key.as_path())
        && repo_of(&key).is_some_and(|repo| checkouts.iter().any(|c| ledger_key(c) == repo));
    let Some(repo) = repo_of(&key).filter(|_| registered) else {
        return Ok(ObserveOutcome::Unregistered);
    };
    if fold(&ledger.read()?.events).live.contains_key(&key) {
        return Ok(ObserveOutcome::AlreadyLive);
    }
    let branch = list_registered_worktrees(repo)
        .unwrap_or_default()
        .into_iter()
        .find(|w| ledger_key(&w.path) == key)
        .and_then(|w| w.branch);
    ledger.append(&LedgerEvent::now(
        key.clone(),
        EventKind::Observed {
            repo: repo.to_path_buf(),
            branch: branch.clone(),
            origin: Origin::AgentIsolation,
            session: session.map(str::to_string),
        },
    ))?;
    Ok(ObserveOutcome::Recorded { branch })
}

/// Slice 1's `removed` rule over the live agent-isolation trees only.
///
/// Why: called at `SubagentStop` / `SessionEnd`, after an agent may have
/// finished. The rule — directory absent AND `git worktree list` answered
/// without it — is what keeps a tree that still exists, or one whose
/// repository cannot be listed, live.
/// What: folds the ledger, keeps the live entries whose origin is
/// `agent_isolation`, and hands them to [`reconcile_removed`]. Appends nothing
/// and runs no git when there are none.
/// Test: `reconcile_marks_removed_only_when_gone_from_disk_and_git`,
/// `reconcile_leaves_a_present_tree_live`.
pub fn reconcile_agent_trees(ledger: &WorktreeLedger) -> Result<ReconcileReport, LedgerError> {
    let mut state = fold(&ledger.read()?.events);
    state
        .live
        .retain(|_, wt| wt.origin == Origin::AgentIsolation);
    if state.live.is_empty() {
        return Ok(ReconcileReport::default());
    }
    reconcile_removed(ledger, &state)
}

/// The last hook-ingest write failure, as the sidecar records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestFailure {
    /// When the latest failure happened.
    pub ts: DateTime<Utc>,
    /// What failed, with the underlying error.
    pub error: String,
    /// Failures since the sidecar was last cleared.
    pub failures: u64,
}

/// The sidecar path for `ledger`: [`INGEST_FAILURE_FILE`] in its directory.
pub fn ingest_failure_path(ledger: &WorktreeLedger) -> PathBuf {
    ledger.path().with_file_name(INGEST_FAILURE_FILE)
}

/// Record one hook-ingest failure in the sidecar.
///
/// Why: the hook must not fail when the ledger cannot be written, and the
/// failure must not stay in the daemon log alone — `tm doctor` may run in a
/// process that never saw it.
/// What: rewrites the sidecar with `error`, the current time, and the previous
/// count plus one (an unreadable previous sidecar counts as zero).
/// Test: `ingest_failure_sidecar_counts_and_clears`.
pub fn record_ingest_failure(ledger: &WorktreeLedger, error: &str) -> std::io::Result<()> {
    let path = ingest_failure_path(ledger);
    let failures = read_ingest_failure(ledger)
        .ok()
        .flatten()
        .map_or(0, |f| f.failures)
        + 1;
    let record = IngestFailure {
        ts: Utc::now(),
        error: error.to_string(),
        failures,
    };
    let body = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}

/// Read the sidecar: `Ok(None)` when absent, `Err` when unreadable or corrupt.
pub fn read_ingest_failure(ledger: &WorktreeLedger) -> Result<Option<IngestFailure>, String> {
    let path = ingest_failure_path(ledger);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Remove the sidecar after a successful append; an absent one is fine.
pub fn clear_ingest_failure(ledger: &WorktreeLedger) -> std::io::Result<()> {
    match std::fs::remove_file(ingest_failure_path(ledger)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[path = "hook_ingest_tests.rs"]
mod tests;
