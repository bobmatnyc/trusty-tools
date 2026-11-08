//! The import report and the §4.10 exit code (SPEC-OKF-01 Appendix A).
//!
//! Why: the report is the one place an operator learns what an import did,
//! and §4.9 forbids reporting one axis on behalf of another. Saved is not
//! searchable, and an import with no feed pass leaves every touched concept
//! Pending (§4.10 phase 3).
//! What: [`ImportReport`] carries the Appendix A fields and one
//! [`ItemReport`] per item, each with one read outcome, one write outcome,
//! one index state and one verification state. [`ImportReport::tally`]
//! derives every count from the items, so a count can never disagree with
//! the list. Without a feed pass every touched concept is `pending`;
//! [`ImportReport::apply_index_states`] reads this machine's index states
//! after one (§4.9.1, §4.9.2). [`exit_code`] maps a result to §4.10: a
//! refusal before any write is 2; any other error 1; a report 3 when an
//! item was skipped or failed or the run was cancelled, else 4 when a feed pass ran and a touched
//! concept is not Searchable (or Withdrawn), else 0. Slice S8 of #10746.
//! Test: `c80_every_item_is_reported_once_with_a_reason`,
//! `c85_failed_feed_reports_not_fully_searchable_and_exits_4`,
//! `c144_no_child_processes_skips_worker_items_and_refuses_connectors`.

use serde::Serialize;

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::record::VerificationState;
use crate::format::id::ConceptId;
use crate::state::{IndexState, IndexStateDir, LiveIdentity};

/// One finding of the run (§2.13.1 `quarantined_key`, §4.8
/// `tombstone_undetectable`, an operation's `staging_left`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportFinding {
    /// `error`, `warning` or `info`.
    pub severity: String,
    /// The finding kind.
    pub kind: String,
    /// The item it is about.
    pub item_id: String,
    /// The key, path or note it names.
    pub detail: String,
}

/// One item's outcomes (§2.14 rule 12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemReport {
    /// The source's item ID.
    pub item_id: String,
    /// Its concept, when one was assigned.
    pub id: Option<ConceptId>,
    /// `read`, `skipped`, `failed` or `none` (a vanished item).
    pub read: String,
    /// `created`, `updated`, `unchanged`, `tombstoned`, `failed`, `missing`
    /// or `none`.
    pub write: String,
    /// The skip, failure or conflict reason.
    pub reason: Option<String>,
    /// The index state of its concept, when it has one on disk.
    pub index: Option<String>,
    /// `verified` or `unverified`, when it has a concept on disk.
    pub verification: Option<String>,
    /// The concept's old verification events moved to history this run.
    pub invalidated: bool,
    /// #10746 S9: a recorded loss, for example `snippet_only` (§2.15 item
    /// 11); counted in [`ImportReport::degraded`].
    pub degraded: Option<String>,
}

impl ItemReport {
    pub(crate) fn new(item_id: &str, id: Option<ConceptId>, read: &str, write: &str) -> Self {
        ItemReport {
            item_id: item_id.to_string(),
            id,
            read: read.to_string(),
            write: write.to_string(),
            reason: None,
            index: None,
            verification: None,
            invalidated: false,
            degraded: None,
        }
    }

    pub(crate) fn because(mut self, reason: &str) -> Self {
        self.reason = Some(reason.to_string());
        self
    }

    /// A concept this run touched: it was created, updated, left unchanged
    /// or tombstoned.
    fn touched(&self) -> bool {
        matches!(
            self.write.as_str(),
            "created" | "updated" | "unchanged" | "tombstoned"
        )
    }
}

/// What one import did (SPEC-OKF-01 Appendix A).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// The run's attempt ID (`<plan run ID>-<n>`).
    pub run_id: String,
    /// The store ID.
    pub store: String,
    /// Entries the source returned.
    pub listed: usize,
    /// No page remained under the ceiling (§2.13).
    pub pagination_exhausted: bool,
    /// Tombstones were allowed.
    pub deletion_safe: bool,
    /// Where a resumed listing starts.
    pub cursor: Option<String>,
    /// Items fetched and extracted.
    pub read: usize,
    /// Items not read, each with a §2.14 rule 8 reason.
    pub skipped: usize,
    /// Items whose fetch, addressing or extraction failed.
    pub failed: usize,
    /// Items read with a recorded loss.
    pub degraded: usize,
    /// New concepts.
    pub created: usize,
    /// Changed concepts.
    pub updated: usize,
    /// Current items; nothing written.
    pub unchanged: usize,
    /// Concepts tombstoned.
    pub tombstoned: usize,
    /// `created + updated + tombstoned`.
    pub written: usize,
    /// Items read but not saved: `Conflict`, `collision` or I/O.
    pub write_failed: usize,
    /// Items absent from a listing that is not deletion-safe.
    pub missing: usize,
    /// Touched concepts not yet in their index state; every one when no
    /// feed pass ran.
    pub pending: usize,
    /// Touched concepts whose bytes the daemon confirmed, unprobed.
    pub acknowledged: usize,
    /// Touched live concepts whose probe passed.
    pub searchable: usize,
    /// Touched withdrawn concepts confirmed out of the index.
    pub withdrawn: usize,
    /// Touched concepts whose current revision is Verified.
    pub verified_current: usize,
    /// Touched concepts whose old events moved to history this run.
    pub verification_invalidated: usize,
    /// Every touched live concept Searchable and every withdrawn one
    /// Withdrawn, under a feed pass.
    pub fully_searchable: bool,
    /// No skip, failure, write failure or loss, and an exhausted listing.
    pub complete: bool,
    /// Whether a feed pass's index states are in this report.
    pub fed: bool,
    /// The §4.10 exit code.
    pub exit_code: i32,
    /// One entry per item.
    pub items: Vec<ItemReport>,
    /// The run's findings.
    pub findings: Vec<ImportFinding>,
    /// #10746 S10: run-level extraction-worker failures (§2.16). Any entry
    /// makes the run incomplete and its exit code at least 3.
    pub worker_errors: Vec<crate::WorkerError>,
    /// #10746 S9+S10 merge: the run was cancelled (SIGINT on the CLI). A
    /// cancelled run is incomplete and exits 3, even with no item listed.
    pub cancelled: bool,
}

fn index_name(s: &IndexState) -> &'static str {
    match s {
        IndexState::NotIndexed => "not_indexed",
        IndexState::Pending { .. } => "pending",
        IndexState::Acknowledged => "acknowledged",
        IndexState::Searchable => "searchable",
        IndexState::WithdrawPending => "withdraw_pending",
        IndexState::Withdrawn => "withdrawn",
    }
}

impl ImportReport {
    /// Recompute every count, `complete`, `fully_searchable` and
    /// `exit_code` from the items.
    pub fn tally(&mut self) {
        let count = |f: &dyn Fn(&ItemReport) -> bool| self.items.iter().filter(|i| f(i)).count();
        self.read = count(&|i| i.read == "read");
        self.skipped = count(&|i| i.read == "skipped");
        self.failed = count(&|i| i.read == "failed");
        self.created = count(&|i| i.write == "created");
        self.updated = count(&|i| i.write == "updated");
        self.unchanged = count(&|i| i.write == "unchanged");
        self.tombstoned = count(&|i| i.write == "tombstoned");
        self.write_failed = count(&|i| i.write == "failed");
        self.missing = count(&|i| i.write == "missing");
        self.degraded = count(&|i| i.degraded.is_some());
        self.written = self.created + self.updated + self.tombstoned;
        let idx = |name: &str| count(&|i| i.touched() && i.index.as_deref() == Some(name));
        self.acknowledged = idx("acknowledged");
        self.searchable = idx("searchable");
        self.withdrawn = idx("withdrawn");
        self.pending = count(&|i| {
            i.touched()
                && !matches!(
                    i.index.as_deref(),
                    Some("acknowledged" | "searchable" | "withdrawn")
                )
        });
        self.verified_current =
            count(&|i| i.touched() && i.verification.as_deref() == Some("verified"));
        self.verification_invalidated = count(&|i| i.invalidated);
        // A live concept reads Withdrawn only when it should leave the
        // index (deprecated or tombstoned), so the two states are the goal.
        self.fully_searchable = self.fed
            && self
                .items
                .iter()
                .filter(|i| i.touched())
                .all(|i| matches!(i.index.as_deref(), Some("searchable" | "withdrawn")));
        self.complete = self.skipped + self.failed + self.write_failed + self.degraded == 0
            && self.pagination_exhausted
            && self.worker_errors.is_empty()
            && !self.cancelled;
        // #10746 S10: a run-level worker failure is never reported as a
        // clean run, even when every committed item kept its outcome.
        self.exit_code = if self.skipped + self.failed + self.write_failed > 0
            || !self.worker_errors.is_empty()
            || self.cancelled
        {
            3
        } else if self.fed && !self.fully_searchable {
            4
        } else {
            0
        };
    }

    /// Read each touched concept's three states from `bundle` and this
    /// machine's index journal, after a feed pass (§4.9.1, §4.9.2).
    ///
    /// What: sets `fed`, then each touched item's `index` and
    /// `verification` from `Bundle::revision_state`, and re-tallies. A
    /// concept no longer on disk reads `pending`. Errors: the state
    /// directory inside the bundle (`StateDirInBundle`) or an I/O failure.
    pub fn apply_index_states(
        &mut self,
        bundle: &Bundle,
        state: &IndexStateDir,
        index_id: &str,
        live: &LiveIdentity,
    ) -> Result<(), OkfError> {
        self.fed = true;
        for item in self.items.iter_mut().filter(|i| i.touched()) {
            let Some(id) = &item.id else { continue };
            match bundle.revision_state(id, state, index_id, live) {
                Ok(rs) => {
                    item.index = Some(index_name(&rs.index).to_string());
                    item.verification = Some(verification_name(&rs.verification).into());
                }
                Err(OkfError::NotFound(_)) => item.index = Some("pending".into()),
                Err(e) => return Err(e),
            }
        }
        self.tally();
        Ok(())
    }

    /// Fill each touched item's verification axis from the record, and its
    /// index axis as `pending` (no feed pass ran, §4.10 phase 3).
    pub(crate) fn unfed_states(&mut self, bundle: &Bundle) -> Result<(), OkfError> {
        for item in self.items.iter_mut().filter(|i| i.touched()) {
            let Some(id) = &item.id else { continue };
            item.index = Some(if item.write == "tombstoned" {
                "withdraw_pending".into()
            } else {
                "pending".into()
            });
            match bundle.verification_state(id) {
                Ok(v) => item.verification = Some(verification_name(&v).into()),
                Err(OkfError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        self.tally();
        Ok(())
    }
}

fn verification_name(v: &VerificationState) -> &'static str {
    match v {
        VerificationState::Verified { .. } => "verified",
        VerificationState::Unverified { .. } => "unverified",
    }
}

/// The SPEC-OKF-01 §4.10 exit code of an `import` call.
///
/// What: a report maps through [`ImportReport::exit_code`]. An error
/// `import` returns only before its first write — `SourceOverlap`,
/// `SelectionRequired`, `PolicyRefused` (run-level `policy_refused`), `RootMismatch`,
/// `StateDirInBundle`, `RecoveryPending`, `InvalidId`, `PathEscape` or
/// `Parse` (an invalid glob or registry) — is 2. Every other error is 1;
/// `import` turns a refusal-shaped error raised after its first write into
/// an I/O error, so it maps to 1.
/// Test: `c144_no_child_processes_skips_worker_items_and_refuses_connectors`,
/// `c82_overlapping_roots_are_refused`.
pub fn exit_code(result: &Result<ImportReport, OkfError>) -> i32 {
    match result {
        Ok(r) => r.exit_code,
        Err(e) if is_refusal(e) => 2,
        Err(_) => 1,
    }
}

/// Errors that `import` returns only before any write.
pub(crate) fn is_refusal(e: &OkfError) -> bool {
    matches!(
        e,
        OkfError::SourceOverlap { .. }
            | OkfError::SelectionRequired(_)
            | OkfError::PolicyRefused { .. }
            | OkfError::RootMismatch { .. }
            | OkfError::StateDirInBundle(_)
            | OkfError::RecoveryPending { .. }
            | OkfError::InvalidId { .. }
            | OkfError::PathEscape(_)
            | OkfError::Parse { .. }
    )
}
