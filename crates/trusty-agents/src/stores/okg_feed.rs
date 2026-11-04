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
//! report `index: null` with a `reason`; none records anything.
//! Test: `crate::stores::okg_feed::tests` (two machines over one tree, the
//! fixture-journal migration, and each error arm), and the
//! `crate::tools::okg` and knowledge-pipeline suites that drive it.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.7.2 (index_feed row), §4.9.1 and §5.4, held in the
//!   trusty-okf project directory.

use serde::Serialize;
use trusty_okf::OkfError;
use trusty_okf::bundle::Bundle;
use trusty_okf::feed::{FeedReport, FeedStatus, IndexFeed};

use crate::kb::okg::index_state::IndexStateRef;
use crate::kb::okg::legacy_journal::migrate_legacy_journals;
use crate::kb::okg::registry::SourceSpec;
use crate::kb::store::KbStore;

/// What one feed call did — the caller-facing summary.
///
/// Why: the honesty half of #3892. `pending` counts this source's entities
/// that are in the tree and not yet confirmed searchable on this machine.
/// What: `indexed` and `removed` count what the pass confirmed across the
/// tree (trusty-okf feeds every concept, §5.4); `pending` and `synced` are
/// this source's coverage after the pass; `status` is the pass verdict.
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
    /// This source's entities current in this machine's index.
    pub synced: usize,
    /// How the pass ended, when it ran (§5.4; only `searchable` is full
    /// success, and none reads searchable before index identities exist).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<FeedStatus>,
    /// Why nothing was fed, when `index` is `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Per-path failures and coverage notes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
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

    /// Whether every put and remove of the pass was confirmed.
    ///
    /// Why: the knowledge pipeline marks a publication complete on
    /// acknowledgement, as before S12; a failed or refused task, or a pass
    /// that never ran, is not acknowledged (§5.4 F1-F5).
    /// Test: `refused_put_is_partial_and_records_nothing`.
    pub fn acknowledged(&self, index: &str) -> bool {
        self.reason.is_none()
            && self.index.as_deref() == Some(index)
            && matches!(
                self.status,
                Some(FeedStatus::Searchable | FeedStatus::Unconfirmed)
            )
    }
}

/// This source's coverage, or zeroes when even the read fails.
fn coverage(store: &KbStore, spec: &SourceSpec, state: Option<&IndexStateRef>) -> (usize, usize) {
    store
        .index_view(state)
        .and_then(|v| store.okg_index_coverage_of(spec, v.as_ref()))
        .map_or((0, 0), |c| (c.pending, c.synced))
}

/// Coverage for a source whose feed could not run.
///
/// Why: "not fed" without the backlog size tells the caller nothing.
pub fn unfed(
    store: &KbStore,
    spec: &SourceSpec,
    state: Option<&IndexStateRef>,
    reason: String,
) -> IndexFeedReport {
    let (pending, synced) = coverage(store, spec, state);
    IndexFeedReport::not_fed(reason, pending, synced)
}

/// Feed the tree into `state`'s index and report this source's coverage.
///
/// Why/What: see the module doc. Never fails the ingest: every error is a
/// `reason` on a not-fed report.
/// Test: `two_machines_over_one_tree_keep_separate_index_state`,
/// `daemon_down_is_not_fed_and_records_nothing`,
/// `root_mismatch_is_refused_before_any_push`,
/// `journal_write_failure_is_not_fed`.
pub async fn feed_source(
    store: &KbStore,
    spec: &SourceSpec,
    state: &IndexStateRef,
    sink: &dyn IndexFeed,
) -> IndexFeedReport {
    let index = state.index_id.as_str();
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
    // #10746 S12: an unreadable coverage claims nothing synced and carries a
    // reason, so `acknowledged` is false (fail closed).
    let (pending, synced, reason) = match store
        .index_view(Some(state))
        .and_then(|v| store.okg_index_coverage_of(spec, v.as_ref()))
    {
        Ok(c) => {
            errors.extend(c.notes);
            (c.pending, c.synced, None)
        }
        Err(e) => (
            r.pending,
            0,
            Some(format!("index coverage could not be read: {e}")),
        ),
    };
    IndexFeedReport {
        index: Some(index.to_string()),
        indexed: r.acknowledged,
        removed: r.removed,
        pending,
        synced,
        status: Some(r.status),
        reason,
        errors,
    }
}

#[cfg(test)]
#[path = "okg_feed_tests.rs"]
mod tests;
