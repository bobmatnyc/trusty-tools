//! Automatic post-merge worktree reclaim, and the one place its probes are
//! assembled (#7504, epic #7505).
//!
//! Why: #2919 shipped the reclaim ENGINE and a manual verb — `tm session
//! prune-worktrees --merged-prs` — and nothing that fires it. The PM therefore
//! removed each merged worktree by hand, and on 2026-09-11 disk hit 90% because
//! that step is a chore nobody owns. The owner's ruling for epic #7505 is that
//! disk management is a deterministic DAEMON function: the daemon decides, by
//! rule evaluation over observable state, and writes an audit line carrying its
//! reason. This module is that function for the post-merge half.
//!
//! What: [`reclaim`] is the single production entry point — the one place the
//! five probes the engine needs are bound to real sources. Both callers go
//! through it, so an operator-typed `--merged-prs` and the automatic sweep can
//! never apply different gates: the `prune-worktrees` route (which still decides
//! Report-vs-Remove from its own `dry_run`) and [`reclaim_loop`], the daemon's
//! background sweep. [`SweepReport`] is the summary the loop logs.
//!
//! # The trigger is PR-merged state, not the clock
//!
//! The cadence decides only when the daemon ASKS. What it acts on is
//! `gh`-reported merge state, re-read per candidate immediately before that
//! candidate's deletion by
//! [`recheck_before_delete`](crate::session_manager::worktree_reclaim_sweep::recheck_before_delete).
//! A worktree whose pull request is open, closed-unmerged, or unresolvable is
//! never removed however often the sweep runs — so the interval cannot make the
//! sweep guess, only make it late.
//!
//! # Why this one runs UNBOUNDED, unlike the doctor probe
//!
//! [`SurveyBudget`](crate::session_manager::worktree_reclaim_sweep::SurveyBudget)
//! bounds the read-only doctor survey at 3 seconds because an interactive
//! diagnostic must answer inside the client's timeout. A destructive pass cannot
//! take that trade: a partial classification does not shrink a REPORT, it shrinks
//! what gets reclaimed, which is the exact failure this issue exists to fix. So
//! the sweep inherits the engine's unbounded budget and pays for it with a slow
//! default cadence and [`MissedTickBehavior::Delay`], which together stop a sweep
//! that overran its interval from re-firing the instant it finishes.
//!
//! # Fail direction
//!
//! Toward keeping the directory, at every layer. A sweep that cannot run reclaims
//! nothing and says so at `warn`; a panicked blocking task is an `Err` the caller
//! reports rather than an empty success; a removal that did not complete lands in
//! `removal_failed` and makes [`SweepReport::failed`] true, so the loop logs the
//! tick at `warn` instead of `info`. No arm turns a failure into an advanced
//! state.
//!
//! Test: `merged_pr_reclaim_tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_reclaim::{LiveClaims, ReclaimMode, ReclaimOutcome};
use crate::session_manager::worktree_reclaim_launch::process_launch_dirs;
use crate::session_manager::worktree_reclaim_sweep::reclaim_merged_pr_worktrees;

/// Environment variable that disables the automatic sweep entirely.
///
/// Why: the sweep deletes directories, so an operator must be able to turn it off
/// without editing a config file or stopping the daemon's other loops. It
/// defaults ON — the owner's ruling is that this is a daemon function rather than
/// a PM chore, and a reclaim that has to be enabled is the manual step #2919
/// already shipped.
/// What: `0`/`false`/`off`/`no` (trimmed, case-insensitive) disables; anything
/// else, including unset, enables. Mirrors `TRUSTY_MPM_ORPHAN_GC`.
/// Test: `parse_enabled_defaults_on_and_honours_the_off_switch`.
pub(crate) const ENV_ENABLED: &str = "TRUSTY_MPM_WORKTREE_RECLAIM";

/// Environment variable overriding the sweep cadence, in seconds.
///
/// Test: `parse_interval_falls_back_on_junk_and_zero`.
pub(crate) const ENV_INTERVAL_SECS: &str = "TRUSTY_MPM_WORKTREE_RECLAIM_INTERVAL_SECS";

/// Default sweep cadence: one hour.
///
/// Why an hour rather than the orphan-GC loop's minute: one pass costs a `gh`
/// lookup per registered worktree plus an unbounded byte walk, which on this
/// repository has exceeded 600 seconds over 46 worktrees. A merged worktree costs
/// only disk until it goes, so being up to an hour late is free; re-walking a
/// terabyte every minute is not.
/// Test: `parse_interval_falls_back_on_junk_and_zero`.
pub(crate) const DEFAULT_INTERVAL_SECS: u64 = 3600;

/// Pure parse of [`ENV_ENABLED`] into an on/off decision.
///
/// Why pure: env vars are process-global, and ~20 tests in this binary write
/// them. Taking the raw value keeps the policy testable with no mutation at all
/// (#5544).
/// Test: `parse_enabled_defaults_on_and_honours_the_off_switch`.
pub(crate) fn parse_enabled(raw: Option<&str>) -> bool {
    match raw {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

/// Whether the automatic sweep should be spawned at all.
///
/// Test: the policy is covered by `parse_enabled_defaults_on_and_honours_the_off_switch`.
pub(crate) fn enabled() -> bool {
    parse_enabled(std::env::var(ENV_ENABLED).ok().as_deref())
}

/// Pure parse of [`ENV_INTERVAL_SECS`] into seconds.
///
/// Why it rejects zero: `tokio::time::interval` panics on a zero period, and a
/// near-zero one would busy-loop a pass that spawns subprocesses. An unparsable
/// or non-positive value falls back to [`DEFAULT_INTERVAL_SECS`] rather than
/// disabling the sweep — disabling is [`ENV_ENABLED`]'s job, and inferring it
/// from a typo would turn a fat-fingered number into silent non-reclamation.
/// Test: `parse_interval_falls_back_on_junk_and_zero`.
pub(crate) fn parse_interval_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

/// One tick's outcome, reduced to the four numbers the audit line carries.
///
/// Why a type rather than four locals: [`Self::failed`] is the thing the loop
/// branches on, and the Fail-Open rule means that branch must exist in exactly
/// one place. Reading `removal_failed.is_empty()` at the log site is how a later
/// edit drops it.
/// What: counts, plus the bytes the engine actually attributed to removals.
/// Test: `a_tick_with_a_failed_removal_is_reported_as_a_failure`,
/// `a_clean_tick_is_not_a_failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SweepReport {
    /// Worktrees removed this tick.
    pub reclaimed: usize,
    /// Bytes the engine attributed to those removals.
    pub bytes: u64,
    /// Candidates a pre-delete gate spared — expected, not a failure.
    pub refused: usize,
    /// Removals that were attempted and did NOT complete.
    pub failed: usize,
}

impl SweepReport {
    /// Reduce an engine outcome to the tick's numbers.
    ///
    /// Test: `a_tick_with_a_failed_removal_is_reported_as_a_failure`.
    pub(crate) fn from_outcome(outcome: &ReclaimOutcome) -> Self {
        Self {
            reclaimed: outcome.removed.len(),
            bytes: outcome.removed_bytes,
            refused: outcome.refused_at_recheck.len(),
            failed: outcome.removal_failed.len(),
        }
    }

    /// Whether this tick failed to complete work it began.
    ///
    /// Why it is NOT about `refused`: a gate sparing a worktree is the sweep
    /// working. A removal that began and did not finish is the only outcome that
    /// leaves the workspace in a state nobody asked for.
    /// Test: `a_tick_with_a_failed_removal_is_reported_as_a_failure`,
    /// `a_clean_tick_is_not_a_failure`.
    pub(crate) fn failed(&self) -> bool {
        self.failed > 0
    }
}

/// The sweep cadence, honouring [`ENV_INTERVAL_SECS`].
///
/// Test: the policy is covered by `parse_interval_falls_back_on_junk_and_zero`.
pub(crate) fn interval_secs() -> u64 {
    parse_interval_secs(std::env::var(ENV_INTERVAL_SECS).ok().as_deref())
}

/// The workspace root the operator's config names — what production callers pass
/// as [`reclaim`]'s `repos_root`.
///
/// Why a named function: the `prune-worktrees` route already resolves this for its
/// orphan pass, and the automatic sweep must resolve the SAME root or the two
/// reclaim paths would walk different workspaces.
/// Test: covered through the callers; the resolution itself is
/// `core::trusty_tools_config`'s.
pub(crate) fn configured_workspace_root() -> PathBuf {
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    crate::core::trusty_tools_config::workspace_root(&config)
}

/// Run the merged-PR reclaim with every production probe bound (#7504).
///
/// Why this exists as a function: binding the probes is the part that is easy to
/// get subtly wrong — `in_use_now` must RE-READ the store per candidate rather
/// than close over a snapshot, the keep-list must be read fallibly so an
/// unparsable config keeps everything, and the agent probe must reach the
/// delegation registry rather than the session records. That assembly lived
/// inline in the `prune-worktrees` route; a second copy for the automatic sweep
/// is exactly the divergence the common-entry-point rule forbids, so there is one
/// copy and both callers use it.
/// What: resolves the adopted anchors and this process's own launch directories,
/// then runs [`reclaim_merged_pr_worktrees`] on the blocking pool — the engine is
/// synchronous and spends minutes in `git`, `gh` and filesystem walks. `mode`
/// decides whether anything is deleted; `invoking_session` is the caller's own
/// managed-session id, which gate 2 uses to tell the caller's claim from a
/// stranger's (#6806) and which the daemon's own sweep leaves `None` because it
/// occupies no pane.
///
/// `repos_root` is a PARAMETER rather than resolved here, for the reason #7357
/// made `adopted` one: resolving it internally makes this function's own tests
/// survey the operator's real workspace with an unbounded budget, which on this
/// repository exceeds 600 seconds. [`configured_workspace_root`] is what
/// production callers pass.
///
/// A panicked pass is `Err`, never an empty `ReclaimOutcome`: "reclaimed nothing"
/// and "crashed" must not render identically.
/// Test: `reclaim_on_an_empty_root_reclaims_nothing`.
pub(crate) async fn reclaim(
    state: &Arc<DaemonState>,
    repos_root: &Path,
    mode: ReclaimMode,
    invoking_session: Option<String>,
) -> Result<ReclaimOutcome, String> {
    let repos_root = repos_root.to_path_buf();
    // #7357: resolved here — on the entry point — never inside the engine, which
    // would make its unit tests read the operator's real adoption store.
    let adopted = crate::project::adopted_anchors_under(state.framework_root());
    // #7504: this process's own cwd and executable directory, resolved once per
    // pass. See `FreshProbes::launched_from` for why a value rather than a probe.
    let launched_from: Vec<PathBuf> = process_launch_dirs();
    // #2919: a HANDLE to the manager, not a captured path list — the delete loop
    // calls the closure per candidate and needs the CURRENT set, not one
    // snapshotted before a survey that takes minutes.
    let mgr_for_probe = state.session_manager().await.clone();
    // #5661: the other gates read SESSION records, and a dispatched agent has
    // none — which is how this path once deleted three live agents' worktrees.
    let state_for_agents = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let in_use_now = move || -> Option<LiveClaims> {
            // `None` means "could not be determined", which REFUSES the delete.
            // `try_current` rather than `current`: the latter PANICS off a
            // runtime, unwinding through a delete loop mid-sweep, where the
            // contract already specifies a fail-closed refusal.
            let handle = tokio::runtime::Handle::try_current().ok()?;
            // #7232: through the crate's single claim producer, which also probes
            // each claiming session for life — a tombstoned record holding an
            // org-level `workspace_path` otherwise blocks every worktree beneath
            // it.
            Some(handle.block_on(mgr_for_probe.workspace_claims(invoking_session.clone())))
        };
        let agent_state =
            move |owner: &crate::session_manager::worktree_ownership::AgentWorktreeOwner| {
                crate::daemon::services::agent_worktree_reap::delegation_state_for_agent(
                    &state_for_agents,
                    &owner.agent_id,
                )
            };
        // #6927: read FALLIBLY and re-read PER CANDIDATE. The lenient loader
        // collapses any YAML error anywhere in the file to an EMPTY keep-list,
        // which is how a vetoed worktree could be deleted; this one keeps
        // everything instead.
        let keep_list = crate::core::trusty_tools_config::load_disk_keep_list;
        reclaim_merged_pr_worktrees(
            &repos_root,
            &in_use_now,
            &agent_state,
            mode,
            &keep_list,
            &adopted,
            &launched_from,
        )
    })
    .await
    .map_err(|e| e.to_string())
}

/// Spawn the automatic sweep unless an operator switched it off (#7504).
///
/// Why the gate lives here rather than at the call site: the daemon's boot
/// sequence should read as one line per background loop, and the env contract for
/// this loop belongs beside the loop it governs — the same split
/// `spawn_idle_reaper_if_enabled` already uses.
/// What: reads [`ENV_ENABLED`] and [`ENV_INTERVAL_SECS`], then spawns
/// [`reclaim_loop`]; when disabled it logs the variable that disabled it, so an
/// operator wondering why nothing is being reclaimed finds the answer in the
/// daemon log rather than by reading this source.
/// Test: the policy is `parse_enabled_defaults_on_and_honours_the_off_switch`;
/// the loop it spawns is `reclaim_loop_exits_on_cancel`.
pub(crate) fn spawn_if_enabled(
    state: Arc<DaemonState>,
    cancel: tokio_util::sync::CancellationToken,
) {
    if !enabled() {
        info!("automatic post-merge worktree reclaim disabled via {ENV_ENABLED}");
        return;
    }
    tokio::spawn(reclaim_loop(
        state,
        Duration::from_secs(interval_secs()),
        cancel,
    ));
}

/// The daemon's automatic post-merge reclaim sweep (#7504).
///
/// Why a loop rather than a webhook: the authority on "has this merged" is
/// GitHub, and this harness has no inbound path from it. Asking `gh` on a cadence
/// is the event source available, and the gates make a late answer harmless — see
/// the module docs on why the clock is not the trigger.
/// What: one pass immediately (so a restart cleans up promptly), then every
/// [`interval`] until `cancel` fires. Each tick refuses outright on a host state
/// that must not be swept — the #6348 quadrant, where a scratch-rooted daemon
/// would otherwise reclaim the operator's real worktrees — then runs [`reclaim`]
/// in [`ReclaimMode::Remove`] and logs the tick. A tick with any failed removal
/// logs at `warn`; a tick that reclaimed nothing and failed nothing logs nothing,
/// because an hourly "reclaimed 0" is noise that trains operators to filter the
/// line that matters.
/// Test: `reclaim_loop_exits_on_cancel`,
/// `reclaim_loop_reclaims_nothing_on_a_scratch_framework_root`.
pub(crate) async fn reclaim_loop(
    state: Arc<DaemonState>,
    interval: Duration,
    cancel: tokio_util::sync::CancellationToken,
) {
    info!(
        interval_secs = interval.as_secs(),
        "automatic post-merge worktree reclaim enabled (#7504)"
    );
    let mut tick = tokio::time::interval(interval);
    // A pass can outlast its own interval on a large workspace. The default
    // `Burst` behaviour would then fire the next tick immediately and keep the
    // machine walking worktrees continuously; `Delay` restarts the clock from the
    // end of the pass instead.
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("worktree-reclaim loop: cancel signal received; exiting");
                break;
            }
            _ = tick.tick() => run_one_tick(&state).await,
        }
    }
}

/// One sweep, gated on host state and logged (#7504).
///
/// Split from [`reclaim_loop`] so a test can drive exactly one pass without
/// racing a timer.
/// Test: `reclaim_loop_reclaims_nothing_on_a_scratch_framework_root`.
pub(crate) async fn run_one_tick(state: &Arc<DaemonState>) {
    // #6348: the same refusal `reap_loop` and the orphan-GC loop apply. A daemon
    // whose framework root is a scratch directory is a test process, and its
    // registry knows nothing of the operator's real worktrees.
    if let Some(reason) = crate::daemon::host_state_refusal(state) {
        warn!("worktree-reclaim sweep: skipped — {reason}");
        return;
    }
    match reclaim(
        state,
        &configured_workspace_root(),
        ReclaimMode::Remove,
        None,
    )
    .await
    {
        Ok(outcome) => {
            let report = SweepReport::from_outcome(&outcome);
            if report.failed() {
                // The Fail-Open boundary: a removal that began and did not finish
                // is never folded into the success line.
                warn!(
                    reclaimed = report.reclaimed,
                    refused = report.refused,
                    failed = report.failed,
                    "worktree-reclaim sweep: {} removal(s) did not complete: {}",
                    report.failed,
                    outcome.removal_failed.join("; ")
                );
                return;
            }
            if report.reclaimed > 0 {
                info!(
                    reclaimed = report.reclaimed,
                    bytes_freed = report.bytes,
                    refused = report.refused,
                    "worktree-reclaim sweep: reclaimed merged-PR worktree(s) (#7504)"
                );
            }
        }
        Err(e) => warn!("worktree-reclaim sweep: the pass did not complete: {e}"),
    }
}

#[cfg(test)]
#[path = "merged_pr_reclaim_tests.rs"]
mod merged_pr_reclaim_tests;
