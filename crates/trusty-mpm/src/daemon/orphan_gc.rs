//! Orphan garbage-collection for managed tmux sessions.
//!
//! Why: the RAII [`crate::session_manager::TmuxSessionGuard`] foundation stops
//! *new* leaks — a session the daemon spawns is killed when its owning guard
//! drops. But it cannot clean up orphans that already exist: sessions leaked by
//! older daemon builds, by crashes before the guard landed, or by a `kill -9`
//! of the daemon. Those `tmpm-*` tmux sessions accumulate forever, drowning the
//! dashboard and eventually the host. This module is the self-healing other
//! half: a conservative reaper that reconciles the live `tmux ls` against BOTH
//! session registries and reaps only sessions that are provably untracked,
//! managed, and idle.
//!
//! What: [`classify_session`] is the pure, side-effect-free decision function —
//! given one tmux pane row and the two registries' known-name sets, it returns a
//! [`GcDecision`]. [`OrphanGc`] holds the cross-pass debounce state and drives a
//! full sweep ([`OrphanGc::plan_sweep`] for the pure plan, plus the async
//! [`run_sweep`] that wires it to a real driver and actually kills).
//!
//! Safety is paramount: there are routinely dozens of *live* agent sessions on
//! the host. A false kill destroys a running agent's work. Every gate here fails
//! CLOSED — anything we are not certain is a dead, untracked, managed shell is
//! KEPT. See [`classify_session`] for the exact conjunction.
//!
//! Test: `classify_*` and `debounce_*` unit tests in the `tests` submodule below
//! exercise the full decision matrix with a fake driver; `run_sweep` is covered
//! by `tests/orphan_gc_sweep.rs` against a fake [`ManagedTmuxDriver`].

use std::collections::HashSet;
use std::process::Command;

use tracing::{debug, info, warn};

use crate::core::names::is_managed_session_name;

/// Shell commands that mark a pane as *idle* (no agent running inside it).
///
/// Why: an orphaned managed session that has dropped back to a bare login shell
/// is dead weight and safe to reap; a session still running `claude`/`node`/`uv`
/// is doing real work and must never be touched. Matching against this explicit
/// allowlist (rather than a denylist of "agent" commands) fails closed: any
/// command we do not recognise as a bare shell is treated as ACTIVE and KEPT.
/// What: the set of `pane_current_command` values tmux reports for an idle pane,
/// covering login (`-zsh`/`-bash`/…) and non-login (`zsh`/`bash`/`sh`/…) forms
/// of the common interactive shells — `zsh`, `bash`, `sh`, `fish`, `dash`,
/// `tcsh`, `csh`, `ksh` — plus the bare `login` process macOS shows briefly.
/// Including the less-common shells keeps a developer running e.g. `fish` from
/// tripping the WarnUntrackedActive path on every sweep.
/// Test: `idle_shell_commands_recognised`, `active_command_not_idle`.
const IDLE_SHELL_COMMANDS: &[&str] = &[
    "zsh", "bash", "sh", "fish", "dash", "tcsh", "csh", "ksh", //
    "-zsh", "-bash", "-sh", "-fish", "-dash", "-tcsh", "-csh", "-ksh", //
    "login",
];

/// True if `pane_command` indicates an idle pane (a bare shell, no agent).
///
/// Why: the idleness gate is the difference between reaping dead weight and
/// killing a live agent; it must be exact and conservative.
/// What: trims `pane_command` and checks membership in [`IDLE_SHELL_COMMANDS`].
/// Anything else — `claude`, `node`, `uv`, `python`, `vim`, … — is NOT idle.
/// Test: `idle_shell_commands_recognised`, `active_command_not_idle`.
pub fn is_idle_shell(pane_command: &str) -> bool {
    let cmd = pane_command.trim();
    IDLE_SHELL_COMMANDS.contains(&cmd)
}

/// One tmux pane row the GC reasons about.
///
/// Why: the reconciler needs the session name, what the pane is running, and
/// (when cheaply available) the pane's shell PID so it can do a second liveness
/// check before reaping. Bundling them keeps [`classify_session`] a pure
/// function of plain data, fully testable without spawning tmux.
/// What: the tmux `session_name`, its `pane_current_command`, and an optional
/// `pane_pid` (the pane's shell PID, `None` when tmux did not report one).
/// Test: constructed throughout the `tests` submodule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    /// tmux session name (e.g. `tmpm-brave-otter`).
    pub session_name: String,
    /// The pane's foreground command (`claude`, `zsh`, `node`, …).
    pub pane_current_command: String,
    /// The pane's shell PID, if tmux reported it.
    pub pane_pid: Option<u32>,
}

/// The GC's verdict for a single tmux session.
///
/// Why: separating the decision from the action makes the conservative reaping
/// rule unit-testable and lets the caller log every kept/warned session for
/// auditability before anything is killed.
/// What: one variant per outcome — kept (with a machine-readable reason),
/// warned-and-kept (an untracked *active* managed session, surfaced loudly), or
/// a reap *candidate* (provably orphaned this pass; still subject to debounce
/// before it is actually reaped).
/// Test: `classify_*` unit tests assert the variant for each input class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcDecision {
    /// Keep the session; carries why it was spared (for `debug!` accounting).
    Keep(KeepReason),
    /// Keep, but loudly: a managed session we do not track is running an agent.
    /// This is the dangerous case the orphan-GC must never silently reap.
    WarnUntrackedActive,
    /// The session is a reap candidate this pass (managed + untracked + idle).
    /// Debounce still gates whether it is actually killed.
    ReapCandidate,
}

/// Why a session was kept by [`classify_session`].
///
/// Why: a single `Keep` would lose the audit trail; recording the reason lets
/// the per-pass `debug!` summary explain itself and makes the unit tests assert
/// the *specific* gate that spared a session.
/// What: the mutually-exclusive reasons a session is not a reap candidate.
/// Test: asserted by `classify_keeps_non_managed`, `classify_keeps_tracked_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// Name lacks a managed prefix — not ours, never touch.
    NotManaged,
    /// Present in the old-style `DaemonState` registry.
    TrackedLegacy,
    /// Present in the new-style `SessionManager` store.
    TrackedManaged,
    /// Managed + untracked but the pane PID still has a live child process.
    LiveChildProcess,
}

/// The set of tracked session names from both registries.
///
/// Why: the orphan test is "absent from BOTH registries"; gathering the two
/// name sets into one struct keeps [`classify_session`]'s signature small and
/// makes the "tracked" check a single membership test against the union.
/// What: `legacy` holds `DaemonState`'s in-memory `tmux_name`s; `managed` holds
/// the `SessionManager` store's `tmux_name`s. `degraded` records that at least
/// one registry could NOT be fully enumerated this pass (e.g. a store read
/// error) — the protected set is therefore *incomplete* and the sweep must not
/// reap on it.
/// Test: used throughout the `tests` submodule; the degraded path is asserted by
/// `sweep_skips_reap_on_degraded_snapshot` in `tests/orphan_gc_sweep.rs`.
#[derive(Debug, Default, Clone)]
pub struct TrackedNames {
    /// tmux names known to the old-style `DaemonState` session registry.
    pub legacy: HashSet<String>,
    /// tmux names known to the new-style `SessionManager` store.
    pub managed: HashSet<String>,
    /// True when the protected set is INCOMPLETE (a registry read failed).
    ///
    /// Why: the orphan criterion is "absent from BOTH registries". If we could
    /// not fully read a registry, a tracked session may be *missing* from this
    /// snapshot, so treating its absence as "untracked" would fail OPEN and risk
    /// reaping live work. When `degraded` is set, the sweep skips its reap phase
    /// entirely (reaps nothing) and resets the debounce — mirroring the existing
    /// "tmux list error → reap nothing" path. Defaults to `false` (complete) so
    /// every existing `TrackedNames::default()` / literal construction is, as
    /// before, a fully-trusted snapshot.
    pub degraded: bool,
}

impl TrackedNames {
    /// True if `name` is tracked by either registry.
    ///
    /// Why: the orphan criterion requires absence from BOTH; a single helper
    /// avoids duplicating the two-set check at every call site.
    /// What: returns whether `name` is in `legacy` or `managed`.
    /// Test: exercised indirectly by every `classify_*` test.
    fn contains(&self, name: &str) -> bool {
        self.legacy.contains(name) || self.managed.contains(name)
    }
}

/// A liveness probe over a pane's process tree.
///
/// Why: the idleness check (`pane_current_command` is a bare shell) is the
/// primary signal, but a pane can momentarily report a shell while an agent
/// child is mid-spawn. A cheap, best-effort "does the pane PID have a live
/// `claude` child?" probe is a belt-and-braces second gate. Abstracting it
/// behind a trait keeps [`classify_session`] pure and lets tests inject a
/// deterministic answer instead of poking the real OS.
/// What: one method that answers, for a pane PID, whether a live agent child
/// exists. The real implementation walks the process tree; the test fake
/// returns a canned answer.
/// Test: [`AlwaysIdleProbe`] (below) drives the unit tests; the live probe is
/// covered by `crate::core::process` tests.
pub trait ChildLivenessProbe {
    /// True if `pane_pid` has any live child process.
    ///
    /// Fail-closed contract: this gate exists to protect live work, so the ONLY
    /// time an implementation may return `false` is when it is *confident* the
    /// pane has no live children. A `None` PID is the one unambiguous case —
    /// there is no process tree to inspect, and the idleness gate (`is_idle_shell`)
    /// already ran — so `None` returns `false`. For a present PID, any
    /// uncertainty (the probe tool is missing, errors, or its output cannot be
    /// parsed) MUST return `true` (treat as "might have a child" → KEEP), never
    /// reaping on doubt.
    fn has_live_child(&self, pane_pid: Option<u32>) -> bool;
}

/// A probe that always reports "no live child".
///
/// Why: the common case — a pane reporting a bare shell with no agent child —
/// and the default the production GC uses when the pane is already a shell. Most
/// unit tests pair it with idle pane commands to exercise the reap path.
/// What: [`has_live_child`](ChildLivenessProbe::has_live_child) always returns
/// `false`.
/// Test: used throughout the `tests` submodule.
pub struct AlwaysIdleProbe;

impl ChildLivenessProbe for AlwaysIdleProbe {
    fn has_live_child(&self, _pane_pid: Option<u32>) -> bool {
        false
    }
}

/// Classify a single tmux pane into a [`GcDecision`] — the heart of the GC.
///
/// Why: this is the one place the conservative orphan criterion lives, so it can
/// be audited and unit-tested exhaustively. A bug here risks killing live agent
/// work, so every gate fails CLOSED (errs toward KEEP).
/// What: a session is a [`GcDecision::ReapCandidate`] ONLY when ALL hold —
/// 1. its name carries a managed prefix ([`is_managed_session_name`]); AND
/// 2. it is absent from BOTH registries (`tracked`); AND
/// 3. it is genuinely idle: `pane_current_command` is a bare shell
///    ([`is_idle_shell`]) AND the `probe` finds no live agent child.
///
/// A managed, untracked, but *active* (non-shell) session yields
/// [`GcDecision::WarnUntrackedActive`] (keep + warn). Everything else is a
/// [`GcDecision::Keep`] with the precise [`KeepReason`].
/// Test: `classify_keeps_non_managed`, `classify_keeps_tracked_legacy`,
/// `classify_keeps_tracked_managed`, `classify_warns_untracked_active`,
/// `classify_reaps_untracked_idle`, `classify_keeps_live_child`.
pub fn classify_session(
    pane: &PaneInfo,
    tracked: &TrackedNames,
    probe: &dyn ChildLivenessProbe,
) -> GcDecision {
    // Gate 1: managed prefix. Anything not ours is untouchable.
    if !is_managed_session_name(&pane.session_name) {
        return GcDecision::Keep(KeepReason::NotManaged);
    }

    // Gate 2: tracked by either registry → keep. Order the checks so the more
    // specific reason wins for the audit log.
    if tracked.legacy.contains(&pane.session_name) {
        return GcDecision::Keep(KeepReason::TrackedLegacy);
    }
    if tracked.managed.contains(&pane.session_name) {
        return GcDecision::Keep(KeepReason::TrackedManaged);
    }
    debug_assert!(!tracked.contains(&pane.session_name));

    // Gate 3: idleness. A managed+untracked session running an agent is the
    // dangerous case — surface it loudly and KEEP it.
    if !is_idle_shell(&pane.pane_current_command) {
        return GcDecision::WarnUntrackedActive;
    }

    // Belt-and-braces: even a bare-shell pane is spared if a live agent child
    // is mid-spawn under it.
    if probe.has_live_child(pane.pane_pid) {
        return GcDecision::Keep(KeepReason::LiveChildProcess);
    }

    GcDecision::ReapCandidate
}

/// Outcome of planning one GC sweep (pure; no sessions killed yet).
///
/// Why: the caller wants to know what *would* be reaped (after debounce) versus
/// merely scanned, both for the per-pass `debug!` summary and so the async
/// runner can act on a plain data plan it can also unit-test.
/// What: `to_reap` is the debounced set of names to actually kill this pass;
/// `scanned`/`kept`/`warned` are counts for the summary log.
/// Test: `debounce_skips_first_observation`, `debounce_reaps_second_observation`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepPlan {
    /// Session names cleared by debounce and slated for reaping this pass.
    pub to_reap: Vec<String>,
    /// Total managed-prefix-or-not sessions scanned this pass.
    pub scanned: usize,
    /// Sessions kept (non-managed, tracked, or live-child).
    pub kept: usize,
    /// Untracked active managed sessions that were warned-and-kept.
    pub warned: usize,
}

/// Cross-pass orphan garbage-collector with a debounce.
///
/// Why: a session caught mid-spawn can momentarily look orphaned (managed name,
/// not yet recorded, pane briefly a shell). Reaping on first sight would race
/// the spawner and could kill a session a millisecond before it became tracked.
/// The debounce closes that race: a candidate must be observed orphaned on TWO
/// consecutive sweeps before it is reaped. This is the simpler, more robust of
/// the two debounce options (no wall-clock/age arithmetic, no dependence on
/// tmux's `session_created` accuracy) — a freshly-appeared candidate always gets
/// at least one full GC interval to become tracked.
/// What: holds `prev_candidates`, the set of names that were reap candidates on
/// the *previous* sweep. [`plan_sweep`](Self::plan_sweep) intersects this pass's
/// candidates with that set to decide what to actually reap, then records this
/// pass's candidates for next time.
/// Test: `debounce_skips_first_observation`, `debounce_reaps_second_observation`,
/// `debounce_resets_when_candidate_disappears`.
#[derive(Debug, Default)]
pub struct OrphanGc {
    /// Names that were reap candidates on the previous sweep.
    prev_candidates: HashSet<String>,
}

impl OrphanGc {
    /// Construct a fresh GC with an empty debounce history.
    ///
    /// Why: the daemon owns one long-lived `OrphanGc` across the process
    /// lifetime so debounce state persists between sweeps.
    /// What: returns a default-initialised instance.
    /// Test: used by every `debounce_*` test and the async runner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the previous pass's reap candidates, restarting the debounce.
    ///
    /// Why: when a sweep is aborted because the inputs are untrustworthy (a
    /// degraded tracked-names snapshot, or a tmux-list failure handled by the
    /// caller), the candidate set carried from the *previous* pass must NOT be
    /// allowed to clear the next pass's debounce — otherwise an orphan could be
    /// reaped on the first trustworthy sweep after a degraded one, collapsing the
    /// two-pass safety margin. Clearing the history forces any candidate to be
    /// observed orphaned on two consecutive *trustworthy* passes again.
    /// What: empties `prev_candidates`.
    /// Test: `reset_debounce_clears_prev_candidates` (pure) and the `run_sweep`
    /// degraded tests in `tests/orphan_gc_sweep.rs`.
    pub fn reset_debounce(&mut self) {
        self.prev_candidates.clear();
    }

    /// Plan one sweep over `panes`, applying the two-pass debounce.
    ///
    /// Why: keeping the planning pure (no tmux kills) makes the debounce logic
    /// unit-testable and lets the async runner act on a plain plan.
    /// What: classifies every pane; collects this pass's reap candidates;
    /// returns a [`SweepPlan`] whose `to_reap` is the intersection of this pass's
    /// candidates with the *previous* pass's candidates (the debounce). Updates
    /// `prev_candidates` to this pass's candidate set so the next sweep sees them.
    /// Tracked (healthy) sessions are counted in `kept` and logged at `debug!`.
    /// Untracked-active managed sessions are counted in `warned` and logged at
    /// `info!` (downgraded from `warn!` in #1813 — these are informational, not
    /// emergencies; frequent `warn!` here was filling logs at 56 MB/day).
    /// True anomalies (kill failures, degraded snapshots) remain at `warn!`.
    /// Test: `debounce_skips_first_observation`, `debounce_reaps_second_observation`,
    /// `gc_key_is_session_name_only_not_composite` (regression for #1813).
    pub fn plan_sweep(
        &mut self,
        panes: &[PaneInfo],
        tracked: &TrackedNames,
        probe: &dyn ChildLivenessProbe,
    ) -> SweepPlan {
        let mut plan = SweepPlan {
            scanned: panes.len(),
            ..SweepPlan::default()
        };
        let mut this_pass: HashSet<String> = HashSet::new();

        for pane in panes {
            match classify_session(pane, tracked, probe) {
                GcDecision::ReapCandidate => {
                    this_pass.insert(pane.session_name.clone());
                }
                GcDecision::WarnUntrackedActive => {
                    plan.warned += 1;
                    // Downgraded from warn! (#1813): an untracked active pane is
                    // informational — we intentionally skip it every sweep. Logging
                    // at warn! caused 56 MB/day of log growth for users with live
                    // managed sessions. Reserve warn! for genuine anomalies.
                    info!(
                        session = %pane.session_name,
                        command = %pane.pane_current_command,
                        "orphan-GC: untracked active managed session — skipping"
                    );
                }
                GcDecision::Keep(reason) => {
                    plan.kept += 1;
                    debug!(
                        session = %pane.session_name,
                        ?reason,
                        "orphan-GC: keeping tracked session"
                    );
                }
            }
        }

        // Debounce: only reap candidates seen on the PREVIOUS pass too.
        for name in &this_pass {
            if self.prev_candidates.contains(name) {
                plan.to_reap.push(name.clone());
            }
        }
        plan.to_reap.sort();

        // Remember this pass's candidates for next time's intersection.
        self.prev_candidates = this_pass;
        plan
    }
}

/// Real [`ChildLivenessProbe`] backed by the OS process tree via `pgrep`.
///
/// Why: the "no live agent child" gate is only a real safety net if it actually
/// inspects the process tree. A pane can momentarily report a bare shell while
/// an agent child is mid-spawn underneath it; reaping then would kill live work.
/// This probe asks the OS directly whether the pane's shell PID has any child.
/// What: runs `pgrep -P <pane_pid>` (the SAME mechanism
/// [`crate::core::process`] already uses to find `claude` children) and reports
/// a live child when the command exits `0` with at least one PID on stdout.
///
/// Cross-platform contract: `pgrep -P <pid>` is present and behaves identically
/// on both of our CI targets — macOS (BSD `pgrep`) and Linux (procps `pgrep`):
/// exit status `0` and one PID per line when children exist, exit status `1`
/// and empty stdout when none do. No other platform is supported or tested.
///
/// Fail-closed: a `None` PID returns `false` (nothing to inspect; the idleness
/// gate already classified the pane as a bare shell). For a present PID, ANY
/// uncertainty — `pgrep` missing, a spawn/IO error, or unparsable output — is
/// treated as "might have a child" and returns `true`, so the session is KEPT.
/// We never reap on probe uncertainty.
/// Test: `process_tree_probe_none_pid_is_idle` (the only `false` path) and
/// `process_tree_probe_self_has_no_managed_child`/`..._fails_closed` below.
pub struct ProcessTreeProbe;

impl ChildLivenessProbe for ProcessTreeProbe {
    fn has_live_child(&self, pane_pid: Option<u32>) -> bool {
        let Some(pid) = pane_pid else {
            // No process tree to inspect; the idleness gate already ran.
            return false;
        };
        pgrep_has_child(pid)
    }
}

/// True if `pid` has at least one live child, per `pgrep -P <pid>`.
///
/// Why: factored out of [`ProcessTreeProbe::has_live_child`] so the production
/// probe is a single thin spawn over the pure [`interpret_pgrep`] decision.
/// What: spawns `pgrep -P <pid>` and delegates the verdict to
/// [`interpret_pgrep`], which fails CLOSED (returns `true` → KEEP) on any
/// spawn/IO error.
/// Test: `pgrep_has_child_for_live_parent` exercises the live-spawn path; the
/// fail-closed and no-child branches are covered by [`interpret_pgrep`]'s tests.
fn pgrep_has_child(pid: u32) -> bool {
    let result = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output();
    interpret_pgrep(pid, result)
}

/// Decide whether `pgrep -P <pid>` output indicates a live child — fail-closed.
///
/// Why: isolating the verdict from the spawn keeps the safety-critical
/// fail-closed rule a pure function, unit-testable with synthesised outputs and
/// errors and with no process-global env mutation.
/// What: `Ok(output)` with a success exit and at least one parsable PID on
/// stdout → `true` (has a child). A clean non-zero exit (pgrep's exit-1
/// "no match") → `false` (the OS confidently reports no children). An `Err`
/// (pgrep missing or un-spawnable) → `true`, failing CLOSED so the caller KEEPs
/// the session rather than reaping on probe uncertainty.
/// Test: `interpret_pgrep_match`, `interpret_pgrep_no_match`,
/// `interpret_pgrep_fails_closed_on_error`.
fn interpret_pgrep(pid: u32, result: std::io::Result<std::process::Output>) -> bool {
    match result {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim().parse::<u32>().is_ok()),
        Ok(_) => false,
        Err(e) => {
            warn!(pid, error = %e, "orphan-GC: pgrep child probe failed; keeping session");
            true
        }
    }
}

/// Run one orphan-GC sweep against a live tmux driver, reaping debounced orphans.
///
/// Why: the daemon's periodic task and the one-shot startup sweep both need to
/// (1) enumerate live managed panes, (2) gather both registries' tracked names,
/// (3) plan with debounce, and (4) actually kill the cleared orphans — with
/// every kill logged at `info!` and the pass summarised at `debug!`.
/// What: takes the already-gathered `panes` and `tracked` sets plus a `driver`.
/// If `tracked.degraded` is set — a registry could not be fully read, so the
/// protected set is INCOMPLETE — the sweep SKIPS its reap phase entirely (reaps
/// nothing, logs at `warn!`) and resets the debounce so an orphan cannot be
/// reaped on the very next pass off a degraded snapshot; this mirrors the
/// existing "tmux list error → reap nothing" path. Otherwise it plans via
/// `gc.plan_sweep`, kills each `to_reap` name through the driver (logging each at
/// `info!` with its reason), and returns the count reaped. A failed kill is
/// logged at `warn!` and does not abort the sweep.
/// Test: `tests/orphan_gc_sweep.rs` drives this with a fake `ManagedTmuxDriver`
/// and asserts only the untracked-idle managed session is killed (and only on
/// the second pass), plus `sweep_skips_reap_on_degraded_snapshot` proving a
/// degraded snapshot reaps NOTHING even for an otherwise-reapable orphan.
pub fn run_sweep(
    gc: &mut OrphanGc,
    panes: &[PaneInfo],
    tracked: &TrackedNames,
    probe: &dyn ChildLivenessProbe,
    driver: &dyn crate::session_manager::ManagedTmuxDriver,
) -> usize {
    // Fail-closed: an incomplete protected set means a tracked session might be
    // missing from `tracked`, so its absence cannot be trusted as "untracked".
    // Skip the reap phase and clear the debounce so we never reap off a degraded
    // snapshot — neither this pass nor (via a stale candidate set) the next.
    if tracked.degraded {
        gc.reset_debounce();
        warn!(
            "orphan-GC: tracked-names snapshot incomplete (registry read failed); skipping reap this sweep"
        );
        return 0;
    }
    let plan = gc.plan_sweep(panes, tracked, probe);
    let mut reaped = 0usize;
    for name in &plan.to_reap {
        match driver.kill_session(name) {
            Ok(()) => {
                reaped += 1;
                info!(
                    session = %name,
                    reason = "untracked idle managed session (orphan-GC, debounced)",
                    "reaped orphaned tmux session"
                );
            }
            Err(e) => {
                warn!(session = %name, error = %e, "orphan-GC kill failed");
            }
        }
    }
    debug!(
        scanned = plan.scanned,
        kept = plan.kept,
        warned = plan.warned,
        candidates = plan.to_reap.len(),
        reaped,
        "orphan-GC sweep complete"
    );
    reaped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe that always claims a live child (forces KEEP via gate 3b).
    struct AlwaysLiveProbe;
    impl ChildLivenessProbe for AlwaysLiveProbe {
        fn has_live_child(&self, _pane_pid: Option<u32>) -> bool {
            true
        }
    }

    fn pane(name: &str, cmd: &str) -> PaneInfo {
        PaneInfo {
            session_name: name.to_string(),
            pane_current_command: cmd.to_string(),
            pane_pid: Some(4242),
        }
    }

    fn tracked_with(legacy: &[&str], managed: &[&str]) -> TrackedNames {
        TrackedNames {
            legacy: legacy.iter().map(|s| s.to_string()).collect(),
            managed: managed.iter().map(|s| s.to_string()).collect(),
            degraded: false,
        }
    }

    #[test]
    fn idle_shell_commands_recognised() {
        for c in [
            // POSIX-y shells (login + non-login forms).
            "zsh", "bash", "sh", "-zsh", "-bash", "-sh", //
            // Less-common shells a developer might run (added in #1458 review).
            "fish", "dash", "tcsh", "csh", "ksh", //
            "-fish", "-dash", "-tcsh", "-csh", "-ksh", //
            // The bare macOS login process, plus a whitespace-padded case.
            "login", "  zsh  ",
        ] {
            assert!(is_idle_shell(c), "{c:?} should be idle");
        }
    }

    #[test]
    fn process_tree_probe_none_pid_is_idle() {
        // A pane with no shell PID has no process tree to inspect; the idleness
        // gate already classified it as a bare shell, so report "no child".
        assert!(!ProcessTreeProbe.has_live_child(None));
    }

    #[test]
    fn pgrep_has_child_for_live_parent() {
        // The current test process spawns a short-lived `sleep` child; the real
        // pgrep probe must see it (proving the production probe is no longer a
        // no-op). If `pgrep` is unavailable the probe fails CLOSED (also `true`),
        // so this assertion holds on every supported CI target either way.
        use std::process::{Child, Command};

        // RAII guard: kills+reaps the child on drop, so neither a failed assert
        // (panic-unwind) nor an early return can ever leak the `sleep` process.
        struct ChildGuard(Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let child = Command::new("sleep")
            .arg("3")
            .spawn()
            .expect("spawn sleep child");
        let _guard = ChildGuard(child);

        let me = std::process::id();
        let saw_child = ProcessTreeProbe.has_live_child(Some(me));
        assert!(saw_child, "probe must see a live child of the test process");
        // `_guard` drops here (and on any panic above), killing+reaping `sleep`.
    }

    /// Build a synthetic `Output` with the given Unix exit code and stdout.
    #[cfg(unix)]
    fn fake_output(code: i32, stdout: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    #[cfg(unix)]
    fn interpret_pgrep_match() {
        // Exit 0 with a PID line = the parent has a live child → KEEP.
        assert!(interpret_pgrep(123, Ok(fake_output(0, "456\n"))));
    }

    #[test]
    #[cfg(unix)]
    fn interpret_pgrep_no_match() {
        // pgrep's exit-1 "no match" with empty stdout = no child → reapable.
        assert!(!interpret_pgrep(123, Ok(fake_output(1, ""))));
        // Defensive: even a "success" exit with no parsable PID is "no child".
        assert!(!interpret_pgrep(123, Ok(fake_output(0, "  \n"))));
    }

    #[test]
    fn interpret_pgrep_fails_closed_on_error() {
        // A spawn/IO error (e.g. pgrep not on PATH) must fail CLOSED → KEEP.
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "pgrep missing");
        assert!(
            interpret_pgrep(123, Err(err)),
            "probe error must keep the session, never reap on uncertainty"
        );
    }

    #[test]
    fn active_command_not_idle() {
        for c in [
            "claude",
            "node",
            "uv",
            "python",
            "vim",
            "claude-code",
            "ssh",
        ] {
            assert!(!is_idle_shell(c), "{c:?} should be active");
        }
    }

    #[test]
    fn classify_keeps_non_managed() {
        // (e) non-managed-prefix idle bare shell → KEEP (NotManaged).
        let d = classify_session(
            &pane("work", "zsh"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
        );
        assert_eq!(d, GcDecision::Keep(KeepReason::NotManaged));
    }

    #[test]
    fn classify_keeps_tracked_legacy() {
        // (b-ish) tracked in old DaemonState registry, even if idle → KEEP.
        let tracked = tracked_with(&["tmpm-tracked"], &[]);
        let d = classify_session(&pane("tmpm-tracked", "zsh"), &tracked, &AlwaysIdleProbe);
        assert_eq!(d, GcDecision::Keep(KeepReason::TrackedLegacy));
    }

    #[test]
    fn classify_keeps_tracked_managed() {
        // (b) tracked in new SessionManager store, even if idle → KEEP.
        let tracked = tracked_with(&[], &["tmpm-managed"]);
        let d = classify_session(&pane("tmpm-managed", "zsh"), &tracked, &AlwaysIdleProbe);
        assert_eq!(d, GcDecision::Keep(KeepReason::TrackedManaged));
    }

    #[test]
    fn classify_keeps_tracked_active() {
        // (a) tracked active session → KEEP (tracking wins before idleness).
        let tracked = tracked_with(&[], &["tmpm-busy"]);
        let d = classify_session(&pane("tmpm-busy", "claude"), &tracked, &AlwaysIdleProbe);
        assert_eq!(d, GcDecision::Keep(KeepReason::TrackedManaged));
    }

    #[test]
    fn classify_warns_untracked_active() {
        // (c) untracked active managed session → WARN + KEEP, never reaped.
        let d = classify_session(
            &pane("tmpm-rogue", "claude"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
        );
        assert_eq!(d, GcDecision::WarnUntrackedActive);
    }

    #[test]
    fn classify_reaps_untracked_idle() {
        // (d) untracked idle bare-shell managed session → the ONLY reap candidate.
        let d = classify_session(
            &pane("tmpm-orphan", "zsh"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
        );
        assert_eq!(d, GcDecision::ReapCandidate);
    }

    #[test]
    fn classify_keeps_live_child() {
        // Even an idle-looking managed+untracked pane is spared if a live agent
        // child is mid-spawn under it.
        let d = classify_session(
            &pane("tmpm-spawning", "zsh"),
            &TrackedNames::default(),
            &AlwaysLiveProbe,
        );
        assert_eq!(d, GcDecision::Keep(KeepReason::LiveChildProcess));
    }

    #[test]
    fn classify_legacy_prefix_is_managed() {
        // Legacy `trusty-mpm-` names are managed too and can be orphans.
        let d = classify_session(
            &pane("trusty-mpm-deadbeef", "bash"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
        );
        assert_eq!(d, GcDecision::ReapCandidate);
    }

    /// The full safety matrix in one sweep: only (d) is selected, and only after
    /// the debounce; (a)(b)(c)(e) are all kept; (c) is warned-and-kept.
    #[test]
    fn classify_full_mix_only_d_selected() {
        let panes = vec![
            pane("tmpm-a-tracked-active", "claude"), // (a) tracked active
            pane("tmpm-b-tracked-idle", "zsh"),      // (b) tracked idle
            pane("tmpm-c-untracked-active", "node"), // (c) untracked active
            pane("tmpm-d-untracked-idle", "zsh"),    // (d) untracked idle  <-- reap
            pane("plain-shell", "zsh"),              // (e) non-managed idle
        ];
        let tracked = tracked_with(&[], &["tmpm-a-tracked-active", "tmpm-b-tracked-idle"]);

        // First pass: (d) is a candidate but debounce holds it back.
        let mut gc = OrphanGc::new();
        let p1 = gc.plan_sweep(&panes, &tracked, &AlwaysIdleProbe);
        assert!(
            p1.to_reap.is_empty(),
            "debounce must spare a first-seen candidate, got {:?}",
            p1.to_reap
        );
        assert_eq!(p1.warned, 1, "exactly the (c) session is warned-and-kept");
        assert_eq!(p1.scanned, 5);

        // Second pass: (d) is still the only orphan → reaped; nothing else.
        let p2 = gc.plan_sweep(&panes, &tracked, &AlwaysIdleProbe);
        assert_eq!(p2.to_reap, vec!["tmpm-d-untracked-idle".to_string()]);
        assert_eq!(p2.warned, 1);
    }

    #[test]
    fn debounce_skips_first_observation() {
        // A freshly-appeared untracked-idle candidate is NOT reaped on first sight.
        let panes = vec![pane("tmpm-fresh", "zsh")];
        let mut gc = OrphanGc::new();
        let plan = gc.plan_sweep(&panes, &TrackedNames::default(), &AlwaysIdleProbe);
        assert!(plan.to_reap.is_empty());
    }

    #[test]
    fn debounce_reaps_second_observation() {
        // Observed orphaned on two consecutive passes → reaped on the second.
        let panes = vec![pane("tmpm-persist", "zsh")];
        let mut gc = OrphanGc::new();
        let _ = gc.plan_sweep(&panes, &TrackedNames::default(), &AlwaysIdleProbe);
        let plan = gc.plan_sweep(&panes, &TrackedNames::default(), &AlwaysIdleProbe);
        assert_eq!(plan.to_reap, vec!["tmpm-persist".to_string()]);
    }

    #[test]
    fn debounce_resets_when_candidate_disappears() {
        // If a name stops being a reap candidate (e.g. it became tracked) between
        // passes, the debounce clock restarts — it must be seen as a candidate on
        // two consecutive passes again before it can be reaped.
        let orphan = vec![pane("tmpm-flaky", "zsh")];
        let mut gc = OrphanGc::new();
        // Pass 1: orphan present (1st sighting).
        let _ = gc.plan_sweep(&orphan, &TrackedNames::default(), &AlwaysIdleProbe);
        // Pass 2: the pane is STILL present, but it is now tracked (the tracked
        // set changed, not the pane) — so it is no longer a reap candidate and
        // the debounce history is cleared of it. Nothing may be reaped this pass.
        let tracked = tracked_with(&[], &["tmpm-flaky"]);
        let p2 = gc.plan_sweep(&orphan, &tracked, &AlwaysIdleProbe);
        assert!(
            p2.to_reap.is_empty(),
            "a now-tracked pane must not be reaped, got {:?}",
            p2.to_reap
        );
        // Pass 3: it becomes an orphan candidate again — but this is a *fresh*
        // 1st sighting after the reset, so it still must not be reaped.
        let p3 = gc.plan_sweep(&orphan, &TrackedNames::default(), &AlwaysIdleProbe);
        assert!(
            p3.to_reap.is_empty(),
            "a candidate that lapsed must restart the debounce, got {:?}",
            p3.to_reap
        );
    }

    #[test]
    fn reset_debounce_clears_prev_candidates() {
        // After a first sighting records a candidate, an explicit reset must wipe
        // the debounce history so the very next sighting is treated as a *first*
        // one again (never reaped). This is the mechanism `run_sweep` uses to
        // fail closed on a degraded tracked-names snapshot.
        let orphan = vec![pane("tmpm-flaky", "zsh")];
        let mut gc = OrphanGc::new();
        // 1st sighting → candidate remembered, nothing reaped.
        let _ = gc.plan_sweep(&orphan, &TrackedNames::default(), &AlwaysIdleProbe);
        // Reset wipes the remembered candidate.
        gc.reset_debounce();
        // Next sighting is therefore a fresh 1st sighting → still not reaped.
        let after = gc.plan_sweep(&orphan, &TrackedNames::default(), &AlwaysIdleProbe);
        assert!(
            after.to_reap.is_empty(),
            "reset must restart the debounce, got {:?}",
            after.to_reap
        );
    }

    /// Regression test for #1813 (key-mismatch bug).
    ///
    /// The GC's lookup key MUST be the **session name alone**, never a composite
    /// formed from `{session_name}_{pane_command}_{pane_pid}`. A composite key
    /// (e.g. `tmpm-27f32eb2-ae6a-485b-a_zsh_45162`) would never match the tracked
    /// set's plain session-name entries and would cause every tracked session to be
    /// misclassified as `WarnUntrackedActive` — preventing any reaping forever while
    /// generating constant warn-level log spam.
    ///
    /// Specifically verifies three properties:
    /// (a) A tracked session running an active command is `Keep(TrackedManaged)`,
    ///     NOT `WarnUntrackedActive` — proving the command and PID are NOT part of
    ///     the lookup key.
    /// (b) A genuinely-orphaned session (untracked + idle shell) is `ReapCandidate`.
    /// (c) A tracked session running an idle shell is `Keep(TrackedManaged)`, NOT
    ///     `ReapCandidate`.
    #[test]
    fn gc_key_is_session_name_only_not_composite() {
        let tracked = tracked_with(&[], &["tmpm-my-session"]);

        // (a) Tracked session running an active command → must be kept, not warned.
        // The pane_current_command ("claude") and pane_pid (45162) must play NO
        // part in the lookup; only session_name determines membership in `tracked`.
        let decision = classify_session(
            &PaneInfo {
                session_name: "tmpm-my-session".to_string(),
                pane_current_command: "claude".to_string(),
                pane_pid: Some(45162),
            },
            &tracked,
            &AlwaysIdleProbe,
        );
        assert_eq!(
            decision,
            GcDecision::Keep(KeepReason::TrackedManaged),
            "(a) tracked active session must be Keep, not WarnUntrackedActive \
             (key mismatch regression: composite key would never match)"
        );

        // (b) Genuinely-orphaned session (untracked + idle bare shell) → ReapCandidate.
        let decision = classify_session(
            &PaneInfo {
                session_name: "tmpm-orphan".to_string(),
                pane_current_command: "zsh".to_string(),
                pane_pid: Some(99999),
            },
            &tracked,
            &AlwaysIdleProbe,
        );
        assert_eq!(
            decision,
            GcDecision::ReapCandidate,
            "(b) untracked idle managed session must be a ReapCandidate"
        );

        // (c) Tracked session running an idle shell → still kept, not reaped.
        let decision = classify_session(
            &PaneInfo {
                session_name: "tmpm-my-session".to_string(),
                pane_current_command: "zsh".to_string(),
                pane_pid: Some(55555),
            },
            &tracked,
            &AlwaysIdleProbe,
        );
        assert_eq!(
            decision,
            GcDecision::Keep(KeepReason::TrackedManaged),
            "(c) tracked idle session must be Keep, not ReapCandidate"
        );
    }
}
