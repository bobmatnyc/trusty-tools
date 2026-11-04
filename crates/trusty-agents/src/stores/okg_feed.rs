//! The agent's OKG index feed, run by `trusty_okf::feed` (#3892, #10746 S12).
//!
//! Why: an ingest that writes the tree and stops there leaves the assistant
//! finding nothing (#3892). SPEC-OKF-01 §3 (e) A moves the push loop into
//! trusty-okf; §4.9.1 moves its journal out of the synced tree. This module
//! is the agent's thin adapter: it never pushes, removes or journals on its
//! own, so every fail-closed rule of `trusty_okf::feed` (§5.4) holds here.
//! What: [`feed_source`] migrates the legacy in-tree journals once
//! (`kb::okg::legacy_journal`), opens the tree as a trusty-okf bundle, runs
//! one feed pass into the bound index with this machine's state, and folds
//! the result into the [`IndexFeedReport`] the OKG tools return. A
//! migration error, a refused preflight and a pass-level failure each
//! report `index: null` with a `reason`; none records anything. One feed
//! runs per state journal at a time ([`FeedLock`]); a second caller reports
//! `feed already running` and starts no pass.
//! Test: `crate::stores::okg_feed::tests` (two machines over one tree, the
//! fixture-journal migration, and each error arm), and the
//! `crate::tools::okg` and knowledge-pipeline suites that drive it.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.7.2 (index_feed row), §4.9.1 and §5.4, held in the
//!   trusty-okf project directory.

use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};
use trusty_okf::OkfError;
use trusty_okf::bundle::Bundle;
use trusty_okf::feed::{FeedReport, FeedStatus, IndexFeed};
use trusty_okf::state::IndexJournal;

use crate::kb::okg::index_state::IndexStateRef;
use crate::kb::okg::legacy_journal::migrate_legacy_journals;
use crate::kb::okg::registry::SourceSpec;
use crate::kb::store::KbStore;

/// What one feed call did — the caller-facing summary.
///
/// Why: the honesty half of #3892. `pending` counts this source's entities
/// that are in the tree and still owed work on this machine.
/// What: `indexed` and `removed` count what the pass confirmed across the
/// tree (trusty-okf feeds every concept, §5.4); `pending`, `unconfirmed`
/// and `synced` are this source's coverage after the pass; `status` is the
/// pass verdict.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IndexFeedReport {
    /// The index that was fed, when the pass ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// Puts the daemon confirmed this pass.
    pub indexed: usize,
    /// Removes the daemon confirmed this pass.
    pub removed: usize,
    /// This source's entities still owed to the index after the pass.
    pub pending: usize,
    /// This source's entities confirmed searchable under a known index
    /// identity (SPEC-OKF-01 §4.9.2; 0 before search reports identities).
    pub synced: usize,
    /// This source's entities pushed and probed on this machine whose index
    /// identity is not confirmed.
    pub unconfirmed: usize,
    /// How the pass ended, when it ran (§5.4; only `searchable` is full
    /// success, and none reads searchable before index identities exist).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<FeedStatus>,
    /// Why nothing was fed, when `index` is `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Per-path failures and coverage notes, across the tree.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// This source's own paths that failed this pass (probe failures
    /// aside), are unreadable, or still owe a push or a remove. Internal:
    /// the publication gate reads it.
    #[serde(skip)]
    source_blocked: Vec<String>,
}

impl IndexFeedReport {
    /// A pass that could not run, with the backlog it leaves behind.
    pub fn not_fed(reason: String, pending: usize, synced: usize) -> Self {
        Self {
            index: None,
            pending,
            synced,
            reason: Some(reason),
            ..Self::default()
        }
    }

    /// Whether this source's own entities were published to `index`.
    ///
    /// Why: the knowledge pipeline marks one record's publication complete.
    /// The pass covers the whole tree, and §5.4 F2 says one failing concept
    /// does not stop the others, so an unreadable or refused path of
    /// ANOTHER source must not block this one (#10746 S12 fix round).
    /// What: the pass ran into `index` with no pass-level reason; no path
    /// of this source's ledger failed this pass with anything but a probe
    /// failure; and the offline read finds none of them unreadable or owing
    /// a push or a remove. The pass's own errors are needed: after an
    /// identity change (F14) a refused re-push or old-revision remove
    /// leaves the earlier acknowledgement at matching bytes, which the
    /// offline read treats as settled. A failed or owed probe does not
    /// block (acknowledgement, as before S12). Errors on other paths stay
    /// in `errors`.
    /// Test: `unrelated_unreadable_concept_does_not_block_publication`,
    /// `refused_put_of_own_entity_blocks_publication`,
    /// `identity_change_with_refused_own_put_blocks_publication`,
    /// `identity_change_with_refused_own_remove_blocks_publication`.
    pub fn acknowledged(&self, index: &str) -> bool {
        self.reason.is_none()
            && self.index.as_deref() == Some(index)
            && self.status.is_some()
            && self.source_blocked.is_empty()
    }
}

/// One feed pass at a time per state journal (#10746 S12 fix round).
///
/// Why: every ingest tool and the knowledge pipeline call [`feed_source`];
/// two at once would each run a full-tree pass and interleave journal lines.
/// What: an exclusive, non-blocking OS file lock on
/// `<state dir>/agent-feed-locks/<sha256 of the journal path>.lock`. It is
/// released when the guard drops — on return, on a panic's unwind, and by
/// the OS if the process dies.
/// Test: `concurrent_feed_calls_run_one_pass`,
/// `feed_lock_is_released_on_panic`.
#[derive(Debug)]
pub struct FeedLock {
    _file: std::fs::File,
}

impl FeedLock {
    /// The lock file for `state`'s journal over `root`.
    fn path(root: &std::path::Path, state: &IndexStateRef) -> anyhow::Result<PathBuf> {
        let journal = IndexJournal::open(&state.dir, root, &state.index_id)?;
        let key: String = Sha256::digest(journal.path().as_os_str().as_encoded_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(state
            .path()
            .join("agent-feed-locks")
            .join(format!("{key}.lock")))
    }

    /// Take the lock, or `Ok(None)` when another feed holds it.
    pub fn try_acquire(
        root: &std::path::Path,
        state: &IndexStateRef,
    ) -> anyhow::Result<Option<FeedLock>> {
        let path = Self::path(root, state)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(FeedLock { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

/// Coverage for a source whose feed could not run.
///
/// Why: "not fed" without the backlog size tells the caller nothing. A read
/// that fails reports zeroes; the `reason` already says nothing was fed.
pub fn unfed(
    store: &KbStore,
    spec: &SourceSpec,
    state: Option<&IndexStateRef>,
    reason: String,
) -> IndexFeedReport {
    let cov = store
        .index_view(state)
        .and_then(|v| store.okg_index_coverage_of(spec, v.as_ref()))
        .unwrap_or_default();
    let mut r = IndexFeedReport::not_fed(reason, cov.pending, cov.synced);
    r.unconfirmed = cov.unconfirmed;
    r
}

/// Feed the tree into `state`'s index and report this source's coverage.
///
/// Why/What: see the module doc. Never fails the ingest: every error is a
/// `reason` on a not-fed report. The [`FeedLock`] is held from before the
/// migration until the report is built.
/// Test: `two_machines_over_one_tree_keep_separate_index_state`,
/// `daemon_down_is_not_fed_and_records_nothing`,
/// `root_mismatch_is_refused_before_any_push`,
/// `journal_write_failure_is_not_fed`,
/// `concurrent_feed_calls_run_one_pass`.
pub async fn feed_source(
    store: &KbStore,
    spec: &SourceSpec,
    state: &IndexStateRef,
    sink: &dyn IndexFeed,
) -> IndexFeedReport {
    let index = state.index_id.as_str();
    let _lock = match FeedLock::try_acquire(&store.root, state) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            let reason = format!("feed already running for index `{index}`; this call fed nothing");
            return unfed(store, spec, Some(state), reason);
        }
        Err(e) => {
            let reason = format!("feed lock could not be taken; nothing was fed: {e:#}");
            return unfed(store, spec, Some(state), reason);
        }
    };
    match migrate_legacy_journals(&store.root, state) {
        Ok(m) if m.migrated > 0 || !m.skipped.is_empty() => {
            tracing::info!(index, report = ?m, "legacy index journals migrated as pending");
        }
        Ok(_) => {}
        Err(e) => {
            let reason = format!("legacy index journal migration failed; nothing was fed: {e:#}");
            return unfed(store, spec, Some(state), reason);
        }
    }
    let pass = match Bundle::open(&store.root) {
        Ok((bundle, _)) => trusty_okf::feed::feed(&bundle, &state.dir, index, sink).await,
        Err(e) => Err(e),
    };
    let report = match pass {
        Ok(r) => r,
        Err(OkfError::RootMismatch { index_root, .. }) => {
            let root =
                index_root.map_or("an unknown root".to_string(), |r| r.display().to_string());
            let reason = format!(
                "index `{index}` is rooted at {root}, which does not contain this tree ({}). \
                 trusty-search drops any search result whose path falls outside the index \
                 root, so entities pushed there would be stored and never returned. Bind a \
                 search index registered over the tree itself.",
                store.root.display()
            );
            return unfed(store, spec, Some(state), reason);
        }
        Err(e) => {
            return unfed(
                store,
                spec,
                Some(state),
                format!("index reconcile failed: {e}"),
            );
        }
    };
    fold(store, spec, state, report)
}

/// Whether a feed error is a probe failure, not a failed put or remove.
///
/// #10746 S12: `trusty_okf::feed::FeedError` carries no kind, so this
/// matches the exact message prefixes `feed::pass` writes for its probe
/// arms: `probe ` (`probe failed: …`, `probe could not run: …`),
/// `withdrawal probe ` and `recheck could not run`. Every other per-path
/// message (`put not confirmed`, `remove …`, `unreadable: …`) is a task
/// failure.
/// Test: `identity_change_with_refused_own_put_blocks_publication`.
fn is_probe_failure(message: &str) -> bool {
    ["probe ", "withdrawal probe ", "recheck could not run"]
        .iter()
        .any(|p| message.starts_with(p))
}

/// Fold a finished pass into the agent's report.
fn fold(
    store: &KbStore,
    spec: &SourceSpec,
    state: &IndexStateRef,
    r: FeedReport,
) -> IndexFeedReport {
    let index = state.index_id.as_str();
    let pass_level: Vec<&str> = r
        .errors
        .iter()
        .filter(|e| e.path.is_empty())
        .map(|e| e.message.as_str())
        .collect();
    if r.status == FeedStatus::NotFed && !pass_level.is_empty() {
        let reason = format!(
            "index `{index}` could not be interrogated: {}",
            pass_level.join("; ")
        );
        return unfed(store, spec, Some(state), reason);
    }
    let mut errors: Vec<String> = r
        .errors
        .iter()
        .map(|e| {
            if e.path.is_empty() {
                e.message.clone()
            } else {
                format!("{}: {}", e.path, e.message)
            }
        })
        .collect();
    // #10746 S12: only a pass that ended Searchable read a known, matching
    // identity for every record (§5.4), so only it confirms `synced`.
    let confirmed = r.status == FeedStatus::Searchable;
    let read = store
        .index_view(Some(state))
        .and_then(|v| store.source_index_state(spec, v.as_ref(), confirmed));
    // An unreadable coverage claims nothing and carries a reason, so
    // `acknowledged` is false (fail closed).
    // #10746 S12: every error this pass raised on one of this source's paths
    // blocks, except a probe failure (acknowledgement, as before S12). The
    // offline `blocking` alone misses a refused re-push after an identity
    // change (F14).
    let (cov, mut blocked, reason) = match read {
        Ok(st) => {
            let mut blocked = st.blocking;
            blocked.extend(
                r.errors
                    .iter()
                    .filter(|e| st.paths.contains(&e.path) && !is_probe_failure(&e.message))
                    .map(|e| e.path.clone()),
            );
            (st.coverage, blocked, None)
        }
        Err(e) => (
            crate::kb::okg::IndexCoverage {
                pending: r.pending,
                ..Default::default()
            },
            Vec::new(),
            Some(format!("index coverage could not be read: {e}")),
        ),
    };
    blocked.sort();
    blocked.dedup();
    errors.extend(cov.notes);
    IndexFeedReport {
        index: Some(index.to_string()),
        indexed: r.acknowledged,
        removed: r.removed,
        pending: cov.pending,
        synced: cov.synced,
        unconfirmed: cov.unconfirmed,
        status: Some(r.status),
        reason,
        errors,
        source_blocked: blocked,
    }
}

#[cfg(test)]
#[path = "okg_feed_tests.rs"]
mod tests;

// #10746 S12 fix round: tests of the feed lock and the `unconfirmed` field,
// kept apart so the red-proof run can pair the older code with
// `okg_feed_tests.rs`.
#[cfg(test)]
#[path = "okg_feed_lock_tests.rs"]
mod lock_tests;
