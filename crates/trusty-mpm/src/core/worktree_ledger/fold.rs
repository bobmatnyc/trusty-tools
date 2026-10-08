//! Current worktree state as a pure fold over the ledger events (#8994).
//!
//! Why: an append-only file is only a registry if "what exists now" is a
//! deterministic function of it. Keeping the fold pure — no clock, no disk, no
//! git — is what lets the doctor row and `tm worktrees` agree, and what makes
//! a replay of the same file always give the same answer.
//! What: [`fold`] and [`LedgerState`], plus the per-project roll-up
//! [`ProjectSummary`] both reports print.
//! Test: `fold_created_measured_removed_leaves_nothing_live`,
//! `fold_keeps_one_live_entry_with_its_last_measured_size`,
//! `fold_replay_of_the_same_file_is_identical`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{EventKind, LedgerEvent, Origin};

/// Bytes in one GiB, the unit both reports print.
pub const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// One worktree the ledger says is live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveWorktree {
    /// The worktree path (the fold key).
    pub path: PathBuf,
    /// Main checkout or base clone it belongs to — the per-project key.
    pub repo: PathBuf,
    /// Branch, when recorded.
    pub branch: Option<String>,
    /// The route that recorded it.
    pub origin: Origin,
    /// `true` for an `observed` tree tm did not create.
    pub observed_only: bool,
    /// Managed session, for a `created` tree that had one.
    pub session: Option<String>,
    /// When it was first recorded.
    pub first_seen: DateTime<Utc>,
    /// Last measured size, `None` until a `measured` event arrives.
    pub bytes: Option<u64>,
    /// When `bytes` was measured.
    pub measured_at: Option<DateTime<Utc>>,
}

/// The live set the ledger describes, keyed and ordered by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerState {
    /// Live worktrees by path.
    pub live: BTreeMap<PathBuf, LiveWorktree>,
}

/// Fold `events`, in order, into the live set.
///
/// Why: see the module doc — state is derived, never stored.
/// What: `created`/`observed` make a path live (a second one for a path that is
/// already live is a no-op, so the first record keeps its attribution);
/// `measured` updates a live path's size and is ignored for a path that is not
/// live; `removed` drops the path. A path recorded again after `removed` is a
/// new live entry.
/// Test: `fold_created_measured_removed_leaves_nothing_live`,
/// `fold_keeps_one_live_entry_with_its_last_measured_size`,
/// `fold_replay_of_the_same_file_is_identical`.
pub fn fold(events: &[LedgerEvent]) -> LedgerState {
    let mut live: BTreeMap<PathBuf, LiveWorktree> = BTreeMap::new();
    for event in events {
        match &event.kind {
            EventKind::Created {
                repo,
                branch,
                origin,
                session,
            } => {
                live.entry(event.path.clone())
                    .or_insert_with(|| LiveWorktree {
                        path: event.path.clone(),
                        repo: repo.clone(),
                        branch: branch.clone(),
                        origin: *origin,
                        observed_only: false,
                        session: session.clone(),
                        first_seen: event.ts,
                        bytes: None,
                        measured_at: None,
                    });
            }
            EventKind::Observed {
                repo,
                branch,
                origin,
            } => {
                live.entry(event.path.clone())
                    .or_insert_with(|| LiveWorktree {
                        path: event.path.clone(),
                        repo: repo.clone(),
                        branch: branch.clone(),
                        origin: *origin,
                        observed_only: true,
                        session: None,
                        first_seen: event.ts,
                        bytes: None,
                        measured_at: None,
                    });
            }
            EventKind::Measured { bytes } => {
                if let Some(entry) = live.get_mut(&event.path) {
                    entry.bytes = Some(*bytes);
                    entry.measured_at = Some(event.ts);
                }
            }
            EventKind::Removed => {
                live.remove(&event.path);
            }
        }
    }
    LedgerState { live }
}

/// Count and size of one project's live worktrees.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectSummary {
    /// Main checkout or base clone — the project key.
    pub repo: PathBuf,
    /// Live worktrees.
    pub count: usize,
    /// Sum of the last measured sizes.
    pub bytes: u64,
    /// `bytes` in GiB, rounded to two decimals.
    pub gib: f64,
    /// Live worktrees with no measurement yet (not in `bytes`).
    pub unmeasured: usize,
}

/// `bytes` in GiB, rounded to two decimals.
pub fn gib(bytes: u64) -> f64 {
    (bytes as f64 / GIB * 100.0).round() / 100.0
}

impl LedgerState {
    /// Per-project roll-up, ordered by repo path.
    ///
    /// What: groups the live set by `repo`; an unmeasured tree counts toward
    /// `count` and `unmeasured`, never toward `bytes`.
    /// Test: `summary_groups_by_repo_with_count_and_gib`.
    pub fn by_project(&self) -> Vec<ProjectSummary> {
        let mut groups: BTreeMap<&PathBuf, (usize, u64, usize)> = BTreeMap::new();
        for wt in self.live.values() {
            let g = groups.entry(&wt.repo).or_default();
            g.0 += 1;
            match wt.bytes {
                Some(b) => g.1 += b,
                None => g.2 += 1,
            }
        }
        groups
            .into_iter()
            .map(|(repo, (count, bytes, unmeasured))| ProjectSummary {
                repo: repo.clone(),
                count,
                bytes,
                gib: gib(bytes),
                unmeasured,
            })
            .collect()
    }
}
