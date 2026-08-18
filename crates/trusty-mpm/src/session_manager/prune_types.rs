//! The prune result and filter types — pure data, no `SessionManager` (#5897).
//!
//! Why: [`super::prune`] carries the prune ENGINE, and it reached the 500-SLOC
//! production cap while #5912 was being fixed in it. These four types are the
//! natural seam: they are plain serializable data with no dependency on
//! `SessionManager`, the tmux driver, or the store, so lifting them out leaves
//! the engine file free to grow with the logic it actually owns. Splitting here
//! mirrors why `prune` itself was split out of [`super::manager`].
//! What: [`PruneFilter`] (which records a prune targets), and [`PruneAction`] /
//! [`PrunedSession`] / [`PruneOutcome`] (what a prune did, or would do under
//! `dry_run`). `super::prune` re-exports all four, so every existing
//! `prune::PruneFilter`-style path still resolves.
//! Test: `prune_filter_parse_round_trip`, `prune_outcome_serializes`, and the
//! per-filter `prune_*` tests in `super::tests`.

use std::fmt;

/// Which managed-session records a prune targets (#1508).
///
/// Why: one teardown tool must serve BOTH the ephemeral-cleanup case (tear down
/// every test session) AND the legacy-purge case (clear the 239 stale
/// stopped/decommissioned records that predate the `ephemeral` flag). Modelling
/// the target as a closed enum keeps the safety rule — never touch a RUNNING
/// (`Active`/`Provisioning`) record unless explicitly forced — in one place.
/// What: [`Ephemeral`](PruneFilter::Ephemeral) selects `ephemeral == true`
/// non-terminal records; [`Stopped`](PruneFilter::Stopped) selects `Stopped`
/// records; [`Decommissioned`](PruneFilter::Decommissioned) and
/// [`Deleted`](PruneFilter::Deleted) select existing tombstones (for compaction
/// only); [`All`](PruneFilter::All) selects every NON-running record (ephemeral,
/// stopped, errored, decommissioned, and deleted).
/// Test: `prune_filter_parse_round_trip`, and the per-filter `prune_*` tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneFilter {
    /// Only sessions tagged `ephemeral == true` (test/throwaway sessions).
    Ephemeral,
    /// Only `Stopped` sessions (runtime gone, workspace still on disk).
    Stopped,
    /// Only `Decommissioned` tombstones — compacted (removed) from the store.
    Decommissioned,
    /// Only `Deleted` tombstones (`--deleted--`) — compacted (removed) from the
    /// store. The permanent-removal path for soft-deleted records (#2012).
    Deleted,
    /// Every NON-running record: ephemeral, stopped, errored, decommissioned,
    /// and deleted.
    All,
}

impl PruneFilter {
    /// Parse a CLI/wire string into a [`PruneFilter`].
    ///
    /// Why: the CLI `--state` flag, the HTTP body, and the MCP tool all accept the
    /// same lowercase spellings; centralising the parse keeps them consistent and
    /// rejects typos with a single actionable message.
    /// What: maps `ephemeral`/`stopped`/`decommissioned`/`all` (case-insensitive,
    /// trimmed) to the matching variant; anything else is an `Err` naming the
    /// supported values.
    /// Test: `prune_filter_parse_round_trip` (covers both the round-trip and the
    /// garbage-rejection case).
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ephemeral" => Ok(Self::Ephemeral),
            "stopped" => Ok(Self::Stopped),
            "decommissioned" => Ok(Self::Decommissioned),
            "deleted" => Ok(Self::Deleted),
            "all" => Ok(Self::All),
            other => Err(format!(
                "unknown prune filter `{other}` (expected: ephemeral | stopped | decommissioned | deleted | all)"
            )),
        }
    }

    /// The canonical lowercase name of this filter.
    ///
    /// Why: responses echo the filter back so callers can confirm what ran.
    /// What: the inverse of [`parse`](Self::parse).
    /// Test: `prune_filter_parse_round_trip`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::Stopped => "stopped",
            Self::Decommissioned => "decommissioned",
            Self::Deleted => "deleted",
            Self::All => "all",
        }
    }
}

impl fmt::Display for PruneFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a prune did (or WOULD do under `dry_run`) to a single record (#1508).
///
/// Why: `decommission` (tear down a live/stopped session) and `remove` (drop an
/// existing tombstone from the store) are semantically distinct outcomes; the
/// caller wants to know which happened per session so a dry-run report is precise.
/// A THIRD outcome exists since the decommission-side dirty-worktree guard: a
/// record can be tombstoned while its in-project worktree is deliberately left
/// on disk because it held unsaved work. Before this variant existed,
/// `prune_managed` discarded the `bool` half of `decommission`'s
/// `(SessionRecord, bool)` return, so `tm sessions prune --state stopped`
/// printed the identical `decommissioned` line whether the worktree was
/// removed or silently retained — the refusal was invisible at the one
/// surface an operator actually reads.
/// What: [`Decommissioned`](PruneAction::Decommissioned) — killed runtime +
/// removed workspace (or the workspace was already gone/never SM-owned) +
/// tombstoned; [`DecommissionedWorktreeRetained`](PruneAction::DecommissionedWorktreeRetained)
/// — tombstoned, but the worktree held unsaved work and was deliberately NOT
/// removed (its path is echoed back on [`PrunedSession::retained_workspace_path`]);
/// [`Removed`](PruneAction::Removed) — an existing `Decommissioned`/`Deleted`
/// tombstone was deleted from the store (compaction).
/// Test: asserted by `decommission_all_ephemeral_ignores_non_ephemeral` (the
/// `Decommissioned` action), `prune_decommissioned_compacts` (the `Removed`
/// action), and `prune_reports_dirty_worktree_retained` (the new variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneAction {
    /// The session was torn down (runtime killed, workspace removed, tombstoned).
    Decommissioned,
    /// The session was tombstoned, but its in-project worktree held unsaved
    /// work and was deliberately NOT removed — see
    /// `worktree_safety::inspect_dirt`. The worktree remains on disk; its path
    /// is echoed on `PrunedSession::retained_workspace_path`.
    DecommissionedWorktreeRetained,
    /// An existing tombstone was deleted from the store (compaction).
    Removed,
}

impl PruneAction {
    /// The canonical lowercase name of this action (for wire/log rendering).
    ///
    /// Why: HTTP/MCP responses and the CLI dry-run render the action per row.
    /// What: `Decommissioned` → `"decommissioned"`,
    /// `DecommissionedWorktreeRetained` → `"decommissioned_worktree_retained"`,
    /// `Removed` → `"removed"`.
    /// Test: `prune_outcome_serializes`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Decommissioned => "decommissioned",
            Self::DecommissionedWorktreeRetained => "decommissioned_worktree_retained",
            Self::Removed => "removed",
        }
    }
}

/// One record a prune touched (or would touch), with enough identity to report.
///
/// Why: a dry-run must show the operator EXACTLY which sessions are in scope —
/// id, tmux name, and prior state — before anything is destroyed.
/// What: the session id, tmux name, the state the record was in, and the
/// [`PruneAction`] applied (or that would be applied under `dry_run`).
/// Test: `prune_dry_run_reports_without_mutating`,
/// `prune_reports_dirty_worktree_retained`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PrunedSession {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name.
    pub tmux_name: String,
    /// The lifecycle state the record was in before the prune.
    pub state: String,
    /// What the prune did (or would do under `dry_run`).
    pub action: PruneAction,
    /// The workspace path left on disk when `action` is
    /// [`PruneAction::DecommissionedWorktreeRetained`]; `None` for every other
    /// action (including a clean `Decommissioned`, whose workspace was
    /// removed and therefore has no on-disk path left to report).
    ///
    /// Why: the previous version of this struct had no way to tell a caller
    /// WHERE the retained work is — `decommission` itself now keeps
    /// `workspace_path` on a retained record for exactly this reason (see
    /// `decommission_with_root`'s tombstone comment), so this field just
    /// surfaces that same path at the prune report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_workspace_path: Option<std::path::PathBuf>,
}

/// The result of a prune: which sessions were (or would be) affected (#1508).
///
/// Why: the CLI, HTTP, and MCP surfaces all need a structured, serializable
/// summary that works for BOTH a real run and a `dry_run` preview.
/// What: `dry_run` (true → nothing was mutated), the targeted `filter`, and the
/// per-session [`PrunedSession`] list. `count()` is the total affected.
/// Test: `prune_outcome_serializes`, `prune_dry_run_reports_without_mutating`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PruneOutcome {
    /// True when this was a preview — no record was killed, removed, or tombstoned.
    pub dry_run: bool,
    /// The filter that selected the affected records.
    pub filter: String,
    /// Every session affected (or that would be affected under `dry_run`).
    pub sessions: Vec<PrunedSession>,
}

impl PruneOutcome {
    /// Number of sessions affected (or that would be affected).
    ///
    /// Why: callers log/print "pruned N sessions" without re-deriving the length.
    /// What: returns `self.sessions.len()`.
    /// Test: `decommission_all_ephemeral_ignores_non_ephemeral` (asserts the count).
    pub fn count(&self) -> usize {
        self.sessions.len()
    }
}
