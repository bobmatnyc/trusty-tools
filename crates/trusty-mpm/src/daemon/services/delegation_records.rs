//! The operator-facing view of a delegation record (#8257).
//!
//! Why: before this module the delegation map reached the wire only as
//! `{agent, count}` per directory, so a dispatch denied on a record nobody
//! could see left the operator with no id to act on and no verb to act with.
//! The deny text, the `tm repair delegation --list` listing and the repair verb
//! now share one projection of a record — its id, owner, age and the command
//! that clears it — so the three cannot disagree about which record is meant.
//! What: [`DelegationRecordView`], its [`repair_command`], and
//! [`list_for_dir`], the read-only listing behind `GET /api/v1/delegations`.
//! Test: `delegation_records_tests`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::agent::{Delegation, DelegationStatus};
use crate::daemon::state::DaemonState;

/// One delegation record as the dispatch deny and the listing show it.
///
/// Why: every field is one the operator needs to decide whether the record is
/// stale and to address it — `agent_id` is `None` for a record a stop matched by
/// agent type (#6556), which is why `delegation_id` always travels too.
/// What: a flat, serializable snapshot; `age_secs` is measured from
/// `started_at` (falling back to `created_at`), the clock the staleness sweep
/// uses, so it compares directly against the six-hour threshold.
/// Test: `record_view_carries_the_repair_command_for_each_id_shape_8257`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRecordView {
    /// The record's own id — the address for `--delegation-id`.
    pub delegation_id: String,
    /// The agent type, e.g. `version-control`.
    pub agent: String,
    /// The harness agent id, when a `PostToolUse` taught one.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// The session that dispatched the agent.
    pub session: String,
    /// The record's lifecycle status.
    pub status: DelegationStatus,
    /// Seconds since the agent started (or the record was created).
    pub age_secs: i64,
    /// The directory the dispatch was issued from.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// The tree the agent reported as its own, when it reported one.
    #[serde(default)]
    pub worktree_path: Option<PathBuf>,
    /// Whether this record blocks a file-mutating dispatch into `cwd`.
    #[serde(default)]
    pub blocks_dispatch: bool,
    /// The exact command that asks the daemon to clear this record.
    pub repair_command: String,
}

impl DelegationRecordView {
    /// Project one record, aged against `now`.
    pub fn of(d: &Delegation, now: chrono::DateTime<chrono::Utc>, blocks_dispatch: bool) -> Self {
        Self {
            delegation_id: d.id.0.to_string(),
            agent: d.agent.clone(),
            agent_id: d.agent_id.clone(),
            session: d.session.0.to_string(),
            status: d.status,
            age_secs: record_age_secs(d, now),
            cwd: d.cwd.clone(),
            worktree_path: d.worktree_path.clone(),
            blocks_dispatch,
            repair_command: repair_command(d),
        }
    }
}

/// Seconds since `d` started, on the staleness sweep's own clock.
pub(crate) fn record_age_secs(d: &Delegation, now: chrono::DateTime<chrono::Utc>) -> i64 {
    (now - d.started_at.unwrap_or(d.created_at)).num_seconds()
}

/// The exact `tm` command that clears `d` (#8257).
///
/// Why: a record a stop matched by agent type never learns an `agent_id`, and
/// `tm repair delegation <agent-id>` could not name it at all — the defect the
/// issue reports. Such a record is addressed by its own delegation id instead.
/// What: `tm repair delegation <agent-id>` when the record carries one, else
/// `tm repair delegation --delegation-id <uuid>`.
/// Test: `record_view_carries_the_repair_command_for_each_id_shape_8257`.
pub fn repair_command(d: &Delegation) -> String {
    match d.agent_id.as_deref() {
        Some(agent_id) => format!("tm repair delegation {agent_id}"),
        None => format!("tm repair delegation --delegation-id {}", d.id.0),
    }
}

/// The body of `GET /api/v1/delegations?cwd=<dir>` (#8257).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DelegationListing {
    /// The directory the listing was asked about, as received.
    pub cwd: PathBuf,
    /// Every non-terminal record issued from, or holding, that directory.
    pub records: Vec<DelegationRecordView>,
}

/// Every non-terminal record whose `cwd` or own tree is `dir`, oldest first.
///
/// Why: the read-only half of #8257 — before it, the only reads of the map were
/// the two POST routes that CLAIM a directory, and a probe that sent the
/// non-dispatch shape was answered the narrower HEAD-write question and told
/// "nobody" about a record that was blocking it. This asks nothing and writes
/// nothing.
/// What: `Stale` records are listed because they are not terminal — a
/// type-reconciled one still occupies the tree for a dispatch, and every one of
/// them still refuses its worktree's reclaim. `blocks_dispatch` is set from the
/// SAME occupancy filter the dispatch guard runs, so the listing and the deny
/// agree on which records block.
/// Test: `listing_names_every_open_record_in_a_directory_8257`.
pub fn list_for_dir(state: &DaemonState, dir: &Path) -> Vec<DelegationRecordView> {
    let now = chrono::Utc::now();
    let blocking: Vec<_> = state
        .shared_tree_records(dir, None, true, None)
        .into_iter()
        .map(|d| d.id)
        .collect();
    let mut records: Vec<_> = state
        .all_delegations()
        .into_iter()
        .filter(|d| {
            !d.status.is_terminal()
                && (d.cwd.as_deref() == Some(dir) || d.worktree_path.as_deref() == Some(dir))
        })
        .collect();
    records.sort_by_key(|d| d.started_at.unwrap_or(d.created_at));
    records
        .iter()
        .map(|d| DelegationRecordView::of(d, now, blocking.contains(&d.id)))
        .collect()
}

#[cfg(test)]
#[path = "delegation_records_tests.rs"]
mod delegation_records_tests;
