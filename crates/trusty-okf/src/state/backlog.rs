//! The index backlog: what one machine's index still owes (SPEC-OKF-01 §5.4
//! "Journal coverage", §4.9.2 rules 3 and 5).
//!
//! Why: the feed must push every concept on disk, hand-owned ones included,
//! and withdraw every journaled path that no longer has a live file. A
//! backlog that walks ledger records only never pushes a hand-written concept
//! and never withdraws a moved one (criterion 50).
//! What: [`Bundle::index_backlog`] diffs the concept files on disk against
//! this machine's index journal and returns one [`IndexTask`] per path. Pure:
//! it reads the bundle and the journal, never the network (B8). The live
//! identity decides re-pushes: under a known identity a record observed
//! against another identity is pushed again (rule 3); without D1 the journaled
//! byte hash alone decides, so an unchanged bundle owes nothing (rule 5).
//! Slice S7 of #10746.
//! Test: `c50_hand_owned_concept_and_moved_path_enter_the_backlog`,
//! `c49_journal_records_the_bytes_sent`.

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::paths::{confined_file, io_err};
use crate::format::concept::parse_concept;
use crate::format::hash::bytes_hash;
use crate::state::index::{
    Folded, IndexIdentity, IndexJournal, IndexRecord, IndexStateDir, LiveIdentity, fold,
    journaled_paths,
};
use crate::state::read::withdraw_intended;

/// What one task asks of the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOp {
    /// `file.put` the current bytes; `replace` removes the journaled older
    /// revision first (F6, F7).
    Push {
        /// A put of other bytes, or under another identity, is journaled.
        replace: bool,
    },
    /// The current bytes are acknowledged; the retrieval probe is owed.
    Probe,
    /// The path must leave the index: no live file, deprecated or
    /// tombstoned (§3 (i) A, §4.8).
    Remove,
    /// The path is withdrawn; re-check that it has not come back (F9).
    Recheck,
}

/// One path the feed must act on (§5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexTask {
    /// The concept file name, bundle-relative (`notes/a.md`).
    pub path: String,
    /// What to do.
    pub op: TaskOp,
}

/// A concept file the backlog could not read (F10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    /// The concept file name, bundle-relative.
    pub path: String,
    /// Why it could not be read.
    pub error: String,
}

/// The tasks one feed pass owes, in path order (§5.4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexBacklog {
    /// Paths to push, probe, withdraw or re-check.
    pub tasks: Vec<IndexTask>,
    /// Concept files that could not be read; each stays Pending (F10).
    pub unreadable: Vec<Unreadable>,
}

impl IndexBacklog {
    /// Paths not yet in their target index state: every task except a
    /// [`TaskOp::Recheck`], plus every unreadable file (criterion 43).
    pub fn owed(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.op != TaskOp::Recheck)
            .count()
            + self.unreadable.len()
    }

    /// The task for `path`, if any.
    pub fn task(&self, path: &str) -> Option<TaskOp> {
        self.tasks.iter().find(|t| t.path == path).map(|t| t.op)
    }
}

/// Whether a recorded identity still holds for backlog purposes: under a
/// known live identity it must be equal (§4.9.2 rule 3); without one the
/// journaled bytes alone decide (rule 5).
fn identity_ok(recorded: &Option<IndexIdentity>, live: &LiveIdentity) -> bool {
    match live {
        LiveIdentity::Known(l) => recorded.as_ref() == Some(l),
        LiveIdentity::Unsupported | LiveIdentity::Unreadable(_) => true,
    }
}

/// Remove, unless the path is already removed and probed under the live
/// identity, in which case only the F9 re-check is owed.
fn withdraw_op(f: &Folded, live: &LiveIdentity) -> TaskOp {
    match &f.removed {
        Some(i) if f.withdrawn && identity_ok(i, live) => TaskOp::Recheck,
        _ => TaskOp::Remove,
    }
}

/// The op a live concept at `disk` bytes owes, if any.
fn live_op(f: &Folded, disk: &str, live: &LiveIdentity) -> Option<TaskOp> {
    match &f.acked {
        Some((b, i)) if b == disk && identity_ok(i, live) => {
            (!f.searchable).then_some(TaskOp::Probe)
        }
        Some(_) => Some(TaskOp::Push { replace: true }),
        None => Some(TaskOp::Push { replace: false }),
    }
}

/// The backlog for `records` (one journal) over `b`'s files.
pub(crate) fn backlog_from(
    b: &Bundle,
    records: &[IndexRecord],
    live: &LiveIdentity,
) -> Result<IndexBacklog, OkfError> {
    let mut out = IndexBacklog::default();
    let mut on_disk = std::collections::BTreeSet::new();
    for id in b.concept_ids()? {
        let path = id.file_name();
        on_disk.insert(path.clone());
        let file = confined_file(b.root(), &path)?;
        let read = std::fs::read(&file)
            .map_err(|e| io_err(&file, e).to_string())
            .and_then(|bytes| {
                let text = String::from_utf8(bytes.clone())
                    .map_err(|_| "concept file is not UTF-8".to_string())?;
                let (c, _) = parse_concept(id.clone(), &text).map_err(|e| e.to_string())?;
                Ok((bytes, c))
            });
        let (bytes, c) = match read {
            Ok(v) => v,
            Err(error) => {
                out.unreadable.push(Unreadable { path, error });
                continue;
            }
        };
        let f = fold(records, &path);
        let op = if withdraw_intended(&c) {
            Some(withdraw_op(&f, live))
        } else {
            live_op(&f, &bytes_hash(&bytes), live)
        };
        if let Some(op) = op {
            out.tasks.push(IndexTask { path, op });
        }
    }
    for path in journaled_paths(records) {
        if !on_disk.contains(&path) {
            let op = withdraw_op(&fold(records, &path), live);
            out.tasks.push(IndexTask { path, op });
        }
    }
    Ok(out)
}

impl Bundle {
    /// What this machine's index for `index_id` still owes (SPEC-OKF-01
    /// §4.3 `index_backlog`, §5.4).
    ///
    /// Why: see the module doc. What: reads the bundle's concept files and
    /// the journal in `state` once; `live` is the identity the caller read
    /// from `search.index.status`. A state directory inside the bundle is
    /// `StateDirInBundle`. Writes nothing.
    /// Test: `c50_hand_owned_concept_and_moved_path_enter_the_backlog`.
    pub fn index_backlog(
        &self,
        state: &IndexStateDir,
        index_id: &str,
        live: &LiveIdentity,
    ) -> Result<IndexBacklog, OkfError> {
        let records = IndexJournal::open(state, self.root(), index_id)?.read()?;
        backlog_from(self, &records, live)
    }
}
