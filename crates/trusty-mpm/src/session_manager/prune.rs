//! Bulk teardown + by-state pruning + tombstone compaction for managed sessions (#1508).
//!
//! Why: the `SessionRecord` store was monotonically append-only and accumulated
//! 239 stale TEST sessions — `decommission` wrote a tombstone but never deleted,
//! `store.remove()` was never called in production, and there was no test/ephemeral
//! marker or bulk teardown. This module adds the two missing teardown verbs and the
//! compaction pass so the store stops growing unbounded, all on top of the existing
//! per-session [`SessionManager::decommission`] internals (tmux kill + workspace
//! removal + tombstone). It lives in its own file so [`super::manager`] stays under
//! the 500-SLOC production cap (mirroring [`super::adopt`]).
//! What: [`PruneFilter`] (which records a prune targets), [`PruneAction`] /
//! [`PrunedSession`] / [`PruneOutcome`] (what a prune did or WOULD do under
//! `dry_run`), and an inherent `impl SessionManager` block adding
//! [`SessionManager::decommission_all_ephemeral`] and the general
//! [`SessionManager::prune_managed`].
//! Test: `prune_*` in `super::tests`.

use std::fmt;

use chrono::{Duration, Utc};
use tracing::{error, info, warn};

use super::driver::ManagedTmuxDriver;
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// Maximum age an EPHEMERAL session may reach before the auto-reaper tears it
/// down — default 24 hours (#1508).
///
/// Why: a panicking or abandoned e2e test can leave an ephemeral session behind
/// even though the harness Drop-guard normally cleans it up; without a backstop
/// these would accumulate exactly like the 239 legacy records this feature exists
/// to prevent. 24h is long enough that no legitimate short-lived test session is
/// ever caught, yet short enough that leaks are reclaimed within a day. ONLY
/// `ephemeral == true` records are ever in scope — real sessions default `false`
/// and are unreachable by this path, so the threshold can be aggressive without
/// risking real work.
/// What: a `chrono::Duration` of 24 hours; the reaper decommissions any
/// `ephemeral` session whose `created_at` is older than `now - MAX_EPHEMERAL_AGE`.
/// Test: `reap_aged_ephemeral_picks_old_ephemeral_only` drives both the "too
/// young" and "non-ephemeral" exclusions against this threshold.
pub const MAX_EPHEMERAL_AGE_HOURS: i64 = 24;

/// Consecutive REAL-SWEEP canonicalize failures on the same path before the
/// #1845 F3 fallback escalates from `warn!` to `error!` (#3715 item 3).
///
/// Why: the F3 fallback WARN fired every minute for ~8h on the same path
/// before the underlying vanished-workspace-root issue (#3715) was noticed —
/// a per-tick WARN buried in a large log carries no signal that distinguishes
/// "just started" from "sustained for hours". 10 is deliberately a count of
/// consecutive OBSERVATIONS, not a wall-clock duration: `prune_orphaned_worktrees`
/// (real, non-dry-run, deletion-capable) has THREE call sites —
/// `orphan_gc_loop`'s periodic ~60s tick (`daemon/mod.rs`, spawned at line
/// 132, via `reap_orphaned_worktrees`), the `prune_worktrees` MCP tool
/// (`daemon/mcp_context.rs:182`, always real), and the
/// `POST /sessions/managed/prune-worktrees` HTTP route
/// (`daemon/managed_routes/prune.rs:111`, real whenever `dry_run` is
/// `false`) — so an operator-triggered manual sweep or MCP call between
/// periodic ticks advances the SAME streak. In the common case (only the
/// periodic loop running) 10 observations is roughly 10 minutes; under
/// interleaved manual sweeps it escalates sooner. Either way the count is
/// still meaningful as "sustained past a single transient blip" — the
/// interleaving only makes detection faster, never slower or wrong.
/// What: the threshold [`CanonicalizeFailureStreaks::record_failure`] compares
/// its return value against, to decide `warn!` vs `error!`.
/// Test: `canonicalize_streak_escalates_at_threshold`.
const CANONICALIZE_FAILURE_STREAK_THRESHOLD: u32 = 10;

/// In-memory, per-path consecutive-failure counter for the #1845 F3
/// canonicalize fallback (#3715 item 3).
///
/// Why: a lone per-tick WARN gives no sense of DURATION — the F3 fallback for
/// the path behind #3715 fired unnoticed for ~8h because every occurrence
/// looked identical in the log. Tracking a streak lets the sweep escalate to
/// `error!` once failure has been sustained past
/// [`CANONICALIZE_FAILURE_STREAK_THRESHOLD`] consecutive REAL-sweep
/// observations (see that constant's doc for why this is observation-count,
/// not wall-clock), making it greppable and a future alerting target,
/// without adding persistence or an external alerting pipeline —
/// deliberately kept as simple in-process state (reset on daemon restart,
/// which is acceptable: a restart re-establishes a clean baseline for the
/// same underlying condition to re-accumulate if it is still present).
/// Entries are evicted once their path leaves the sweep's active-session set
/// (`retain_active`, called every real sweep — #3715 finding-2 follow-up)
/// so a decommissioned/deleted/moved session's streak does not linger
/// forever.
/// What: `record_failure` increments (or starts at 1) the counter for `path`
/// and returns the new streak length; `record_success` clears any existing
/// entry for `path` (a single successful canonicalize breaks the streak);
/// `retain_active` drops every tracked path NOT in the caller-supplied active
/// set.
/// Test: `canonicalize_streak_escalates_at_threshold`,
/// `canonicalize_streak_resets_on_success`,
/// `canonicalize_streak_evicts_paths_no_longer_active`.
#[derive(Debug, Default)]
struct CanonicalizeFailureStreaks {
    counts: std::collections::HashMap<std::path::PathBuf, u32>,
}

impl CanonicalizeFailureStreaks {
    /// Record one more consecutive failure for `path`, returning the new streak length.
    fn record_failure(&mut self, path: &std::path::Path) -> u32 {
        let count = self.counts.entry(path.to_path_buf()).or_insert(0);
        *count += 1;
        *count
    }

    /// Record a success for `path`, resetting (removing) any existing streak.
    fn record_success(&mut self, path: &std::path::Path) {
        self.counts.remove(path);
    }

    /// Evict every tracked path NOT present in `active` (#3715 finding 2).
    ///
    /// Why: without this, a path whose session is decommissioned/deleted, or
    /// whose `workspace_path` simply changes, leaves a permanent orphaned
    /// entry in `counts` — unbounded growth over the daemon's lifetime.
    /// What: called once per real sweep with the set of `workspace_path`s
    /// actually observed THIS sweep; removes any tracked key absent from it.
    fn retain_active(&mut self, active: &std::collections::HashSet<std::path::PathBuf>) {
        self.counts.retain(|path, _| active.contains(path));
    }
}

/// Process-global streak state backing [`CanonicalizeFailureStreaks`] (#3715
/// item 3).
///
/// Why: exactly one [`CanonicalizeFailureStreaks`] instance should back all
/// three real-sweep call sites (see [`CANONICALIZE_FAILURE_STREAK_THRESHOLD`]'s
/// doc for why there are three, not one) so a streak observed via the MCP
/// tool or the HTTP route counts toward the same escalation as the periodic
/// loop — process-global state achieves that without adding a field (and
/// constructor-init site) to the shared `SessionManager` struct in
/// `manager.rs`, keeping this change confined to `prune.rs`. Deliberately
/// unpersisted — see the type's own doc.
/// What: lazily-initialized `Mutex`-guarded counter map, accessed only via
/// [`canonicalize_failure_streaks`].
/// Test: covered indirectly by `canonicalize_streak_escalates_at_threshold`,
/// `canonicalize_streak_resets_on_success`, and
/// `canonicalize_streak_evicts_paths_no_longer_active`, which exercise
/// [`CanonicalizeFailureStreaks`] directly (no global state involved) to stay
/// deterministic and independent of test execution order.
static CANONICALIZE_FAILURE_STREAKS: std::sync::OnceLock<
    std::sync::Mutex<CanonicalizeFailureStreaks>,
> = std::sync::OnceLock::new();

/// Accessor for the process-global [`CanonicalizeFailureStreaks`] instance.
fn canonicalize_failure_streaks() -> &'static std::sync::Mutex<CanonicalizeFailureStreaks> {
    CANONICALIZE_FAILURE_STREAKS
        .get_or_init(|| std::sync::Mutex::new(CanonicalizeFailureStreaks::default()))
}

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
/// What: [`Decommissioned`](PruneAction::Decommissioned) — killed runtime +
/// removed workspace + tombstoned; [`Removed`](PruneAction::Removed) — an existing
/// `Decommissioned` tombstone was deleted from the store (compaction).
/// Test: asserted by `decommission_all_ephemeral_ignores_non_ephemeral` (the
/// `Decommissioned` action) and `prune_decommissioned_compacts` (the `Removed`
/// action).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneAction {
    /// The session was torn down (runtime killed, workspace removed, tombstoned).
    Decommissioned,
    /// An existing tombstone was deleted from the store (compaction).
    Removed,
}

impl PruneAction {
    /// The canonical lowercase name of this action (for wire/log rendering).
    ///
    /// Why: HTTP/MCP responses and the CLI dry-run render the action per row.
    /// What: `Decommissioned` → `"decommissioned"`, `Removed` → `"removed"`.
    /// Test: `prune_outcome_serializes`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Decommissioned => "decommissioned",
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
/// Test: `prune_dry_run_reports_without_mutating`.
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

/// Whether a record is currently RUNNING (must not be auto-torn-down) — a REAL
/// liveness probe, not a persisted-state check (#2022).
///
/// Why: the core #1508 safety invariant is "never kill a running session
/// unless the operator explicitly forces it" — but the ORIGINAL implementation
/// answered that question by reading the persisted `state` field
/// (`Active`/`Provisioning`), which is a snapshot that can go stale. If the
/// tmux session backing a record dies (crash, `tmux kill-server`, host
/// restart, manual `tmux kill-session`) without the daemon observing it, the
/// record keeps saying `Active` forever, and the fail-closed guard then
/// refuses `delete`/`prune`/`decommission` on a session that is, in fact,
/// already dead — forcing the operator to `--force` past a guard that no
/// longer protects anything real (#2022). Probing the actual tmux backing
/// makes the guard track reality instead of a snapshot that can drift from
/// it. There is no persisted PID to additionally check (the `claude` PID is
/// discovered on demand by scanning the tmux session's process tree — see
/// `crate::core::process::find_claude_pid_in_tmux` — never stored on the
/// record), so the tmux probe alone IS the real liveness signal; a live pane
/// implies a live (or at least still-open) process tree underneath it.
/// What: returns whether a tmux session named `record.tmux_name` currently
/// exists, via [`ManagedTmuxDriver::session_exists`]. A dead/absent tmux
/// session is NOT running regardless of the persisted `state` — so a stale
/// `Active` record is deletable/prunable/decommissionable without `--force`.
/// A live tmux session IS running regardless of `state` — so the guard still
/// refuses an unforced delete/prune/decommission of a genuinely active
/// session (and, as a bonus, closes the reverse gap noted in #2022 where a
/// stale non-running `state` could under-guard a session whose tmux is
/// somehow still alive).
/// Test: `prune_by_state_never_touches_active` (live session still guarded),
/// `delete_record_refuses_running_without_force` (live session still
/// guarded), `delete_record_stale_active_deletable_when_tmux_dead` (#2022 —
/// stale `Active` with a dead tmux is deletable without `--force`),
/// `prune_stale_active_removable_without_force_*` (#2022 — same for
/// `prune_managed`).
pub(super) fn is_running(record: &SessionRecord, tmux: &dyn ManagedTmuxDriver) -> bool {
    tmux.session_exists(&record.tmux_name)
}

/// Whether a record matches a prune `filter` (ignoring the running-state guard).
///
/// Why: keeping the match logic separate from the running-state safety gate keeps
/// each rule readable and individually testable.
/// What: `Ephemeral` → `ephemeral && state != Decommissioned`; `Stopped` →
/// `state == Stopped`; `Decommissioned` → `state == Decommissioned`; `All` → any
/// non-running record (the caller still applies [`is_running`] as the final gate).
/// Test: the per-filter `prune_*` tests.
fn matches_filter(record: &SessionRecord, filter: PruneFilter) -> bool {
    match filter {
        PruneFilter::Ephemeral => {
            record.ephemeral && record.state != ManagedSessionState::Decommissioned
        }
        PruneFilter::Stopped => record.state == ManagedSessionState::Stopped,
        PruneFilter::Decommissioned => record.state == ManagedSessionState::Decommissioned,
        PruneFilter::Deleted => record.state == ManagedSessionState::Deleted,
        PruneFilter::All => true,
    }
}

/// Enumerate orphaned per-session worktree directories under `repos_root` (#1840).
///
/// Why: extracted from `SessionManager::prune_orphaned_worktrees` so the
/// walk logic can be tested independently of the full session-manager setup,
/// and reused by the `doctor.rs` worktree health probe without duplicating the
/// filesystem walk.
/// What: walks BOTH known worktree-store shapes (#3649) under each
/// `<repos_root>/<owner>/<repo>/`: the in-project shape
/// (`.worktrees/<name>`, added #1840) AND the clone-based shared-base-checkout
/// shape (`.base/.worktrees/<session-id>`, added #3649 — this walk previously
/// covered ONLY the in-project shape, so every `.base/.worktrees` dir was
/// invisible to both this scan and the doctor/dry-run surfaces built on it).
/// Any leaf directory whose canonicalized path is NOT in `active_set` is
/// collected as an orphan. Using a `HashSet` with canonicalized paths avoids
/// O(n×m) linear scan and correctly handles symlinked workspace paths. A
/// non-existent or unreadable `repos_root` returns an empty vec.
/// Test: `prune_orphaned_worktrees_spares_active`,
///       `prune_orphaned_worktrees_removes_orphan`,
///       `find_orphaned_worktrees_covers_base_worktrees_shape` (#3649).
pub(crate) fn find_orphaned_worktrees(
    repos_root: &std::path::Path,
    active_set: &std::collections::HashSet<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut orphans = Vec::new();
    let Ok(owner_entries) = std::fs::read_dir(repos_root) else {
        return orphans;
    };
    for owner_entry in owner_entries.flatten() {
        let owner_path = owner_entry.path();
        if !owner_path.is_dir() {
            continue;
        }
        let Ok(repo_entries) = std::fs::read_dir(&owner_path) else {
            continue;
        };
        for repo_entry in repo_entries.flatten() {
            let repo_path = repo_entry.path();
            // Shape 1 (#1840): in-project worktrees at `<repo>/.worktrees/<name>`.
            scan_worktree_shape(&repo_path.join(".worktrees"), active_set, &mut orphans);
            // Shape 2 (#3649): clone-based shared-base-checkout worktrees at
            // `<repo>/.base/.worktrees/<session-id>` (see
            // `provisioner::workspace::WorkspaceProvisioner::provision_in`).
            scan_worktree_shape(
                &repo_path.join(".base").join(".worktrees"),
                active_set,
                &mut orphans,
            );
        }
    }
    orphans
}

/// Scan one `.worktrees`-shaped directory for leaf dirs not in `active_set`,
/// appending any found to `orphans` (#3649 extraction — shared by both
/// worktree-store shapes [`find_orphaned_worktrees`] walks).
///
/// Why: `find_orphaned_worktrees` originally inlined this loop for the single
/// in-project shape it knew about; #3649 adds a second shape
/// (`.base/.worktrees`) that must apply the IDENTICAL leaf-dir/canonicalize/
/// active-set logic, so the loop body is extracted rather than duplicated.
/// What: no-ops if `wt_dir` is not a directory; otherwise lists its immediate
/// children, skips non-directories and paths that fail to canonicalize (#1845
/// item 8 — a dangling symlink or deletion race must not be misclassified),
/// and appends every canonicalized-but-not-active leaf to `orphans`.
/// Test: `prune_orphaned_worktrees_collects_orphan`,
///       `find_orphaned_worktrees_covers_base_worktrees_shape` (#3649).
fn scan_worktree_shape(
    wt_dir: &std::path::Path,
    active_set: &std::collections::HashSet<std::path::PathBuf>,
    orphans: &mut Vec<std::path::PathBuf>,
) {
    if !wt_dir.is_dir() {
        return;
    }
    let Ok(wt_entries) = std::fs::read_dir(wt_dir) else {
        return;
    };
    for wt_entry in wt_entries.flatten() {
        let wt_path = wt_entry.path();
        if !wt_path.is_dir() {
            continue;
        }
        // Item 8 (#1845): skip if the path cannot be canonicalized.
        // A dangling symlink or a deletion race makes the path
        // unresolvable; we cannot safely compare it against the active
        // set, so we leave it untouched rather than risk misclassifying
        // a live-but-partially-deleted worktree as an orphan.
        let canonical_wt = match std::fs::canonicalize(&wt_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !active_set.contains(&canonical_wt) {
            orphans.push(wt_path);
        }
    }
}

/// Outcome of an orphaned-worktree sweep (#3649): which candidates were (or
/// would be, under `dry_run`) removed vs. skipped because ownership could not
/// be established.
///
/// Why: the #3649 safe default — an owner-unknown worktree is NEVER
/// auto-deleted — must not just silently vanish from the caller's view; the
/// daemon's orphan-GC log line and `tm session prune-worktrees` both need to
/// see this count so operators know legacy worktrees are being conservatively
/// left in place for `tm doctor` / `--dry-run` review, not merely "not found".
/// What: `removed` — paths actually removed (or that WOULD be removed under
/// `dry_run`); `owner_unknown` — paths whose ownership sentinel had no
/// resolvable owner (absent, empty/legacy, or unparsable content).
/// Test: `prune_orphaned_worktrees_skips_owner_unknown`,
///       `prune_orphaned_worktrees_reclaims_terminal_owner`.
#[derive(Debug, Clone, Default)]
pub struct OrphanSweepOutcome {
    /// Paths actually removed (or that would be removed under `dry_run`).
    pub removed: Vec<std::path::PathBuf>,
    /// Paths skipped because their sentinel's owner could not be resolved —
    /// never auto-deleted; surfaced here for `tm doctor`/manual review.
    pub owner_unknown: Vec<std::path::PathBuf>,
}

/// Best-effort cross-check: does `git worktree list` on the checkout owning
/// `candidate` agree that `candidate` is a real, currently-registered git
/// worktree (#3649)?
///
/// Why: the sentinel + store-ownerless checks establish WHO owned this
/// directory and whether that owner is provably gone, but neither confirms
/// git's OWN bookkeeping still recognises the path as a worktree at all — a
/// belt-and-suspenders safety net against deleting a directory that merely
/// LOOKS like a worktree (e.g. its git worktree entry was already pruned by
/// something else, or the shape matched by coincidence). A disagreement is
/// treated conservatively: skip rather than delete.
/// What: runs `git -C <repo_root> worktree list --porcelain`, where
/// `repo_root` is `candidate`'s grandparent directory — the SAME derivation
/// `decommission::remove_session_worktree` uses, which works identically for
/// both worktree-store shapes (`<repo>/.worktrees/<name>` and
/// `<repo>/.base/.worktrees/<id>`, since either way the grandparent of the
/// worktree leaf is the git checkout root). Returns `true` (agree — deletion
/// may proceed, subject to the caller's other checks) when the git command
/// cannot be run or fails outright — this check is an ADDITIONAL safety net
/// on top of the sentinel/store checks, not a replacement for them, so a
/// missing `git` binary or a transient failure never blocks a deletion those
/// checks already approved. Returns `true` only when `candidate`'s
/// canonicalized path appears among the porcelain output's `worktree <path>`
/// lines.
/// Test: `git_worktree_list_agrees_true_for_real_worktree`,
///       `git_worktree_list_agrees_false_for_untracked_dir`.
fn git_worktree_list_agrees(candidate: &std::path::Path) -> bool {
    let Some(repo_root) = candidate.parent().and_then(|p| p.parent()) else {
        return true;
    };
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output();
    let Ok(out) = out else {
        return true; // best-effort: git unavailable must never block a delete
    };
    if !out.status.success() {
        return true;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let canonical_candidate =
        std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| {
            let pb = std::path::PathBuf::from(p);
            std::fs::canonicalize(&pb).unwrap_or(pb) == canonical_candidate
        })
}

impl SessionManager {
    /// Tear down EVERY ephemeral session — bulk teardown (#1508).
    ///
    /// Why: e2e harnesses (and any operator who tagged test sessions) need a
    /// one-shot "clean up all my throwaway sessions" verb that reuses the existing
    /// per-session [`decommission`](SessionManager::decommission) internals (tmux
    /// kill + workspace removal + tombstone). REAL sessions default
    /// `ephemeral == false` and so are unreachable here — the safety invariant.
    /// What: convenience wrapper over [`prune_managed`](Self::prune_managed) with
    /// `PruneFilter::Ephemeral`, not a dry run, never touching running sessions
    /// (a running ephemeral session is decommissioned like any other — `decommission`
    /// itself kills the runtime first). Returns the count of sessions decommissioned.
    /// Test: `decommission_all_ephemeral_ignores_non_ephemeral` (asserts the
    /// ephemeral-only decommission path and the non-ephemeral safety exclusion).
    pub async fn decommission_all_ephemeral(&self) -> Result<usize, ManagedError> {
        // Ephemeral sessions are throwaway by definition: include running ones so a
        // panicking test that left an Active ephemeral session is still cleaned up.
        // `caller: None` — this is an operator/daemon-internal bulk sweep, never a
        // session acting on its own behalf; the #3649 owner gate does not apply.
        let outcome = self
            .prune_managed(PruneFilter::Ephemeral, false, true, None)
            .await?;
        Ok(outcome.count())
    }

    /// Prune managed sessions by state — bulk teardown + compaction (#1508).
    ///
    /// Why: ONE tool must (a) tear down all ephemeral/stopped sessions and (b)
    /// compact the store by dropping decommissioned tombstones, so the legacy 239
    /// stale records can be purged with the SAME verb that cleans up test sessions.
    /// It is the engine behind `decommission_all_ephemeral`, the `tm session prune`
    /// CLI verb, the prune HTTP route, and the prune MCP tool.
    ///
    /// SAFETY (the #1508 invariant): a RUNNING record (`Active`/`Provisioning`) is
    /// NEVER killed or removed unless `include_active` is explicitly `true`. The
    /// `Decommissioned` filter only ever removes tombstones (it cannot reach a
    /// running record by construction).
    ///
    /// What: snapshots `store.all()`, selects records that both
    /// [`matches_filter`] AND pass the running-state gate (unless `include_active`),
    /// then per record —
    /// - a non-`Decommissioned` match → [`decommission`](Self::decommission) it
    ///   (kill runtime + remove workspace + tombstone) → [`PruneAction::Decommissioned`];
    /// - a `Decommissioned` match → [`SessionStore::remove`] it from the store
    ///   (compaction) → [`PruneAction::Removed`].
    ///
    /// When `dry_run` is true NOTHING is mutated — the returned [`PruneOutcome`]
    /// lists what WOULD happen. A per-session failure is logged and skipped (the
    /// sweep is best-effort, like the orphan-GC) so one stuck session cannot block
    /// the rest of a legacy purge.
    /// Test: `decommission_all_ephemeral_ignores_non_ephemeral` (ephemeral path),
    /// `prune_by_state_never_touches_active` (Stopped→Decommissioned + the
    /// running-state safety gate), `prune_decommissioned_compacts` (compaction),
    /// `prune_all_targets_non_running`, `prune_dry_run_reports_without_mutating`.
    ///
    /// `caller` (#3649, Option B): threaded straight through to
    /// [`decommission`](Self::decommission) for each non-tombstone target —
    /// `None` (every current call site: CLI, HTTP route, MCP tool,
    /// `decommission_all_ephemeral`) preserves full pre-#3649 authority.
    /// `Some(id)` applies the owner gate per-target, so a fleet-wide prune
    /// invoked on behalf of a specific session still cannot reclaim a peer
    /// session's actively-owned worktree.
    pub async fn prune_managed(
        &self,
        filter: PruneFilter,
        dry_run: bool,
        include_active: bool,
        caller: Option<ManagedSessionId>,
    ) -> Result<PruneOutcome, ManagedError> {
        // Snapshot the full set ONCE (reloads-on-read so out-of-process writes are
        // seen). We then mutate per record below, each of which re-reads/saves.
        //
        // Pick up any out-of-process write under a brief write lock (the reload is
        // `&mut`), then drop it and take the read-only snapshot under a READ lock so
        // the (in-memory) `cached_all()` clone never blocks concurrent store readers
        // (#1508 review fix). `cached_all()` is `&self` and does no I/O.
        self.store.write().await.reload_if_changed().await?;
        let all = self.store.read().await.cached_all();

        // Select the in-scope records, applying the running safety gate — a REAL
        // tmux liveness probe (#2022), not the persisted `state` field.
        let tmux = self.tmux.as_ref();
        let targets: Vec<SessionRecord> = all
            .into_iter()
            .filter(|r| matches_filter(r, filter))
            .filter(|r| include_active || !is_running(r, tmux))
            .collect();

        let mut sessions = Vec::with_capacity(targets.len());
        for record in targets {
            // Both `Decommissioned` and `Deleted` (`--deleted--`) are terminal
            // tombstones: prune COMPACTS (removes) them rather than re-running a
            // decommission teardown.
            let is_tombstone = matches!(
                record.state,
                ManagedSessionState::Decommissioned | ManagedSessionState::Deleted
            );
            let action = if is_tombstone {
                PruneAction::Removed
            } else {
                PruneAction::Decommissioned
            };

            if !dry_run {
                if is_tombstone {
                    // Compaction: drop the tombstone from the store entirely.
                    if let Err(e) = self.store.write().await.remove(&record.id).await {
                        warn!(id = %record.id, "prune: compaction remove failed: {e}; skipping");
                        continue;
                    }
                } else if let Err(e) = self.decommission(&record.id, caller).await {
                    warn!(id = %record.id, "prune: decommission failed: {e}; skipping");
                    continue;
                }
            }

            sessions.push(PrunedSession {
                id: record.id.to_string(),
                tmux_name: record.tmux_name.clone(),
                state: record.state.to_string(),
                action,
            });
        }

        if dry_run {
            info!(
                filter = %filter,
                count = sessions.len(),
                "prune (dry-run): reporting candidates, no mutation"
            );
        } else {
            info!(
                filter = %filter,
                count = sessions.len(),
                "prune: applied teardown/compaction"
            );
        }

        Ok(PruneOutcome {
            dry_run,
            filter: filter.as_str().to_string(),
            sessions,
        })
    }

    /// Remove a single decommissioned tombstone from the store (#1508).
    ///
    /// Why: the age-based reaper and any caller that has just decommissioned a
    /// record may want it gone from `sessions.json` rather than left as a tombstone,
    /// so the file stops growing unbounded. This is the single-record compaction
    /// primitive [`prune_managed`](Self::prune_managed) uses for the
    /// `Decommissioned` filter, exposed for direct callers.
    /// What: removes the record keyed by `id` via [`SessionStore::remove`] and
    /// persists. A not-present id is a no-op warning inside `remove`.
    /// Test: `compact_record_removes_from_store`.
    pub async fn compact_record(&self, id: &ManagedSessionId) -> Result<(), ManagedError> {
        self.store.write().await.remove(id).await?;
        Ok(())
    }

    /// Remove orphaned per-session git worktrees from the managed workspace root (#1840).
    ///
    /// Why: `decommission` now calls `git worktree remove --force` for in-project
    /// worktrees, but sessions decommissioned before the fix — or where the git
    /// command failed — leave stale `.worktrees/<session-id>/` directories. This
    /// sweep removes them without touching any directory that still corresponds to a
    /// live session (i.e. whose path appears in `active_workspace_paths`).
    ///
    /// SAFETY: only directories whose full canonicalized path is NOT in the active
    /// set are removed. Active session worktrees are NEVER touched. Paths are
    /// canonicalized to handle symlinks correctly (Fix 1b, #1840).
    ///
    /// TOCTOU safety (#1840, hardened #1845 item 9): the sweep runs in two phases.
    /// Phase 1 discovers orphan candidates using the caller-supplied
    /// `active_workspace_paths` snapshot (which may have been taken moments before
    /// this call). Phase 2 — the real deletion path — takes ONE fresh snapshot from
    /// the live session store immediately before the deletion loop (O(1) lock
    /// acquisitions vs. the prior O(n) per-candidate approach). The snapshot is
    /// taken as late as possible — just before the first deletion — to minimise the
    /// residual window. **Residual TOCTOU window:** a session registered AFTER the
    /// Phase 2 snapshot but BEFORE a candidate's deletion is NOT seen by the snapshot
    /// and could theoretically be deleted. This window is sub-millisecond in practice
    /// (the snapshot is taken after all I/O-bound Phase 1 work), making it
    /// substantially narrower than the per-candidate approach (which had an O(n)
    /// window). Treat this as narrowing the window to near-zero, not eliminating it.
    /// Dry-run returns after Phase 1 (no deletion, no snapshot).
    ///
    /// What: Phase 1 calls [`find_orphaned_worktrees`] inside `spawn_blocking`
    /// (the filesystem walk is blocking); panics are propagated as `Err`. Phase 2
    /// (real-delete only) takes ONE fresh `self.store` snapshot, then per candidate:
    /// canonicalize (skip on error — item 8), check against snapshot, apply the
    /// #3649 OWNERSHIP GATE (below), then call `remove_session_worktree` in its
    /// own `spawn_blocking`. Returns an [`OrphanSweepOutcome`] rather than a bare
    /// path list (#3649) so a caller can see BOTH what was (or would be) removed
    /// AND what was conservatively skipped for owner-unknown review.
    ///
    /// #3649 OWNERSHIP GATE (applied to every candidate, including under
    /// `dry_run` — so a preview matches what a real run would do): read the
    /// candidate's ownership sentinel via
    /// [`super::worktree_ownership::read_sentinel_owner`].
    /// - Owner UNKNOWN (legacy zero-byte sentinel, absent sentinel, or
    ///   unparsable content) → NEVER delete; counted in
    ///   [`OrphanSweepOutcome::owner_unknown`] so it keeps surfacing via
    ///   `tm doctor` / `--dry-run` until a human acts (zero-migration, ADR-0020).
    /// - Owner KNOWN → delete only if
    ///   [`SessionManager::resolve_ownerless_with_grace`] says the owner is
    ///   provably ownerless (a resolvable record in a terminal state, OR no
    ///   resolvable record AND the sentinel is older than
    ///   [`super::worktree_ownership::OWNERLESS_GRACE`] — see that constant's
    ///   doc for why an absent-but-YOUNG owner is a creation race, not a
    ///   deletion) AND `git worktree list` on the owning checkout agrees the
    ///   path is a real worktree ([`git_worktree_list_agrees`]) — a
    ///   disagreement is skipped conservatively, never deleted.
    ///
    /// Test: `prune_orphaned_worktrees_removes_orphan`,
    /// `prune_orphaned_worktrees_spares_active`,
    /// `prune_orphaned_worktrees_store_snapshot_blocks_deletion` (item 1),
    /// `prune_orphaned_worktrees_skips_owner_unknown`,
    /// `prune_orphaned_worktrees_reclaims_terminal_owner`,
    /// `prune_orphaned_worktrees_spares_live_owner`,
    /// `prune_orphaned_worktrees_spares_recent_unregistered_owner` (#3649).
    pub async fn prune_orphaned_worktrees(
        &self,
        repos_root: &std::path::Path,
        active_workspace_paths: &[std::path::PathBuf],
        dry_run: bool,
    ) -> Result<OrphanSweepOutcome, anyhow::Error> {
        use super::decommission::remove_session_worktree;
        use super::worktree_ownership::SentinelOwner;
        use std::collections::HashSet;

        let repos_root = repos_root.to_path_buf();
        // Build a canonicalized set for O(1) lookup and symlink safety.
        let initial_active: HashSet<std::path::PathBuf> = active_workspace_paths
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();

        // Phase 1: discover orphan candidates using the initial snapshot.
        // Propagate a spawn_blocking panic as Err (#1845 item 7) rather than
        // silently returning an empty candidate list.
        let candidates = tokio::task::spawn_blocking({
            let initial_active = initial_active.clone();
            move || find_orphaned_worktrees(&repos_root, &initial_active)
        })
        .await
        .map_err(|e| anyhow::anyhow!("prune-worktrees: orphan scan panicked: {e}"))?;

        // Phase 1.5 (#3649): classify every candidate by ownership BEFORE any
        // deletion decision — applied identically under dry-run and real runs
        // so a preview reflects reality.
        let mut owner_unknown = Vec::new();
        let mut reclaimable = Vec::new();
        for candidate in candidates {
            match super::worktree_ownership::read_sentinel_owner(&candidate) {
                SentinelOwner::Unknown => {
                    info!(
                        path = %candidate.display(),
                        "prune-worktrees: owner-unknown sentinel — never auto-deleting; \
                         run `tm doctor` or inspect manually (#3649)"
                    );
                    owner_unknown.push(candidate);
                }
                SentinelOwner::Known(owner, created_at) => {
                    if !self.resolve_ownerless_with_grace(owner, created_at).await {
                        info!(
                            path = %candidate.display(),
                            owner = %owner,
                            "prune-worktrees: owner is still live/resumable, or the sentinel \
                             is too young to rule out a creation race — skipping (#3649)"
                        );
                        continue;
                    }
                    if !git_worktree_list_agrees(&candidate) {
                        warn!(
                            path = %candidate.display(),
                            "prune-worktrees: git worktree list disagrees this path is a \
                             worktree — skipping conservatively (#3649)"
                        );
                        continue;
                    }
                    reclaimable.push(candidate);
                }
            }
        }

        if dry_run {
            for p in &reclaimable {
                info!(path = %p.display(), "prune-worktrees (dry-run): would remove orphaned worktree");
            }
            return Ok(OrphanSweepOutcome {
                removed: reclaimable,
                owner_unknown,
            });
        }

        // Phase 2 (real-delete path): ONE fresh snapshot immediately before the
        // deletion loop (#1845 item 9). Each active path is inserted in BOTH its
        // canonicalized form (for symlink-safe comparison) and its raw form
        // (Finding 3 #1845: if canonicalize fails on the active side, keep the
        // raw path as a protective fallback so a canonicalize failure can never
        // cause an active worktree to be misidentified as an orphan and deleted).
        let fresh_active: HashSet<std::path::PathBuf> = {
            let mut set = HashSet::new();
            // Raw `workspace_path`s actually observed THIS sweep, used below to
            // evict stale streak entries (#3715 finding 2) — kept separate from
            // `set` because `set` also accumulates canonicalized forms, which are
            // not the keys `CanonicalizeFailureStreaks` tracks.
            let mut checked_paths: HashSet<std::path::PathBuf> = HashSet::new();
            for r in self.store.read().await.cached_all() {
                let session_id = r.id;
                let Some(p) = r.workspace_path else {
                    continue;
                };
                checked_paths.insert(p.clone());
                if let Ok(c) = std::fs::canonicalize(&p) {
                    set.insert(c);
                    // Success breaks any in-flight failure streak (#3715 item 3).
                    canonicalize_failure_streaks()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record_success(&p);
                } else {
                    let streak = canonicalize_failure_streaks()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record_failure(&p);
                    if streak >= CANONICALIZE_FAILURE_STREAK_THRESHOLD {
                        error!(
                            session = %session_id,
                            path = %p.display(),
                            streak,
                            "prune-worktrees: active session path has failed to \
                             canonicalize for {streak} consecutive real-sweep \
                             observations — sustained failure, investigate before \
                             a stop/reap silently reconstitutes this workspace \
                             root (#3715)"
                        );
                    } else {
                        warn!(
                            path = %p.display(),
                            "prune-worktrees: active session path failed to canonicalize; \
                             using raw path as protective fallback (#1845 F3)"
                        );
                    }
                }
                // Always insert the raw path so the raw-form check below catches
                // cases where the active side failed to canonicalize.
                set.insert(p);
            }
            // #3715 finding 2: evict any tracked streak whose path is no longer
            // among this sweep's active sessions (decommissioned, deleted, or
            // workspace_path changed) so the counter map cannot grow unbounded
            // across the daemon's lifetime.
            canonicalize_failure_streaks()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain_active(&checked_paths);
            set
        };

        let mut removed = Vec::new();
        for candidate in reclaimable {
            // Item 8 (#1845): skip on canonicalize failure — a path that can't be
            // resolved is left untouched rather than risk incorrect deletion.
            let canonical_candidate = match std::fs::canonicalize(&candidate) {
                Ok(c) => c,
                Err(_) => {
                    warn!(
                        path = %candidate.display(),
                        "prune-worktrees: skipping candidate — canonicalize failed"
                    );
                    continue;
                }
            };
            // Check both the canonicalized form (symlink-safe) AND the raw form
            // (Finding 3 #1845: protects against active-path canonicalize failures
            // — if the active side couldn't be canonicalized, its raw path is in
            // the set and a raw-path match prevents accidental deletion).
            if fresh_active.contains(&canonical_candidate) || fresh_active.contains(&candidate) {
                info!(
                    path = %candidate.display(),
                    "prune-worktrees: skipping — active session appeared after initial snapshot"
                );
                continue;
            }

            info!(path = %candidate.display(), "prune-worktrees: removing orphaned worktree");
            let candidate_clone = candidate.clone();
            let removed_ok =
                tokio::task::spawn_blocking(move || remove_session_worktree(&candidate_clone))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!(
                            "prune-worktrees: spawn_blocking panicked during removal: {e}"
                        );
                        false
                    });
            if removed_ok {
                removed.push(candidate);
            }
        }
        Ok(OrphanSweepOutcome {
            removed,
            owner_unknown,
        })
    }

    /// Auto-reap orphaned per-session worktree dirs using the manager's own live
    /// record set (#1838).
    ///
    /// Why: [`prune_orphaned_worktrees`](Self::prune_orphaned_worktrees) is only
    /// invoked manually (the `tm session prune-worktrees` CLI / HTTP route), so
    /// the managed-clone `.worktrees/<id>` tree still grows without bound — one
    /// project accumulated 94 dead worktree dirs because nothing ran the sweep
    /// automatically. This thin convenience wrapper lets the daemon's orphan-GC
    /// loop reclaim orphaned worktree dirs on the SAME cadence it reaps orphaned
    /// tmux sessions, without each caller re-assembling the active-path set.
    /// What: snapshots every live record's `workspace_path` from the store as the
    /// active set, then delegates to `prune_orphaned_worktrees` with
    /// `dry_run = false`. The two-phase TOCTOU safety (a fresh store snapshot taken
    /// immediately before deletion) and the "only leaf dirs under `.worktrees/`,
    /// never the base clone" guard are inherited unchanged. Returns the paths removed.
    /// Test: `reap_orphaned_worktrees_removes_orphan_preserves_live` in
    /// `super::reap_orphaned_worktrees_tests`.
    pub async fn reap_orphaned_worktrees(
        &self,
        repos_root: &std::path::Path,
    ) -> Result<OrphanSweepOutcome, anyhow::Error> {
        let active: Vec<std::path::PathBuf> = self
            .list()
            .await
            .into_iter()
            .filter_map(|r| r.workspace_path)
            .collect();
        self.prune_orphaned_worktrees(repos_root, &active, false)
            .await
    }

    /// Age-based auto-reap of stale ephemeral sessions (#1508).
    ///
    /// Why: a panicking or abandoned e2e test can leave an ephemeral session behind
    /// despite the harness Drop-guard. Without a backstop these leak exactly like
    /// the 239 legacy records this feature prevents. The orphan-GC loop calls this
    /// each sweep so leaked test sessions are reclaimed within
    /// [`MAX_EPHEMERAL_AGE_HOURS`].
    ///
    /// SAFETY: only `ephemeral == true` records are EVER in scope — real sessions
    /// default `false` and are unreachable by this path. State is irrelevant
    /// (a stuck Active ephemeral session past the age cutoff is reaped too, since
    /// `decommission` kills the runtime first).
    /// What: takes `max_age` as a parameter (for deterministic tests), snapshots the
    /// store, selects `ephemeral && created_at < now - max_age`, and decommissions
    /// each (best-effort; a per-session failure is logged and skipped). Returns the
    /// count reaped and logs the tmux names so the sweep is visible in daemon logs.
    /// Test: `reap_aged_ephemeral_picks_old_ephemeral_only`.
    pub async fn reap_aged_ephemeral(&self, max_age: Duration) -> Result<usize, ManagedError> {
        let cutoff = Utc::now() - max_age;
        // Reload under a brief write lock (the reload is `&mut`), then snapshot under
        // a READ lock via the in-memory, I/O-free `cached_all()` so the snapshot
        // clone never blocks concurrent store readers (#1508 review fix).
        self.store.write().await.reload_if_changed().await?;
        let all = self.store.read().await.cached_all();
        let stale: Vec<SessionRecord> = all
            .into_iter()
            .filter(|r| {
                r.ephemeral
                    && r.state != ManagedSessionState::Decommissioned
                    && r.created_at < cutoff
            })
            .collect();

        let mut reaped = 0usize;
        for record in stale {
            match self.decommission(&record.id, None).await {
                Ok(_) => {
                    reaped += 1;
                    info!(
                        id = %record.id,
                        name = %record.tmux_name,
                        created_at = %record.created_at.to_rfc3339(),
                        "auto-reap: decommissioned stale ephemeral session"
                    );
                }
                Err(e) => {
                    warn!(id = %record.id, "auto-reap: decommission failed: {e}; skipping");
                }
            }
        }
        Ok(reaped)
    }
}

#[cfg(test)]
#[path = "prune_orphan_tests.rs"]
mod orphan_tests;
