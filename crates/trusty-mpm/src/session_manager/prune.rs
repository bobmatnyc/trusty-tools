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
//! What: an inherent `impl SessionManager` block adding
//! [`SessionManager::decommission_all_ephemeral`] and the general
//! [`SessionManager::prune_managed`]. The result and filter types those return —
//! [`PruneFilter`], [`PruneAction`], [`PrunedSession`], [`PruneOutcome`] — live in
//! `prune_types.rs` and are re-exported here, so `prune::PruneFilter` still
//! resolves.
//! Test: `prune_*` in `super::tests`.

use chrono::{Duration, Utc};
use tracing::{error, info, warn};

use super::driver::ManagedTmuxDriver;
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::worktree_safety::{
    DirtyWorktree, DirtyWorktreePolicy, dirt_blocks_removal, git_worktree_list_agrees, inspect_dirt,
};

#[path = "prune_types.rs"]
mod types;

// Re-exported so `prune::PruneFilter` and friends keep resolving after the
// #5912 split — no call site moved.
pub use types::{PruneAction, PruneFilter, PruneOutcome, PrunedSession};

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
///
/// #5859: an UNDETERMINABLE probe is not an absent session. `Err` propagates to
/// the caller, which refuses the teardown, rather than folding into `false` and
/// letting a live pane be pruned without `--force`.
/// Test: `prune_by_state_never_touches_active` (live session still guarded),
/// `delete_record_refuses_running_without_force` (live session still
/// guarded), `delete_record_stale_active_deletable_when_tmux_dead` (#2022 —
/// stale `Active` with a dead tmux is deletable without `--force`),
/// `prune_stale_active_removable_without_force_*` (#2022 — same for
/// `prune_managed`), `delete_record_refuses_when_the_tmux_probe_fails` and
/// `prune_refuses_when_the_tmux_probe_fails` (#5859 — the error arm).
pub(super) fn is_running(
    record: &SessionRecord,
    tmux: &dyn ManagedTmuxDriver,
) -> Result<bool, ManagedError> {
    tmux.session_exists_checked(&record.tmux_name)
}

/// Whether a record is a terminal tombstone — the two states prune COMPACTS
/// rather than tearing down again.
///
/// Why: two places ask this question about the same record and must not answer
/// it differently. [`SessionManager::prune_managed`]'s loop asks it of the
/// snapshot to pick an action; [`SessionManager::compact_and_release_slot`] asks
/// it again of the record the store holds at removal time, to decide whether the
/// number is safe to hand back. Two spellings of one predicate drifting apart is
/// the shape of #5897 itself.
/// What: `Decommissioned | Deleted`.
/// Test: `prune_decommissioned_compacts`, `prune_deleted_compacts`,
/// `prune_keeps_the_record_and_slot_of_a_session_reactivated_after_the_snapshot`.
fn is_tombstone(record: &SessionRecord) -> bool {
    matches!(
        record.state,
        ManagedSessionState::Decommissioned | ManagedSessionState::Deleted
    )
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

/// Enumerate orphaned worktree reclaim candidates under `repos_root` (#1840,
/// rebuilt git-native #4207 slice 1).
///
/// Why: this function used to WALK the filesystem, probing five hard-coded
/// location shapes under each `<repos_root>/<owner>/<repo>/`. Every shape was
/// added by a bug report about a location the previous shape list had missed
/// (#3649, #3971, and the #3971 follow-up), and the list could never be
/// complete because nothing stops a worktree from being registered anywhere on
/// disk. Worse, the walk found DIRECTORIES, not worktrees, so it had no idea
/// which checkout owned any of them — the grandparent guess that filled that
/// gap left fourteen worktrees physically inside `.base` but registered to the
/// parent repo permanently unreclaimable (#4207). Git maintains the registry;
/// deriving from it deletes the whole category of missed-location bug.
/// What: delegates discovery to
/// [`super::worktree_registry::enumerate_registered_worktrees`] — every
/// worktree git itself registers inside a managed project, wherever in that
/// project it lives — then removes any whose path is in `active_set`.
/// Candidates come back canonicalized (so the active-set comparison is
/// symlink-safe), sorted, and de-duplicated. A non-existent or unreadable
/// `repos_root` yields an empty vec.
///
/// BOUNDARY (#4224 review, HIGH): "wherever it lives" is bounded — a candidate
/// must be a strict descendant of the managed project directory whose registry
/// named it. Location is irrelevant WITHIN a project (that is the #4207 fix);
/// it is decisive at the project edge, so an operator checkout parked beside a
/// project, or a worktree the operator registered outside it, is never a
/// candidate. See
/// [`super::worktree_registry::enumerate_registered_worktrees`].
///
/// A candidate here is still only a CANDIDATE: `prune_orphaned_worktrees`
/// applies the #3649 ownership-sentinel gate and the #4091 dirty-tree gate
/// before anything is deleted, so a Claude-Code-created worktree (which never
/// carries trusty-mpm's sentinel) lands in `owner_unknown` and is reported,
/// never removed.
///
/// SCOPE NOTE (#4207): a directory that git does NOT register — a husk left
/// behind by a half-finished removal, for instance — is no longer enumerated.
/// It was already unreclaimable before this change, because
/// `git_worktree_list_agrees` refused every such path; the difference is that
/// it is now also absent from the report. Reclaiming unregistered husks is a
/// separate concern (#3715), deliberately not smuggled in here.
/// Test: `prune_orphaned_worktrees_spares_active`,
///       `prune_orphaned_worktrees_removes_orphan`,
///       `find_orphaned_worktrees_discovers_worktree_at_unwalked_location`
///       (#4207 — fails against the five-shape walk),
///       `find_orphaned_worktrees_ignores_plain_directory`.
pub(crate) fn find_orphaned_worktrees(
    repos_root: &std::path::Path,
    active_set: &std::collections::HashSet<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    super::worktree_registry::enumerate_registered_worktrees(repos_root)
        .into_iter()
        .filter(|candidate| !active_set.contains(candidate))
        .collect()
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
/// resolvable owner (absent, empty/legacy, or unparsable content);
/// `skipped_dirty` (#4091) — paths whose owner WAS resolvable and provably
/// gone, but which still hold uncommitted or unpushed work (or whose
/// dirty-check could not complete), each with the reason and counts behind
/// the decision so no skip is ever silent.
/// Test: `prune_orphaned_worktrees_skips_owner_unknown`,
///       `prune_orphaned_worktrees_reclaims_terminal_owner`,
///       `prune_orphaned_worktrees_skips_modified_tracked_file` (#4091).
#[derive(Debug, Clone, Default)]
pub struct OrphanSweepOutcome {
    /// Paths actually removed (or that would be removed under `dry_run`).
    pub removed: Vec<std::path::PathBuf>,
    /// Paths skipped because their sentinel's owner could not be resolved —
    /// never auto-deleted; surfaced here for `tm doctor`/manual review.
    pub owner_unknown: Vec<std::path::PathBuf>,
    /// Paths skipped because they hold unsaved work (#4091) — never
    /// auto-deleted under the default [`DirtyWorktreePolicy::Skip`].
    pub skipped_dirty: Vec<DirtyWorktree>,
    /// Paths owned by a dispatched agent (#4311) — reclaimed by that agent's
    /// exit, never by this sweep.
    ///
    /// Why this is reported rather than merely skipped: before #4311 these
    /// carried no sentinel and landed in `owner_unknown`, so they were
    /// unreclaimable but VISIBLE in `--dry-run`, the prune HTTP route, the MCP
    /// tool, and `tm doctor`. Attributing them must not cost an operator that
    /// view — a directory that vanishes from every report is worse than one
    /// reported as unreclaimable.
    /// Test: `prune_orphaned_worktrees_skips_an_agent_owned_worktree`.
    pub agent_owned: Vec<std::path::PathBuf>,
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
    /// - a `Decommissioned` match →
    ///   [`compact_and_release_slot`](Self::compact_and_release_slot), which
    ///   removes it from the store (compaction) → [`PruneAction::Removed`] and
    ///   frees its slot number unless the record the store held at that moment
    ///   says otherwise (#5897).
    ///
    /// Slot-release ownership is shared with
    /// [`sweep_terminal_records`](SessionManager::sweep_terminal_records), and both
    /// gate on the same `workspace_needs_protection` predicate — see
    /// [`compact_and_release_slot`](Self::compact_and_release_slot) for why an
    /// unconditional release is the worse outcome, and why the guard reads the
    /// record afresh rather than trusting the snapshot this loop iterates.
    ///
    /// When `dry_run` is true NOTHING is mutated — the returned [`PruneOutcome`]
    /// lists what WOULD happen. A per-session failure is logged and skipped (the
    /// sweep is best-effort, like the orphan-GC) so one stuck session cannot block
    /// the rest of a legacy purge.
    /// Test: `decommission_all_ephemeral_ignores_non_ephemeral` (ephemeral path),
    /// `prune_by_state_never_touches_active` (Stopped→Decommissioned + the
    /// running-state safety gate), `prune_decommissioned_compacts` (compaction),
    /// `prune_all_targets_non_running`, `prune_dry_run_reports_without_mutating`,
    /// `prune_compaction_releases_the_slot`,
    /// `prune_compaction_keeps_the_slot_when_the_worktree_is_still_on_disk`.
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
        //
        // #5859: the gate fails CLOSED. `is_running` now returns `Err` when tmux
        // could not be observed at all, and that error aborts the whole prune
        // rather than reading as "not running" and tearing down a live pane.
        // `include_active` short-circuits before the probe, so the bulk
        // ephemeral sweep (which ignores liveness by design) still runs on a
        // host with no reachable tmux.
        let tmux = self.tmux.as_ref();
        let mut targets: Vec<SessionRecord> = Vec::new();
        for record in all.into_iter().filter(|r| matches_filter(r, filter)) {
            if include_active || !is_running(&record, tmux)? {
                targets.push(record);
            }
        }

        // Worktree base names for the slot-release guard below, resolved AT MOST
        // ONCE per prune and only when a tombstone is actually compacted —
        // `worktree_dir_names` reads config and logs, so a per-record resolve
        // would repeat both per record (see `is_session_worktree_with`).
        let mut names: Option<trusty_common::workspace_layout::WorktreeDirNames> = None;

        let mut sessions = Vec::with_capacity(targets.len());
        for record in targets {
            // Both `Decommissioned` and `Deleted` (`--deleted--`) are terminal
            // tombstones: prune COMPACTS (removes) them rather than re-running a
            // decommission teardown.
            let is_tombstone = is_tombstone(&record);
            // Predicted action for the `dry_run` preview, and the default for
            // a real run — overwritten below by the ACTUAL `decommission`
            // result. A dry-run cannot know in advance whether a worktree
            // will turn out dirty (`inspect_dirt` only runs as part of a real
            // teardown attempt), so the preview stays optimistic here exactly
            // as it did before this fix.
            let mut action = if is_tombstone {
                PruneAction::Removed
            } else {
                PruneAction::Decommissioned
            };
            let mut retained_workspace_path = None;

            if !dry_run {
                if is_tombstone {
                    // #5897: compaction drops the tombstone from the store AND
                    // frees its slot — otherwise `numbered_snapshot` renders it as
                    // `-- deleted --` forever and `NUM` climbs, the opposite of
                    // what prune advertises.
                    let names = names.get_or_insert_with(super::decommission::worktree_dir_names);
                    match self.compact_and_release_slot(&record, names).await {
                        Ok(true) => {}
                        // #5912: the record stopped being a tombstone inside the
                        // window, so nothing was compacted. Reporting `Removed`
                        // for a session still in the store would be a lie.
                        Ok(false) => continue,
                        Err(e) => {
                            warn!(id = %record.id, "prune: compaction failed: {e}; skipping");
                            continue;
                        }
                    }
                } else {
                    match self.decommission(&record.id, caller).await {
                        Ok((tombstone, workspace_removed)) => {
                            // The dirty-worktree guard in `decommission` can
                            // tombstone a record while deliberately leaving its
                            // in-project worktree on disk. Surface that here
                            // instead of reporting the same `Decommissioned`
                            // line regardless of whether the worktree was
                            // actually removed — the previous version of this
                            // loop discarded the `bool` half of
                            // `decommission`'s return entirely.
                            if !workspace_removed && tombstone.workspace_path.is_some() {
                                action = PruneAction::DecommissionedWorktreeRetained;
                                retained_workspace_path = tombstone.workspace_path.clone();
                            }
                        }
                        Err(e) => {
                            warn!(id = %record.id, "prune: decommission failed: {e}; skipping");
                            continue;
                        }
                    }
                }
            }

            sessions.push(PrunedSession {
                id: record.id.to_string(),
                tmux_name: record.tmux_name.clone(),
                state: record.state.to_string(),
                action,
                retained_workspace_path,
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

    /// Compact `snapshot` out of the store and free its slot — deciding against
    /// the record the store holds at that moment, never the snapshot (#5897).
    ///
    /// Why (the release): [`super::slots::SlotRegistry::release`] had exactly one
    /// caller, [`SessionManager::sweep_terminal_records`], so compaction removed
    /// the record and left the registry still holding its number.
    /// [`SessionManager::numbered_snapshot`] walks the slots the registry HOLDS
    /// and tombstones any whose record has vanished, so a pruned session kept
    /// rendering as a `-- deleted --` row and its number was never reusable —
    /// `NUM` climbed with every prune. The two paths now agree on who owns slot
    /// release: both do, under the same conditions.
    ///
    /// Why (the re-read): [`prune_managed`](Self::prune_managed) takes ONE
    /// `cached_all()` snapshot before its loop, then tears tmux sessions and git
    /// worktrees down inside that loop, so a tombstone reached late can be judged
    /// against a body read minutes earlier while any process is free to write the
    /// store. Retention's sibling re-reads immediately before its own destructive
    /// step for the same reason ([`SessionManager::revalidate_for_eviction`],
    /// #1845 item 9).
    ///
    /// What the re-read is actually for is `state`, not `workspace_path`.
    /// [`SessionManager::mark_reactivated`] revives a `Decommissioned` record in
    /// place, needs no tmux, and can land anywhere in that window — so a record
    /// targeted as a tombstone can be a LIVE session by the time prune reaches it.
    /// Two things then ride on that one reload (#5912). Handing its number back is
    /// the silent reuse #3034 exists to prevent, and a hazard this path introduces:
    /// before #5897 prune freed no slot at all. Deleting its record is worse and
    /// predates the slot work — `self.get(id)` starts answering `SessionNotFound`
    /// for a session that is live and running, which breaks hook correlation and
    /// `tm session status`, and `numbered_snapshot` renders the live session as a
    /// `-- deleted --` phantom. So the reload gates the removal and the release
    /// together; a record that is no longer a tombstone keeps both.
    /// `workspace_path` cannot move under this guard by contrast — its only
    /// production writers are `set_workspace` (always on an id `spawn_managed`
    /// minted moments earlier) and `decommission`, which since #4344 blanks it
    /// only once the directory is already gone, so a stale `Some(gone)` and a
    /// fresh `None` reach the same verdict through the probe below.
    ///
    /// `workspace_needs_protection` is the other condition, and it is the point
    /// rather than caution: it refuses to release while the session's
    /// `.worktrees/<uuid>` directory and its `.trusty-mpm-worktree` sentinel are
    /// still on disk, because something may still be standing in that tree.
    /// Reusing retention's predicate rather than restating it is what keeps the
    /// two paths from drifting apart again.
    ///
    /// Accepted consequence, and it applies to the workspace-protection arm only:
    /// compacting a record whose worktree is still on disk leaves a `-- deleted --`
    /// row, because that arm does remove the record and keep the number. The
    /// retention sweep frees that slot once the worktree goes. The non-tombstone
    /// arm removes nothing, so it leaves no such row.
    ///
    /// What: takes the store write lock ONCE, reloads so an out-of-process write
    /// is visible, and reads the record — then that one body decides everything.
    /// If the store no longer holds the record, or no longer holds it as a
    /// tombstone, the record and the slot are both kept and the lock drops
    /// untouched. Otherwise the record is removed under the same acquisition, so
    /// the body judged is the body removed, and the slot then releases unless
    /// `workspace_needs_protection` says its workspace still looks live. A record
    /// holding no slot (never listed, so never observed) releases nothing —
    /// already `release`'s no-op case.
    ///
    /// Returns whether the record was actually compacted, so the caller can omit
    /// a still-live session from the outcome instead of reporting
    /// [`PruneAction::Removed`] for a record still in the store (#5912).
    /// Test: `prune_compaction_releases_the_slot`,
    /// `prune_compaction_keeps_the_slot_when_the_worktree_is_still_on_disk`,
    /// `prune_keeps_the_record_and_slot_of_a_session_reactivated_after_the_snapshot`.
    async fn compact_and_release_slot(
        &self,
        snapshot: &SessionRecord,
        names: &trusty_common::workspace_layout::WorktreeDirNames,
    ) -> Result<bool, ManagedError> {
        let mut store = self.store.write().await;
        store.reload_if_changed().await?;
        let current = store.cached_get(&snapshot.id).ok();
        // #5912: the reload decides the REMOVAL too, not just the release. A
        // record that is no longer a tombstone keeps both.
        let Some(record) = current.filter(is_tombstone) else {
            drop(store);
            info!(
                id = %snapshot.id,
                "prune: kept the record and its slot — the store holds no tombstone for it"
            );
            return Ok(false);
        };
        store.remove(&snapshot.id).await?;
        drop(store);
        if super::retention::workspace_needs_protection(
            record.workspace_path.as_deref(),
            names,
            |p| p.try_exists(),
        ) {
            info!(
                id = %record.id,
                "prune: compacted the record but kept its slot — its workspace still looks \
                 like a live session worktree"
            );
            return Ok(true);
        }
        if let Some(slot) = self.slots.write().await.release(&record.id) {
            info!(id = %record.id, slot, "prune: freed the compacted record's slot");
        }
        Ok(true)
    }

    /// Remove a single decommissioned tombstone from the store (#1508).
    ///
    /// Why: the age-based reaper and any caller that has just decommissioned a
    /// record may want it gone from `sessions.json` rather than left as a tombstone,
    /// so the file stops growing unbounded. This is the single-record compaction
    /// primitive [`prune_managed`](Self::prune_managed) uses for the
    /// `Decommissioned` filter, exposed for direct callers.
    /// What: removes the record keyed by `id` via [`SessionStore::remove`](crate::session_manager::SessionStore::remove) and
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
    /// live session (i.e. whose path appears in `in_use_workspace_paths`).
    ///
    /// SAFETY: only directories whose full canonicalized path is NOT in the active
    /// set are removed. Active session worktrees are NEVER touched. Paths are
    /// canonicalized to handle symlinks correctly (Fix 1b, #1840).
    ///
    /// TOCTOU safety (#1840, hardened #1845 item 9): the sweep runs in two phases.
    /// Phase 1 discovers orphan candidates using the caller-supplied
    /// `in_use_workspace_paths` snapshot (which may have been taken moments before
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
    /// (git-derived since #4207, but still blocking — it spawns git per
    /// project); panics are propagated as `Err`. Phase 2
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
    /// #4091 DIRTY-TREE GATE (applied AFTER the #3649 gate, additively — it
    /// never widens what ownership already approved, only narrows it): every
    /// candidate that survived the ownership gate is passed to
    /// [`inspect_dirt`], which fails toward DIRTY on any error. Under the
    /// default `policy` ([`DirtyWorktreePolicy::Skip`]) a dirty candidate is
    /// NEVER removed — it is reported in [`OrphanSweepOutcome::skipped_dirty`]
    /// with the reason and file/commit counts. `DirtyWorktreePolicy::ForceDiscard`
    /// removes it anyway after a `warn!` naming exactly what is being
    /// discarded; that variant is reachable only from an explicit operator
    /// opt-in (`discard_dirty` on the HTTP route / `tm session prune-worktrees
    /// --discard-dirty`), never from the default `/tm-session-pause` path.
    /// The gate runs under `dry_run` too, so a preview matches a real run.
    ///
    /// #4118 DIRTY-GATE TOCTOU: the Phase 1.5 verdict above is computed for
    /// EVERY candidate before ANY removal happens, so across a ~95-candidate
    /// sweep the gap between "certified clean" and "deleted" is the sweep's
    /// whole duration — minutes, not the sub-millisecond window the paragraph
    /// above describes for the ACTIVE-SESSION check. [`inspect_dirt`] is
    /// therefore re-run immediately before each individual
    /// `remove_session_worktree`, so the authoritative verdict is adjacent to
    /// the deletion. The Phase 1.5 pass is kept because it is what `dry_run`
    /// reports and what keeps a preview honest. Two extra git invocations per
    /// candidate is nothing against a `remove_dir_all` of gigabytes.
    ///
    /// Test: `prune_orphaned_worktrees_removes_orphan`,
    /// `prune_orphaned_worktrees_spares_active`,
    /// `prune_orphaned_worktrees_store_snapshot_blocks_deletion` (item 1),
    /// `prune_orphaned_worktrees_skips_owner_unknown`,
    /// `prune_orphaned_worktrees_reclaims_terminal_owner`,
    /// `prune_orphaned_worktrees_spares_live_owner`,
    /// `prune_orphaned_worktrees_spares_recent_unregistered_owner` (#3649),
    /// `prune_orphaned_worktrees_skips_modified_tracked_file`,
    /// `prune_orphaned_worktrees_skips_untracked_file`,
    /// `prune_orphaned_worktrees_skips_unpushed_commit`,
    /// `prune_orphaned_worktrees_reclaims_clean_pushed_worktree`,
    /// `prune_orphaned_worktrees_skips_when_dirty_check_errors`,
    /// `prune_orphaned_worktrees_force_discards_dirty` (#4091).
    pub async fn prune_orphaned_worktrees(
        &self,
        repos_root: &std::path::Path,
        in_use_workspace_paths: &[std::path::PathBuf],
        dry_run: bool,
        policy: DirtyWorktreePolicy,
    ) -> Result<OrphanSweepOutcome, anyhow::Error> {
        use super::decommission::{WorktreeRemoval, remove_session_worktree};
        use super::worktree_ownership::SentinelOwner;
        use std::collections::HashSet;

        let repos_root = repos_root.to_path_buf();
        // Build a canonicalized set for O(1) lookup and symlink safety.
        let initial_in_use: HashSet<std::path::PathBuf> = in_use_workspace_paths
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();

        // Phase 1: discover orphan candidates using the initial snapshot.
        // Propagate a spawn_blocking panic as Err (#1845 item 7) rather than
        // silently returning an empty candidate list.
        let candidates = tokio::task::spawn_blocking({
            let initial_in_use = initial_in_use.clone();
            move || find_orphaned_worktrees(&repos_root, &initial_in_use)
        })
        .await
        .map_err(|e| anyhow::anyhow!("prune-worktrees: orphan scan panicked: {e}"))?;

        // Phase 1.5 (#3649): classify every candidate by ownership BEFORE any
        // deletion decision — applied identically under dry-run and real runs
        // so a preview reflects reality.
        let mut owner_unknown = Vec::new();
        let mut agent_owned = Vec::new();
        let mut skipped_dirty: Vec<DirtyWorktree> = Vec::new();
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
                // #4311: attributed to a dispatched agent, whose liveness this
                // sweep cannot answer — there is no session record to look up,
                // so `resolve_ownerless_with_grace` would report every agent
                // worktree reclaimable once past the grace window. Skipped like
                // the live-owner arm below: owned, and reclaimed by
                // `daemon::services::agent_worktree_reap` on the agent's exit.
                SentinelOwner::Agent(agent, _) => {
                    info!(
                        path = %candidate.display(),
                        agent_id = %agent.agent_id,
                        "prune-worktrees: owned by a dispatched agent — reclaimed when that \
                         agent exits, never by this sweep (#4311)"
                    );
                    agent_owned.push(candidate);
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
                    // #4091: last gate — never destroy unsaved work.
                    if let Some(dirt) = dirt_blocks_removal(&candidate, policy, "scan") {
                        skipped_dirty.push(dirt);
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
                skipped_dirty,
                agent_owned,
            });
        }

        // Phase 2 (real-delete path): ONE fresh snapshot immediately before the
        // deletion loop (#1845 item 9). Each active path is inserted in BOTH its
        // canonicalized form (for symlink-safe comparison) and its raw form
        // (Finding 3 #1845: if canonicalize fails on the active side, keep the
        // raw path as a protective fallback so a canonicalize failure can never
        // cause an active worktree to be misidentified as an orphan and deleted).
        let fresh_in_use: HashSet<std::path::PathBuf> = {
            let mut set = HashSet::new();
            // Raw `workspace_path`s actually observed THIS sweep, used below to
            // evict stale streak entries (#3715 finding 2) — kept separate from
            // `set` because `set` also accumulates canonicalized forms, which are
            // not the keys `CanonicalizeFailureStreaks` tracks.
            let mut checked_paths: HashSet<std::path::PathBuf> = HashSet::new();
            // #4288: DELIBERATELY UNFILTERED by record state, exactly like the
            // caller-supplied set this backstops. Do NOT add
            // `if r.state != Active { continue; }` here — a `SessionRecord`'s
            // state is bookkeeping, not a liveness signal (session
            // `2eb72dca-…` was measured RUNNING in tmux pane `%981` while
            // recorded `state: "stopped"`, holding 12 modified tracked files,
            // 31 untracked files, and 1 unpushed commit).
            //
            // This read is the LAST thing standing between a reclaimable
            // candidate and `remove_session_worktree`. It is what makes
            // narrowing any single caller's active set survivable, so it is
            // also the one whose loss is least visible: filter here and the
            // callers' own unfiltered reads still hide the damage until one of
            // them is tidied up too. Pinned by
            // `reap_spares_a_stopped_records_workspace` (real sweep) — that
            // test goes red once this read AND a caller's set are both narrowed.
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
            if fresh_in_use.contains(&canonical_candidate) || fresh_in_use.contains(&candidate) {
                info!(
                    path = %candidate.display(),
                    "prune-worktrees: skipping — active session appeared after initial snapshot"
                );
                continue;
            }

            // #4118 TOCTOU: the scan-time verdict is now minutes old. Re-ask
            // immediately before THIS removal so the clean-to-deleted window is
            // sub-millisecond again rather than the whole sweep's duration.
            if let Some(dirt) = dirt_blocks_removal(&candidate, policy, "pre-removal") {
                skipped_dirty.push(dirt);
                continue;
            }

            info!(path = %candidate.display(), "prune-worktrees: removing orphaned worktree");
            let candidate_clone = candidate.clone();
            let outcome =
                tokio::task::spawn_blocking(move || remove_session_worktree(&candidate_clone))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!(
                            "prune-worktrees: spawn_blocking panicked during removal: {e}"
                        );
                        WorktreeRemoval::Kept(format!("the removal task panicked: {e}"))
                    });
            // #4732: the remover now reports WHY it kept a worktree — most
            // often a deliberate refusal (a `git worktree lock`, a stale
            // pointer), which used to be indistinguishable from a silent no-op.
            if let Some(reason) = outcome.reason() {
                warn!(
                    path = %candidate.display(),
                    "prune-worktrees: worktree kept — {reason}"
                );
            }
            if outcome.removed() {
                removed.push(candidate);
            }
        }
        Ok(OrphanSweepOutcome {
            removed,
            owner_unknown,
            skipped_dirty,
            agent_owned,
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
    /// immediately before deletion) is inherited unchanged, as is the #4091
    /// dirty-tree gate — the periodic daemon sweep ALWAYS uses the default
    /// [`DirtyWorktreePolicy::Skip`] and has no way to opt into discarding
    /// work. Returns the paths removed.
    ///
    /// # What actually bounds THIS path (#4224 review, corrected)
    ///
    /// This is the only entry point that deletes with no human present, so the
    /// guarantees named here have to be the ones that hold. Until #4207 this doc
    /// claimed an "only leaf dirs under `.worktrees/`, never the base clone"
    /// guard; that guard was a property of the five-shape walk and no longer
    /// exists. In its place, a directory must clear ALL of:
    ///
    /// 1. git itself registers it as a worktree (a husk or a stray `mkdir` is
    ///    not a candidate) that is not main, bare, prunable, or `locked`;
    /// 2. it is a STRICT DESCENDANT of the managed project directory whose
    ///    registry named it, and lies under `repos_root`
    ///    ([`super::worktree_registry::enumerate_registered_worktrees`]) — so
    ///    the operator's own checkouts and anything parked beside a project are
    ///    structurally unreachable, not merely unlisted;
    /// 3. it is absent from both the initial and the pre-deletion active set;
    /// 4. it carries a #3649 ownership sentinel naming an owner that is
    ///    provably ownerless past [`super::worktree_ownership::OWNERLESS_GRACE`]
    ///    — an absent, empty, or unparsable sentinel is reported, never removed;
    /// 5. `git worktree list` (asked at the candidate itself) agrees; and
    /// 6. the #4091/#4118 dirty gate finds no uncommitted or unpushed work,
    ///    re-checked immediately before the removal.
    ///
    /// Gates 2 and 4 are independent structural boundaries by design: neither is
    /// load-bearing alone.
    /// Test: `reap_orphaned_worktrees_removes_orphan_preserves_live` and
    /// `reap_spares_a_stopped_records_workspace` (#4288) in
    /// `super::reap_orphaned_worktrees_tests`;
    /// `enumerate_excludes_a_sibling_checkout_of_the_same_repo` and
    /// `enumerate_excludes_a_worktree_parked_beside_the_project` pin gate 2.
    pub async fn reap_orphaned_worktrees(
        &self,
        repos_root: &std::path::Path,
    ) -> Result<OrphanSweepOutcome, anyhow::Error> {
        // #4288 (item 4 of #4207): DELIBERATELY UNFILTERED, exactly as in the
        // manual `prune_worktrees_route`. Do NOT "tidy this up" by adding
        // `.filter(|r| r.state == ManagedSessionState::Active)` — every record
        // with a `workspace_path` belongs in this set, whatever its state.
        //
        // Why: a `SessionRecord`'s state is bookkeeping, NOT a liveness signal.
        // It is written by reconcile/stop/hook paths that can miss, race, or be
        // skipped entirely, so live sessions are routinely observed carrying a
        // terminal state. Measured on this repo 2026-07-28: session
        // `2eb72dca-de08-481b-8dfa-22ab7f81b1f9` was RUNNING (tmux pane `%981`,
        // `pane_current_path` inside its own worktree) while `sessions.json`
        // recorded it as `state: "stopped"`, holding 12 modified tracked files,
        // 31 untracked files, and 1 unpushed commit.
        //
        // SCOPE OF THIS SET, measured rather than assumed (#4288): narrowing
        // THIS read alone does NOT delete anything. It makes a live-but-
        // mislabelled worktree an orphan CANDIDATE, but `prune_orphaned_worktrees`
        // then re-reads the store for its Phase 2 `fresh_in_use` snapshot —
        // itself deliberately unfiltered, see the comment there — and that
        // second read still spares the candidate immediately before deletion.
        // The two unfiltered reads are defense-in-depth: NEITHER is load-bearing
        // alone, and data loss requires narrowing BOTH. Do not read that as
        // permission to narrow one "because the other covers it" — that reasoning
        // applied twice is exactly how a pair of independent boundaries collapses
        // into none, and this path is the one that runs unattended on a timer
        // (`daemon::mod`'s orphan-GC loop) with `dry_run: false` hardcoded and no
        // preview mode to catch it.
        //
        // Pinned by `reap_spares_a_stopped_records_workspace` in
        // `super::reap_orphaned_worktrees_tests`, which asserts a STOPPED
        // record's worktree survives a real sweep, and by the pair property
        // above: that test goes red when BOTH reads are narrowed, and stays
        // green under either single narrowing because the other read genuinely
        // still protects the worktree.
        //
        // Be precise about what that test does NOT add: the literal
        // `== Active` tidy-up at THIS line was already caught before it existed,
        // because `create_with_id` persists records as `Provisioning` (not
        // `Active`), so `prune_orphaned_worktrees_removes_orphan_preserves_live`
        // already fails on it. The new coverage here is the STOPPED case — a
        // state that reads terminal but is routinely live — and the pair
        // property, not the tidy-up as literally written.
        let in_use: Vec<std::path::PathBuf> = self
            .list()
            .await
            .into_iter()
            .filter_map(|r| r.workspace_path)
            .collect();
        self.prune_orphaned_worktrees(repos_root, &in_use, false, DirtyWorktreePolicy::Skip)
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
            // #4118: `decommission` reaches the SAME `remove_session_worktree`
            // (worktree remove --force + remove_dir_all + branch -D) as the
            // orphan sweep, but never passed through the #4091 guard — and this
            // reaper fires automatically on every daemon GC tick. Guarding one
            // sweep while its sibling deletes freely is a half-measure. There is
            // no discard opt-in here by design: an automatic reaper must never
            // be able to destroy work.
            //
            // Checked UNCONDITIONALLY, not just for `.worktrees/<leaf>` paths:
            // `decommission` also `remove_dir_all`s the whole workspace when
            // `workspace_owned` is true, which an `is_session_worktree` filter
            // would have excused entirely. No current call site sets that flag,
            // but it is persisted store data that outlives upgrades — and "no
            // caller does this today" is not a safety property.
            if let Some(dirt) = record.workspace_path.as_deref().and_then(inspect_dirt) {
                warn!(
                    id = %record.id, path = %dirt.path.display(), reason = %dirt.reason,
                    "auto-reap: workspace holds unsaved work — leaving the session in \
                     place for an operator (#4118)"
                );
                continue;
            }
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

#[cfg(test)]
#[path = "prune_slot_tests.rs"]
mod slot_tests;
