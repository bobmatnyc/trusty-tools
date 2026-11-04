//! This machine's index state for an OKG tree, read through trusty-okf.
//!
//! Why: an acknowledgement and a probe describe the search daemon that
//! answered them, and each machine runs its own daemon. The old in-tree
//! journal (`_sources/<id>.index.jsonl`) synced with the tree, so a second
//! machine read Acknowledged state its own daemon never received
//! (SPEC-OKF-01 §4.9.1, the r4 BLOCKER 5 defect). #10746 S12 moves the agent
//! onto `trusty_okf::state::index`, with the state directory in this
//! machine's trusty-agents data area, outside every synced tree (§3 (r) A).
//! What: [`IndexStateRef`] names one bound index's state on this machine;
//! [`IndexStateRef::machine_local`] resolves it under
//! `<data dir>/trusty-agents/okf-index-state`. [`KbStore::index_view`] reads
//! the trusty-okf backlog for the tree, and [`KbStore::okg_index_coverage_of`]
//! folds it per source into [`IndexCoverage`]. Read-only, no network: the
//! live identity is reported unreadable, so recorded bytes decide (§4.9.2
//! rule 4) and nothing here can read Searchable.
//! Test: `coverage_is_pending_until_this_machines_daemon_confirms`,
//! `two_state_dirs_over_one_tree_keep_separate_state`.
//!
//! # Spec References
//! - SPEC-OKF-01 §4.9.1, §4.9.2 and §5.4, held in the trusty-okf project
//!   directory.

use std::path::{Path, PathBuf};

use trusty_okf::build::ledger::{ItemStatus, Ledger};
use trusty_okf::bundle::Bundle;
use trusty_okf::state::{IndexBacklog, IndexStateDir, LiveIdentity, TaskOp};

use crate::kb::okg::IndexCoverage;
use crate::kb::okg::registry::SourceSpec;
use crate::kb::store::KbStore;

/// The data-area application name the state directory lives under.
const DATA_APP: &str = "trusty-agents";

/// The state directory under the trusty-agents data area.
pub const INDEX_STATE_SUBDIR: &str = "okf-index-state";

/// One bound search index's state on this machine.
#[derive(Debug, Clone)]
pub struct IndexStateRef {
    /// The caller-supplied, machine-local directory (§4.9.1).
    pub dir: IndexStateDir,
    /// The bound trusty-search index ID.
    pub index_id: String,
}

impl IndexStateRef {
    /// State for `index_id` under `dir`.
    pub fn new(dir: impl Into<PathBuf>, index_id: impl Into<String>) -> Self {
        Self {
            dir: IndexStateDir::new(dir),
            index_id: index_id.into(),
        }
    }

    /// State for `index_id` in this machine's trusty-agents data area.
    ///
    /// Why: §4.9.1 binds the caller to pass a directory outside the synced
    /// tree; the OS data directory (`~/Library/Application Support`,
    /// `~/.local/share`) is per machine and is never a KB tree.
    /// What: `<resolve_data_dir("trusty-agents")>/okf-index-state`. trusty-okf
    /// still refuses it with `StateDirInBundle` when a tree contains it.
    /// Test: `machine_local_state_lives_in_the_data_area`.
    pub fn machine_local(index_id: &str) -> anyhow::Result<Self> {
        let base = trusty_common::resolve_data_dir(DATA_APP)?;
        Ok(Self::new(base.join(INDEX_STATE_SUBDIR), index_id))
    }

    /// The state directory.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// The live identity an offline read reports: unreadable, so a record's
/// bytes decide and no read is Searchable (§4.9.2 rule 4).
fn offline() -> LiveIdentity {
    LiveIdentity::Unreadable("offline read: no daemon was asked".to_string())
}

impl KbStore {
    /// This machine's index backlog for the tree, or `None` with no state.
    ///
    /// Why: one backlog read serves every source of a status view.
    /// What: opens the tree as a trusty-okf bundle and diffs it against the
    /// journal in `state`. A state directory inside the tree is an error
    /// (`StateDirInBundle`). Writes nothing.
    pub fn index_view(
        &self,
        state: Option<&IndexStateRef>,
    ) -> anyhow::Result<Option<IndexBacklog>> {
        let Some(state) = state else {
            return Ok(None);
        };
        let (bundle, _) = Bundle::open(&self.root)?;
        Ok(Some(bundle.index_backlog(
            &state.dir,
            &state.index_id,
            &offline(),
        )?))
    }

    /// One source's index coverage on this machine (#3892, #10746 S12).
    ///
    /// Why: "ingested" and "findable" are two claims; the second must come
    /// from this machine's own index state, never from the synced tree.
    /// What: for each ledger row, a live item is `synced` when the backlog
    /// owes it nothing (pushed at its current bytes and probed) and
    /// `pending` otherwise; a tombstoned item is `synced` once its
    /// withdrawal is confirmed and `pending` while a remove is owed. A live
    /// row whose file is missing or unreadable is `pending` and noted. With
    /// no `view` (no bound index), every row is `pending`.
    /// Test: `coverage_is_pending_until_this_machines_daemon_confirms`,
    /// `sources_report_index_coverage`.
    pub fn okg_index_coverage_of(
        &self,
        spec: &SourceSpec,
        view: Option<&IndexBacklog>,
    ) -> anyhow::Result<IndexCoverage> {
        let ledger = Ledger::load(&self.root, &spec.id)?;
        let mut cov = IndexCoverage::default();
        for record in ledger.records() {
            let slug = record.entity.rsplit('/').next().unwrap_or(&record.entity);
            let path = self.entity_path(&spec.collection, slug)?;
            let rel = self.rel(&path).replace('\\', "/");
            let live = record.status == ItemStatus::Ingested;
            let unreadable = view.and_then(|v| v.unreadable.iter().find(|u| u.path == rel));
            if live && (!path.is_file() || unreadable.is_some()) {
                let why = unreadable.map_or("missing".to_string(), |u| u.error.clone());
                cov.notes.push(format!(
                    "{}: entity {} is not readable at {} ({why})",
                    record.item_id,
                    record.entity,
                    path.display()
                ));
                cov.pending += 1;
                continue;
            }
            let Some(view) = view else {
                cov.pending += 1;
                continue;
            };
            match view.task(&rel) {
                Some(TaskOp::Recheck) => cov.synced += 1,
                Some(_) => cov.pending += 1,
                // A tombstoned row with no file and no record owes nothing.
                None if live || path.is_file() => cov.synced += 1,
                None => {}
            }
        }
        Ok(cov)
    }
}

#[cfg(test)]
#[path = "index_state_tests.rs"]
mod tests;
