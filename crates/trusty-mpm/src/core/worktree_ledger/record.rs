//! Writing to the ledger: the provisioning hook and the size measurement
//! (#8994).
//!
//! Why: a registry a creation route can skip is not a registry. Every tm route
//! that creates a worktree calls [`create_recorded`], which refuses the
//! creation when the ledger cannot be written rather than creating an
//! unrecorded tree and logging a warning.
//! What: [`create_recorded`] and [`measure_live`].
//! Test: `create_recorded_refuses_before_creating_when_the_ledger_is_unwritable`,
//! `create_recorded_appends_one_created_event`,
//! `measure_live_records_the_size_of_each_present_tree`.

use std::path::{Path, PathBuf};

use super::fold::LedgerState;
use super::{EventKind, LedgerError, LedgerEvent, Origin, WorktreeLedger, ledger_key};

/// What the ledger needs to know about a tree a route is about to create.
#[derive(Debug, Clone)]
pub struct CreationIntent<'a> {
    /// Main checkout or base clone the tree belongs to.
    pub repo: &'a Path,
    /// Branch the tree will check out.
    pub branch: Option<String>,
    /// The creating route.
    pub origin: Origin,
    /// Managed session the tree is for.
    pub session: Option<String>,
}

/// Why a recorded creation failed.
#[derive(Debug, thiserror::Error)]
pub enum RecordedCreateError {
    /// The ledger could not be opened or written.
    #[error("worktree not recorded, creation refused: {0}")]
    Ledger(#[from] LedgerError),
    /// The creation itself failed; the ledger is untouched.
    #[error("{0}")]
    Create(String),
}

/// Create a worktree through `create` and record it, or refuse.
///
/// Why (#8994): an append error must reach the caller. Opening the ledger
/// BEFORE `create` runs means the common failure — an unwritable ledger —
/// refuses with nothing created on disk.
/// What: opens the ledger for append (error → [`RecordedCreateError::Ledger`],
/// `create` never runs), runs `create` (error → [`RecordedCreateError::Create`],
/// nothing appended), then appends one `created` event keyed by
/// [`ledger_key`]. If that final write fails the tree exists but the call still
/// returns `Err`, so the caller does not proceed; the next backfill records it.
/// Test: `create_recorded_refuses_before_creating_when_the_ledger_is_unwritable`,
/// `create_recorded_appends_one_created_event`,
/// `create_recorded_appends_nothing_when_creation_fails`.
pub fn create_recorded(
    ledger: &WorktreeLedger,
    intent: CreationIntent<'_>,
    create: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<PathBuf, RecordedCreateError> {
    let mut appender = ledger.open_appender()?;
    let path = create().map_err(RecordedCreateError::Create)?;
    appender.append(&LedgerEvent::now(
        ledger_key(&path),
        EventKind::Created {
            repo: ledger_key(intent.repo),
            branch: intent.branch,
            origin: intent.origin,
            session: intent.session,
        },
    ))?;
    Ok(path)
}

/// Outcome of one [`measure_live`] pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MeasureReport {
    /// Trees measured and recorded.
    pub measured: usize,
    /// Live trees whose directory is gone; left for slice 2's reconciliation.
    pub missing: usize,
}

/// Measure every live tree in `state` that still exists and record the size.
///
/// Why: the doctor row reads only the ledger, so a size reaches it only as a
/// `measured` event; `tm worktrees` is the route that takes the measurement.
/// What: for each live path that is a directory, appends one `measured` event
/// with its allocated bytes (`du` semantics). A missing directory is counted,
/// not removed — slice 1 adds no remove route.
/// Test: `measure_live_records_the_size_of_each_present_tree`.
pub fn measure_live(
    ledger: &WorktreeLedger,
    state: &LedgerState,
) -> Result<MeasureReport, LedgerError> {
    let mut report = MeasureReport::default();
    if state.live.is_empty() {
        return Ok(report);
    }
    let mut appender = ledger.open_appender()?;
    for path in state.live.keys() {
        if !path.is_dir() {
            report.missing += 1;
            continue;
        }
        let bytes = trusty_common::sys_metrics::dir_allocated_bytes(path);
        appender.append(&LedgerEvent::now(
            path.clone(),
            EventKind::Measured { bytes },
        ))?;
        report.measured += 1;
    }
    Ok(report)
}
