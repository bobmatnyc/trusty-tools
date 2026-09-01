//! Orphan garbage-collection for managed tmux sessions.
//!
//! Why: the RAII [`crate::session_manager::TmuxSessionGuard`] foundation stops
//! *new* leaks — a session the daemon spawns is killed when its owning guard
//! drops. But it cannot clean up orphans that already exist: sessions leaked by
//! older daemon builds, by crashes before the guard landed, or by a `kill -9`
//! of the daemon. Those managed (`tm-*`/`tmpm-*`) tmux sessions accumulate
//! forever, drowning the dashboard and eventually the host. This module is the
//! self-healing other half: a conservative reaper that reconciles the live
//! `tmux ls` against BOTH
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
//! by `tests/orphan_gc_sweep.rs` against a fake [`ManagedTmuxDriver`](crate::session_manager::ManagedTmuxDriver).

use std::collections::{HashMap, HashSet};
use std::process::Command;

use tracing::{debug, info, warn};

use crate::core::names::is_managed_session_name;

/// How many sweeps pass between two "untracked active managed session" lines
/// for the SAME session, when nothing else changes.
///
/// Why (#6118): the previous code logged one line per warned session per sweep.
/// 478 permanently-warned sessions on a 60-second sweep produced 992,078 lines
/// in 48 hours — 76% of every daemon log line written. The set of warned
/// sessions is stable for hours at a time, so re-stating it every minute adds
/// no information after the first line.
/// What: a session is logged on its FIRST sweep as a warn candidate and then
/// once every `DEFAULT_SKIP_LOG_EVERY_SWEEPS` sweeps thereafter; the per-sweep
/// TOTAL stays in the sweep-complete summary either way. At the daemon's
/// 60-second cadence this is one line per session per hour.
/// Test: `skip_log_is_throttled_per_session`, `skip_log_repeats_after_n_sweeps`.
pub const DEFAULT_SKIP_LOG_EVERY_SWEEPS: u32 = 60;

/// Environment override for [`DEFAULT_SKIP_LOG_EVERY_SWEEPS`].
///
/// `0` or an unparsable value falls back to the default; `1` restores the
/// pre-#6118 log-every-sweep behavior.
pub const ENV_SKIP_LOG_EVERY_SWEEPS: &str = "TRUSTY_MPM_ORPHAN_GC_SKIP_LOG_EVERY";

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
/// function of plain data, fully testable without spawning tmux. The stable
/// tmux `pane_id` (#2789) additionally lets the reactivate-reconcile path
/// (`daemon::managed_routes::reactivate`) identify the ONE pane that belongs to
/// the caller making an in-place-relaunch request — see that module for why
/// that pane must be treated as idle rather than as a live runtime.
/// What: the tmux `session_name`, its `pane_current_command`, an optional
/// `pane_pid` (the pane's shell PID, `None` when tmux did not report one), and
/// an optional stable `pane_id` (tmux's `%N`, `None` when tmux did not report
/// one or an older enumeration path did not capture it).
/// Test: constructed throughout the `tests` submodule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    /// tmux session name (e.g. `tm-brave-otter`).
    pub session_name: String,
    /// The pane's foreground command (`claude`, `zsh`, `node`, …).
    pub pane_current_command: String,
    /// The pane's shell PID, if tmux reported it.
    pub pane_pid: Option<u32>,
    /// The pane's stable tmux id (e.g. `"%5"`), if the enumeration captured it.
    /// Distinct from `pane_pid` (which the OS can reuse across a pane's
    /// lifetime); never inherited across panes, unlike an env var.
    pub pane_id: Option<String>,
    /// The pane's working directory as tmux reports it, when the enumeration
    /// captured one (#6118).
    ///
    /// Why: the GC's only reap path used to be "bare shell, no live child",
    /// which never covers a pane whose worktree was deleted underneath it — the
    /// pane keeps running an agent, `reconcile` declines to adopt it because its
    /// cwd does not resolve, and nothing else can act. tmux still reports the
    /// path it recorded for the pane, so comparing that path against the
    /// filesystem is POSITIVE evidence the pane has nothing left to work in.
    /// What: `Some(path)` when tmux reported a non-empty `#{pane_current_path}`;
    /// `None` when it reported an empty field or the enumeration predates this
    /// column. `None` is NO evidence and can never select a pane for reaping.
    pub pane_current_path: Option<String>,
}

/// What the filesystem says about a pane's reported working directory (#6118).
///
/// Why (ADR-0045): "the directory is gone" and "I could not tell" are different
/// answers, and only the first is evidence for a kill. Collapsing them into a
/// bool is exactly the fail-open shape that ADR forbids on a destructive path.
///
/// ADR-0045's trigger table names the case that makes this more than a
/// formality: an unmounted or ejected volume returns plain ENOENT, NOT an error
/// kind that could be recognised as "undeterminable". An external disk carrying
/// a live agent's worktree therefore looks byte-for-byte like a deleted
/// worktree to `symlink_metadata` alone. [`FsCwdProbe`] separates them by asking
/// about the PARENT — see its doc.
/// What: three states. `Exists` and `Gone` are decided answers; `Undeterminable`
/// covers every case where the answer was not obtained, including tmux reporting
/// no path at all.
/// Test: `fs_cwd_probe_reports_exists_for_a_real_dir`,
/// `fs_cwd_probe_reports_gone_for_a_deleted_dir`,
/// `fs_cwd_probe_is_undeterminable_when_the_parent_is_gone_too`,
/// `classify_keeps_active_pane_when_cwd_is_undeterminable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdEvidence {
    /// The path tmux reported resolves to something on disk.
    Exists,
    /// The path tmux reported is confirmed ABSENT — the one reapable answer.
    Gone,
    /// No answer: tmux reported no path, or the filesystem could not say.
    Undeterminable,
}

/// A probe answering whether a pane's reported working directory still exists.
///
/// Why: keeps [`classify_session`] pure and lets tests decide the answer without
/// creating and deleting real directories, mirroring [`ChildLivenessProbe`].
/// What: one method mapping a non-empty path string to a [`CwdEvidence`].
/// Fail-closed contract: return [`CwdEvidence::Gone`] ONLY when the path's
/// absence is attributable to that path itself. An error that is not "not
/// found" — a permission denial, an I/O failure, a stale NFS handle — and an
/// absence that extends to the path's whole enclosing subtree both MUST return
/// [`CwdEvidence::Undeterminable`], which keeps the pane.
/// Test: [`FsCwdProbe`]'s tests below; the fakes in the `tests` submodule drive
/// the classifier.
pub trait CwdProbe {
    /// Classify `path` (guaranteed non-empty by the caller).
    fn evidence(&self, path: &str) -> CwdEvidence;
}

/// The production [`CwdProbe`], backed by `std::fs`.
///
/// Why: the reap decision must consult the real filesystem, and the
/// absent-vs-undeterminable split cannot be made from the `io::Error` kind
/// alone. ADR-0045's trigger table is explicit that an unmounted or ejected
/// volume answers plain ENOENT — the SAME answer a deleted worktree gives. A
/// probe that stopped at `NotFound` would kill an agent working on an external
/// disk about two minutes after someone ejected it, which is the opposite of
/// what #6118 asks for.
///
/// What separates the two is the PARENT. #6118's target is a worktree removed
/// from an intact `.claude/worktrees/`, so the parent survives. An ejected
/// volume, an unmounted share, and a deleted parent tree all take the parent
/// with them, and none of those is evidence about this pane's own directory.
/// What: `symlink_metadata` on the path — never `metadata`, which follows the
/// link and would call a dangling symlink "gone" when the pane's own entry is
/// very much there. `Ok` → [`CwdEvidence::Exists`]. `ErrorKind::NotFound` →
/// [`CwdEvidence::Gone`] only when the parent directory is itself present;
/// otherwise [`CwdEvidence::Undeterminable`]. A path with no parent, and every
/// other error kind, are [`CwdEvidence::Undeterminable`].
/// Test: `fs_cwd_probe_reports_exists_for_a_real_dir`,
/// `fs_cwd_probe_reports_gone_for_a_deleted_dir`,
/// `fs_cwd_probe_is_undeterminable_when_the_parent_is_gone_too`,
/// `fs_cwd_probe_is_undeterminable_on_a_non_notfound_error`.
pub struct FsCwdProbe;

impl CwdProbe for FsCwdProbe {
    fn evidence(&self, path: &str) -> CwdEvidence {
        match std::fs::symlink_metadata(path) {
            Ok(_) => CwdEvidence::Exists,
            // #6118: absent — but absent WHY? Only a surviving parent makes this
            // about this directory rather than about its whole volume.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let Some(parent) = std::path::Path::new(path).parent() else {
                    return CwdEvidence::Undeterminable;
                };
                if std::fs::symlink_metadata(parent).is_ok() {
                    CwdEvidence::Gone
                } else {
                    debug!(
                        path = %path,
                        "orphan-GC: a pane's cwd is absent and so is its parent — an unmounted \
                         volume answers exactly like a deleted directory (ADR-0045), so this is \
                         not evidence; keeping the pane (#6118)"
                    );
                    CwdEvidence::Undeterminable
                }
            }
            Err(e) => {
                warn!(
                    path = %path,
                    error = %e,
                    "orphan-GC: cannot determine whether a pane's cwd still exists; keeping the pane (#6118)"
                );
                CwdEvidence::Undeterminable
            }
        }
    }
}

/// Classify a pane's reported cwd, treating an unreported path as no evidence.
///
/// Why: `None` and `Some("")` both mean tmux told us nothing, and neither may
/// ever reach the filesystem probe as a path to test — an empty string would
/// resolve to the process cwd and answer `Exists` for a pane we know nothing
/// about.
/// What: returns [`CwdEvidence::Undeterminable`] for a missing or blank path;
/// otherwise delegates to `probe`.
/// Test: `pane_cwd_evidence_is_undeterminable_without_a_path`.
pub fn pane_cwd_evidence(pane: &PaneInfo, probe: &dyn CwdProbe) -> CwdEvidence {
    match pane.pane_current_path.as_deref().map(str::trim) {
        Some(p) if !p.is_empty() => probe.evidence(p),
        _ => CwdEvidence::Undeterminable,
    }
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
    /// The session is a reap candidate for a DIFFERENT reason (#6118): it is
    /// managed, untracked, and its pane's working directory is confirmed gone,
    /// even though the pane's foreground command is not a bare shell. Debounce
    /// gates this exactly as it gates [`Self::ReapCandidate`].
    ReapCandidateCwdGone,
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
/// A managed, untracked, *active* (non-shell) session whose pane's working
/// directory is CONFIRMED gone yields [`GcDecision::ReapCandidateCwdGone`]
/// (#6118) — the pane has no directory left to work in, `reconcile` has already
/// declined to adopt it for that reason, and nothing else could ever act on it.
/// Any other active untracked session yields [`GcDecision::WarnUntrackedActive`]
/// (keep + warn). Everything else is a [`GcDecision::Keep`] with the precise
/// [`KeepReason`].
///
/// The cwd gate is POSITIVE evidence only (ADR-0045): a pane whose path tmux
/// never reported, or whose existence the filesystem could not decide, is KEPT
/// exactly as before. Both reap candidates still pass through the same two-sweep
/// debounce in [`OrphanGc::plan_sweep`] before anything is killed.
/// Test: `classify_keeps_non_managed`, `classify_keeps_tracked_legacy`,
/// `classify_keeps_tracked_managed`, `classify_warns_untracked_active`,
/// `classify_reaps_untracked_idle`, `classify_keeps_live_child`,
/// `classify_reaps_active_pane_whose_cwd_is_gone`,
/// `classify_keeps_active_pane_when_cwd_is_undeterminable`,
/// `classify_keeps_active_pane_whose_cwd_exists`,
/// `classify_keeps_tracked_pane_whose_cwd_is_gone`.
pub fn classify_session(
    pane: &PaneInfo,
    tracked: &TrackedNames,
    probe: &dyn ChildLivenessProbe,
    cwd_probe: &dyn CwdProbe,
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
    // dangerous case — surface it loudly and KEEP it, UNLESS its working
    // directory is provably gone.
    if !is_idle_shell(&pane.pane_current_command) {
        // #6118: a declined-adopt pane (reconcile refused it because its cwd
        // does not resolve) stayed here forever — 478 of them, re-logged every
        // sweep. A confirmed-absent cwd is the positive evidence that makes it
        // reapable; anything less keeps the pre-#6118 warn-and-keep outcome.
        return match pane_cwd_evidence(pane, cwd_probe) {
            CwdEvidence::Gone => GcDecision::ReapCandidateCwdGone,
            CwdEvidence::Exists | CwdEvidence::Undeterminable => GcDecision::WarnUntrackedActive,
        };
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
    /// How many of `warned` actually produced a log line this sweep (#6118).
    /// The rest were suppressed by the per-session throttle.
    pub warn_logged: usize,
    /// Names in `to_reap` selected because their pane's cwd is gone, not
    /// because the pane is an idle shell (#6118). Drives the kill-log reason.
    pub cwd_gone: HashSet<String>,
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
    /// #6118: how many sweeps each currently-warned session has been warned
    /// for. Drives the per-session log throttle; entries for sessions that
    /// stopped being warned are dropped each sweep so the map cannot grow past
    /// the live warned set.
    warn_sweeps: HashMap<String, u32>,
    /// #6118: log a warned session on its first sweep and then once every this
    /// many sweeps. `0` is normalised to `1` (log every sweep) on construction.
    skip_log_every: u32,
}

impl OrphanGc {
    /// Construct a fresh GC with an empty debounce history.
    ///
    /// Why: the daemon owns one long-lived `OrphanGc` across the process
    /// lifetime so debounce state persists between sweeps.
    /// What: an empty debounce set and the default #6118 log throttle
    /// ([`DEFAULT_SKIP_LOG_EVERY_SWEEPS`]).
    /// Test: used by every `debounce_*` test and the async runner.
    pub fn new() -> Self {
        Self::with_skip_log_every(DEFAULT_SKIP_LOG_EVERY_SWEEPS)
    }

    /// Construct a GC whose #6118 log throttle comes from the process env.
    ///
    /// Why: the daemon's reap loop is the one production construction site, and
    /// it should read the override without spelling out the env plumbing there —
    /// `daemon/mod.rs` sits at its 500-SLOC cap.
    /// What: [`Self::with_skip_log_every`] over [`Self::skip_log_every_from_env`].
    /// Test: the parsing is covered by `skip_log_every_from_env_parsing`.
    pub fn from_env() -> Self {
        Self::with_skip_log_every(Self::skip_log_every_from_env(|k| std::env::var(k).ok()))
    }

    /// Construct a GC whose untracked-active log repeats every `sweeps` sweeps.
    ///
    /// Why (#6118): the throttle interval has to be settable so a test can
    /// assert both the suppression and the eventual repeat without running 60
    /// sweeps, and so an operator debugging a live host can restore the old
    /// every-sweep behavior through [`ENV_SKIP_LOG_EVERY_SWEEPS`].
    /// What: stores `sweeps`, normalising `0` to `1` — a modulus of zero has no
    /// meaning and silently disabling the log would hide the very sessions this
    /// line exists to surface.
    /// Test: `skip_log_is_throttled_per_session`, `skip_log_repeats_after_n_sweeps`,
    /// `skip_log_every_zero_logs_every_sweep`.
    pub fn with_skip_log_every(sweeps: u32) -> Self {
        Self {
            prev_candidates: HashSet::new(),
            warn_sweeps: HashMap::new(),
            skip_log_every: sweeps.max(1),
        }
    }

    /// Read the #6118 log-throttle interval from an injectable environment.
    ///
    /// Why: the daemon constructs its `OrphanGc` from the process environment,
    /// and threading a resolver keeps that reachable from a test without any
    /// process-wide `set_var`.
    /// What: parses [`ENV_SKIP_LOG_EVERY_SWEEPS`]; an absent, unparsable, or
    /// zero value yields [`DEFAULT_SKIP_LOG_EVERY_SWEEPS`].
    /// Test: `skip_log_every_from_env_parsing`.
    pub fn skip_log_every_from_env(get: impl Fn(&str) -> Option<String>) -> u32 {
        get(ENV_SKIP_LOG_EVERY_SWEEPS)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_SKIP_LOG_EVERY_SWEEPS)
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
    /// #6118 adds a second candidate class — a pane whose cwd is confirmed gone
    /// ([`GcDecision::ReapCandidateCwdGone`]) — through the SAME debounce, and
    /// throttles the untracked-active log to once per session per
    /// `skip_log_every` sweeps.
    /// Test: `debounce_skips_first_observation`, `debounce_reaps_second_observation`,
    /// `gc_key_is_session_name_only_not_composite` (regression for #1813);
    /// `cwd_gone_pane_is_debounced_then_reaped`, `skip_log_is_throttled_per_session`
    /// (#6118).
    pub fn plan_sweep(
        &mut self,
        panes: &[PaneInfo],
        tracked: &TrackedNames,
        probe: &dyn ChildLivenessProbe,
        cwd_probe: &dyn CwdProbe,
    ) -> SweepPlan {
        let mut plan = SweepPlan {
            scanned: panes.len(),
            ..SweepPlan::default()
        };
        let mut this_pass: HashSet<String> = HashSet::new();
        let mut warned_this_pass: HashMap<String, u32> = HashMap::new();

        for pane in panes {
            match classify_session(pane, tracked, probe, cwd_probe) {
                GcDecision::ReapCandidate => {
                    this_pass.insert(pane.session_name.clone());
                }
                GcDecision::ReapCandidateCwdGone => {
                    this_pass.insert(pane.session_name.clone());
                    plan.cwd_gone.insert(pane.session_name.clone());
                }
                GcDecision::WarnUntrackedActive => {
                    plan.warned += 1;
                    // #6118: the same ids were re-logged every 60s forever —
                    // 992,078 lines in 48h. Count the sweeps this session has
                    // been warned for and log only the 1st and every Nth.
                    let seen = self
                        .warn_sweeps
                        .get(pane.session_name.as_str())
                        .copied()
                        .unwrap_or(0);
                    warned_this_pass.insert(pane.session_name.clone(), seen + 1);
                    if seen % self.skip_log_every == 0 {
                        plan.warn_logged += 1;
                        // Downgraded from warn! (#1813): an untracked active pane is
                        // informational — we intentionally skip it every sweep. Logging
                        // at warn! caused 56 MB/day of log growth for users with live
                        // managed sessions. Reserve warn! for genuine anomalies.
                        info!(
                            session = %pane.session_name,
                            command = %pane.pane_current_command,
                            sweeps_skipped = seen,
                            repeats_every = self.skip_log_every,
                            "orphan-GC: untracked active managed session — skipping"
                        );
                    }
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
        // A cwd-gone name still held back by the debounce must not be reported
        // as a reason for a kill that is not happening.
        plan.cwd_gone.retain(|n| plan.to_reap.contains(n));

        // Remember this pass's candidates for next time's intersection, and
        // replace (never merge) the warn counters so a session that stopped
        // being warned is forgotten rather than accumulating forever.
        self.prev_candidates = this_pass;
        self.warn_sweeps = warned_this_pass;
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
    cwd_probe: &dyn CwdProbe,
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
    let plan = gc.plan_sweep(panes, tracked, probe, cwd_probe);
    let mut reaped = 0usize;
    for name in &plan.to_reap {
        // #6118: name the evidence that selected this pane — an idle shell and
        // a vanished working directory are different kills to audit.
        let reason = if plan.cwd_gone.contains(name) {
            "untracked managed session whose working directory is gone (orphan-GC, debounced, #6118)"
        } else {
            "untracked idle managed session (orphan-GC, debounced)"
        };
        match driver.kill_session(name) {
            Ok(()) => {
                reaped += 1;
                info!(session = %name, reason, "reaped orphaned tmux session");
            }
            Err(e) => {
                warn!(session = %name, error = %e, "orphan-GC kill failed");
            }
        }
    }
    debug!(
        scanned = plan.scanned,
        kept = plan.kept,
        // #6118: the per-session lines are throttled, so the aggregate is the
        // only place the full warned count is still stated every sweep.
        warned = plan.warned,
        warn_logged = plan.warn_logged,
        candidates = plan.to_reap.len(),
        cwd_gone = plan.cwd_gone.len(),
        reaped,
        "orphan-GC sweep complete"
    );
    reaped
}

/// [`run_sweep`] with the production filesystem cwd probe (#6118).
///
/// Why: the daemon's reap loop is the only production caller and always wants
/// [`FsCwdProbe`]; naming it at the call site cost `daemon/mod.rs` five lines it
/// does not have against its 500-SLOC cap. Tests keep using `run_sweep` so they
/// can inject a deterministic probe.
/// What: forwards every argument, supplying [`FsCwdProbe`].
/// Test: `tests/orphan_gc_sweep.rs` covers the wrapped function directly.
pub fn run_sweep_fs(
    gc: &mut OrphanGc,
    panes: &[PaneInfo],
    tracked: &TrackedNames,
    probe: &dyn ChildLivenessProbe,
    driver: &dyn crate::session_manager::ManagedTmuxDriver,
) -> usize {
    run_sweep(gc, panes, tracked, probe, &FsCwdProbe, driver)
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

    /// A cwd probe that never answers — the pre-#6118 world, where nothing is
    /// known about any pane's working directory.
    struct NoCwdEvidence;
    impl CwdProbe for NoCwdEvidence {
        fn evidence(&self, _path: &str) -> CwdEvidence {
            CwdEvidence::Undeterminable
        }
    }

    /// A cwd probe that reports every path as CONFIRMED absent.
    struct AllCwdGone;
    impl CwdProbe for AllCwdGone {
        fn evidence(&self, _path: &str) -> CwdEvidence {
            CwdEvidence::Gone
        }
    }

    /// A cwd probe that reports every path as present.
    struct AllCwdExists;
    impl CwdProbe for AllCwdExists {
        fn evidence(&self, _path: &str) -> CwdEvidence {
            CwdEvidence::Exists
        }
    }

    fn pane(name: &str, cmd: &str) -> PaneInfo {
        PaneInfo {
            session_name: name.to_string(),
            pane_current_command: cmd.to_string(),
            pane_pid: Some(4242),
            pane_id: None,
            pane_current_path: None,
        }
    }

    /// A pane that reports a working directory (#6118).
    fn pane_at(name: &str, cmd: &str, path: &str) -> PaneInfo {
        PaneInfo {
            pane_current_path: Some(path.to_string()),
            ..pane(name, cmd)
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
            &NoCwdEvidence,
        );
        assert_eq!(d, GcDecision::Keep(KeepReason::NotManaged));
    }

    #[test]
    fn classify_keeps_tracked_legacy() {
        // (b-ish) tracked in old DaemonState registry, even if idle → KEEP.
        let tracked = tracked_with(&["tmpm-tracked"], &[]);
        let d = classify_session(
            &pane("tmpm-tracked", "zsh"),
            &tracked,
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert_eq!(d, GcDecision::Keep(KeepReason::TrackedLegacy));
    }

    #[test]
    fn classify_keeps_tracked_managed() {
        // (b) tracked in new SessionManager store, even if idle → KEEP.
        let tracked = tracked_with(&[], &["tmpm-managed"]);
        let d = classify_session(
            &pane("tmpm-managed", "zsh"),
            &tracked,
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert_eq!(d, GcDecision::Keep(KeepReason::TrackedManaged));
    }

    #[test]
    fn classify_keeps_tracked_active() {
        // (a) tracked active session → KEEP (tracking wins before idleness).
        let tracked = tracked_with(&[], &["tmpm-busy"]);
        let d = classify_session(
            &pane("tmpm-busy", "claude"),
            &tracked,
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert_eq!(d, GcDecision::Keep(KeepReason::TrackedManaged));
    }

    #[test]
    fn classify_warns_untracked_active() {
        // (c) untracked active managed session → WARN + KEEP, never reaped.
        let d = classify_session(
            &pane("tmpm-rogue", "claude"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
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
            &NoCwdEvidence,
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
            &NoCwdEvidence,
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
            &NoCwdEvidence,
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
        let p1 = gc.plan_sweep(&panes, &tracked, &AlwaysIdleProbe, &NoCwdEvidence);
        assert!(
            p1.to_reap.is_empty(),
            "debounce must spare a first-seen candidate, got {:?}",
            p1.to_reap
        );
        assert_eq!(p1.warned, 1, "exactly the (c) session is warned-and-kept");
        assert_eq!(p1.scanned, 5);

        // Second pass: (d) is still the only orphan → reaped; nothing else.
        let p2 = gc.plan_sweep(&panes, &tracked, &AlwaysIdleProbe, &NoCwdEvidence);
        assert_eq!(p2.to_reap, vec!["tmpm-d-untracked-idle".to_string()]);
        assert_eq!(p2.warned, 1);
    }

    // ---------------------------------------------------------------------
    // #6118 — reaping a pane whose working directory is gone.
    // ---------------------------------------------------------------------

    /// The gap this issue reopened for: an untracked pane running an agent whose
    /// worktree was deleted underneath it. `reconcile` declines to adopt it
    /// (its cwd does not resolve), and before this fix `classify_session`
    /// answered `WarnUntrackedActive` forever — 478 permanent zombies. RED
    /// before the fix: the pre-fix classifier had no cwd input at all.
    #[test]
    fn classify_reaps_active_pane_whose_cwd_is_gone() {
        let d = classify_session(
            &pane_at("tm-zombie", "claude", "/gone/worktree"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdGone,
        );
        assert_eq!(
            d,
            GcDecision::ReapCandidateCwdGone,
            "a confirmed-gone cwd is positive evidence an untracked pane has \
             nothing left to work in (#6118)"
        );
    }

    /// ADR-0045: an undeterminable answer is not an absent one. A pane whose cwd
    /// the probe could not decide keeps the pre-#6118 warn-and-keep outcome.
    #[test]
    fn classify_keeps_active_pane_when_cwd_is_undeterminable() {
        let d = classify_session(
            &pane_at("tm-unknown", "claude", "/maybe/here"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert_eq!(d, GcDecision::WarnUntrackedActive);
    }

    /// A pane whose cwd is present is ordinary live work — never a candidate.
    #[test]
    fn classify_keeps_active_pane_whose_cwd_exists() {
        let d = classify_session(
            &pane_at("tm-busy", "claude", "/real/dir"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdExists,
        );
        assert_eq!(d, GcDecision::WarnUntrackedActive);
    }

    /// The #4091-family protection the cwd gate must never weaken: tracking wins
    /// before any idleness or cwd reasoning. A TRACKED session whose cwd is gone
    /// stays kept — the store, not the GC, owns that record's fate.
    #[test]
    fn classify_keeps_tracked_pane_whose_cwd_is_gone() {
        let tracked = tracked_with(&[], &["tm-tracked-gone"]);
        let d = classify_session(
            &pane_at("tm-tracked-gone", "claude", "/gone"),
            &tracked,
            &AlwaysIdleProbe,
            &AllCwdGone,
        );
        assert_eq!(d, GcDecision::Keep(KeepReason::TrackedManaged));
    }

    /// A non-managed pane is untouchable whatever its cwd says.
    #[test]
    fn classify_keeps_non_managed_pane_whose_cwd_is_gone() {
        let d = classify_session(
            &pane_at("my-own-work", "vim", "/gone"),
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdGone,
        );
        assert_eq!(d, GcDecision::Keep(KeepReason::NotManaged));
    }

    /// Fail-open check: tmux reporting no path must never reach the filesystem
    /// probe, because an empty path would resolve to the process cwd and answer
    /// `Exists` for a pane we know nothing about.
    #[test]
    fn pane_cwd_evidence_is_undeterminable_without_a_path() {
        assert_eq!(
            pane_cwd_evidence(&pane("tm-a", "claude"), &AllCwdGone),
            CwdEvidence::Undeterminable
        );
        assert_eq!(
            pane_cwd_evidence(&pane_at("tm-a", "claude", "   "), &AllCwdGone),
            CwdEvidence::Undeterminable
        );
    }

    /// The real probe against the real filesystem — the absent/present split.
    #[test]
    fn fs_cwd_probe_reports_exists_for_a_real_dir() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        assert_eq!(
            FsCwdProbe.evidence(&tmp.path().to_string_lossy()),
            CwdEvidence::Exists
        );
    }

    #[test]
    fn fs_cwd_probe_reports_gone_for_a_deleted_dir() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("worktree");
        std::fs::create_dir(&path).expect("create");
        assert_eq!(
            FsCwdProbe.evidence(&path.to_string_lossy()),
            CwdEvidence::Exists
        );
        std::fs::remove_dir(&path).expect("remove");
        assert_eq!(
            FsCwdProbe.evidence(&path.to_string_lossy()),
            CwdEvidence::Gone,
            "a deleted worktree inside an intact parent is the exact live \
             condition #6118 reports"
        );
    }

    /// ADR-0045's first trigger row: an unmounted or ejected volume answers
    /// plain ENOENT, indistinguishable from a deleted directory by error kind
    /// alone. The surviving-parent gate is what separates them — an ejected disk
    /// takes the parent with it, a removed worktree does not.
    ///
    /// Input this reproduces: an untracked pane running `claude` with its cwd on
    /// an external volume, ejected. Without the gate the agent is killed about
    /// two sweeps (~2 minutes) later.
    /// Test: this is the test. RED before the parent gate: `Gone`.
    #[test]
    fn fs_cwd_probe_is_undeterminable_when_the_parent_is_gone_too() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let mount = tmp.path().join("Volumes-Ext");
        let workdir = mount.join("work").join("repo");
        std::fs::create_dir_all(&workdir).expect("create");
        assert_eq!(
            FsCwdProbe.evidence(&workdir.to_string_lossy()),
            CwdEvidence::Exists
        );

        // The whole subtree disappears at once, as an eject does.
        std::fs::remove_dir_all(&mount).expect("eject");
        assert_eq!(
            FsCwdProbe.evidence(&workdir.to_string_lossy()),
            CwdEvidence::Undeterminable,
            "an absent path whose parent is also absent is not evidence about \
             that path — it is an unmounted volume (ADR-0045)"
        );
    }

    /// The `Err(other)` arm: an error that is not `NotFound` must keep the pane.
    ///
    /// What: probing `<regular file>/child` yields ENOTDIR on every supported
    /// target — a non-`NotFound` error reachable without root or a real mount.
    #[test]
    fn fs_cwd_probe_is_undeterminable_on_a_non_notfound_error() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let file = tmp.path().join("not-a-dir");
        std::fs::write(&file, b"x").expect("write");
        let under_a_file = file.join("child");
        assert_eq!(
            FsCwdProbe.evidence(&under_a_file.to_string_lossy()),
            CwdEvidence::Undeterminable,
            "an unexpected errno must never read as absence"
        );
    }

    /// A path with no parent cannot be gated, so it is never evidence.
    #[test]
    fn fs_cwd_probe_is_undeterminable_for_a_parentless_path() {
        assert_eq!(FsCwdProbe.evidence("/"), CwdEvidence::Exists);
        // A bare relative name has an empty parent, which does not stat.
        assert_eq!(
            FsCwdProbe.evidence("definitely-not-here-6118"),
            CwdEvidence::Undeterminable
        );
    }

    /// A cwd-gone pane earns the SAME two-sweep confirmation an idle-shell
    /// orphan does before anything is killed — a pane running a live foreground
    /// process is never reaped on one observation.
    #[test]
    fn cwd_gone_pane_is_debounced_then_reaped() {
        let panes = vec![pane_at("tm-zombie", "claude", "/gone")];
        let mut gc = OrphanGc::new();

        let p1 = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdGone,
        );
        assert!(
            p1.to_reap.is_empty(),
            "first sighting must never kill a pane with a live foreground process"
        );

        let p2 = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdGone,
        );
        assert_eq!(p2.to_reap, vec!["tm-zombie".to_string()]);
        assert!(
            p2.cwd_gone.contains("tm-zombie"),
            "the kill must be attributed to the cwd-gone reason, not to idleness"
        );
    }

    /// A cwd-gone candidate whose directory reappears between sweeps restarts
    /// the debounce, exactly as a lapsed idle-shell candidate does.
    #[test]
    fn cwd_gone_debounce_resets_when_the_directory_returns() {
        let panes = vec![pane_at("tm-flaky", "claude", "/maybe")];
        let mut gc = OrphanGc::new();
        let _ = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdGone,
        );
        let p2 = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdExists,
        );
        assert!(p2.to_reap.is_empty(), "got {:?}", p2.to_reap);
        let p3 = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdGone,
        );
        assert!(
            p3.to_reap.is_empty(),
            "a lapsed candidate must restart the debounce, got {:?}",
            p3.to_reap
        );
    }

    /// `cwd_gone` names only what is ACTUALLY being killed this sweep, so the
    /// kill log can never attribute a reason to a pane it is not touching.
    #[test]
    fn cwd_gone_reason_is_dropped_while_the_debounce_holds() {
        let panes = vec![pane_at("tm-zombie", "claude", "/gone")];
        let mut gc = OrphanGc::new();
        let p1 = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &AllCwdGone,
        );
        assert!(p1.cwd_gone.is_empty(), "got {:?}", p1.cwd_gone);
    }

    // ---------------------------------------------------------------------
    // #6118 — throttling the untracked-active skip log.
    // ---------------------------------------------------------------------

    /// The log flood: the same warned session used to emit one line per sweep.
    /// RED before the fix — `warn_logged` did not exist and every sweep logged.
    #[test]
    fn skip_log_is_throttled_per_session() {
        let panes = vec![pane("tm-warned", "claude")];
        let mut gc = OrphanGc::with_skip_log_every(3);
        let p1 = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert_eq!(p1.warned, 1);
        assert_eq!(p1.warn_logged, 1, "the first sighting is always logged");
        for sweep in 2..=3 {
            let p = gc.plan_sweep(
                &panes,
                &TrackedNames::default(),
                &AlwaysIdleProbe,
                &NoCwdEvidence,
            );
            assert_eq!(p.warned, 1, "sweep {sweep} still counts the session");
            assert_eq!(p.warn_logged, 0, "sweep {sweep} must be suppressed");
        }
    }

    #[test]
    fn skip_log_repeats_after_n_sweeps() {
        let panes = vec![pane("tm-warned", "claude")];
        let mut gc = OrphanGc::with_skip_log_every(3);
        for _ in 0..3 {
            let _ = gc.plan_sweep(
                &panes,
                &TrackedNames::default(),
                &AlwaysIdleProbe,
                &NoCwdEvidence,
            );
        }
        let p4 = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert_eq!(
            p4.warn_logged, 1,
            "the line must reappear every N sweeps so a zombie is never invisible"
        );
    }

    /// A zero interval would make the modulus meaningless and could silence the
    /// line entirely; it is normalised to log-every-sweep instead.
    #[test]
    fn skip_log_every_zero_logs_every_sweep() {
        let panes = vec![pane("tm-warned", "claude")];
        let mut gc = OrphanGc::with_skip_log_every(0);
        for _ in 0..3 {
            let p = gc.plan_sweep(
                &panes,
                &TrackedNames::default(),
                &AlwaysIdleProbe,
                &NoCwdEvidence,
            );
            assert_eq!(p.warn_logged, 1);
        }
    }

    /// The throttle map must not grow without bound: a session that stops being
    /// warned is forgotten, and its next appearance logs as a first sighting.
    #[test]
    fn skip_log_counter_resets_when_a_session_stops_being_warned() {
        let warned = vec![pane("tm-warned", "claude")];
        let mut gc = OrphanGc::with_skip_log_every(10);
        let _ = gc.plan_sweep(
            &warned,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        // The session becomes tracked, so it is no longer warned at all.
        let tracked = tracked_with(&[], &["tm-warned"]);
        let _ = gc.plan_sweep(&warned, &tracked, &AlwaysIdleProbe, &NoCwdEvidence);
        // It goes untracked again — a fresh first sighting, logged.
        let p3 = gc.plan_sweep(
            &warned,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert_eq!(p3.warn_logged, 1);
    }

    #[test]
    fn skip_log_every_from_env_parsing() {
        assert_eq!(
            OrphanGc::skip_log_every_from_env(|_| None),
            DEFAULT_SKIP_LOG_EVERY_SWEEPS
        );
        assert_eq!(
            OrphanGc::skip_log_every_from_env(|_| Some("  7 ".to_string())),
            7
        );
        // Zero and garbage both fall back rather than silencing the line.
        assert_eq!(
            OrphanGc::skip_log_every_from_env(|_| Some("0".to_string())),
            DEFAULT_SKIP_LOG_EVERY_SWEEPS
        );
        assert_eq!(
            OrphanGc::skip_log_every_from_env(|_| Some("nope".to_string())),
            DEFAULT_SKIP_LOG_EVERY_SWEEPS
        );
    }

    #[test]
    fn debounce_skips_first_observation() {
        // A freshly-appeared untracked-idle candidate is NOT reaped on first sight.
        let panes = vec![pane("tmpm-fresh", "zsh")];
        let mut gc = OrphanGc::new();
        let plan = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert!(plan.to_reap.is_empty());
    }

    #[test]
    fn debounce_reaps_second_observation() {
        // Observed orphaned on two consecutive passes → reaped on the second.
        let panes = vec![pane("tmpm-persist", "zsh")];
        let mut gc = OrphanGc::new();
        let _ = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        let plan = gc.plan_sweep(
            &panes,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
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
        let _ = gc.plan_sweep(
            &orphan,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        // Pass 2: the pane is STILL present, but it is now tracked (the tracked
        // set changed, not the pane) — so it is no longer a reap candidate and
        // the debounce history is cleared of it. Nothing may be reaped this pass.
        let tracked = tracked_with(&[], &["tmpm-flaky"]);
        let p2 = gc.plan_sweep(&orphan, &tracked, &AlwaysIdleProbe, &NoCwdEvidence);
        assert!(
            p2.to_reap.is_empty(),
            "a now-tracked pane must not be reaped, got {:?}",
            p2.to_reap
        );
        // Pass 3: it becomes an orphan candidate again — but this is a *fresh*
        // 1st sighting after the reset, so it still must not be reaped.
        let p3 = gc.plan_sweep(
            &orphan,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
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
        let _ = gc.plan_sweep(
            &orphan,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        // Reset wipes the remembered candidate.
        gc.reset_debounce();
        // Next sighting is therefore a fresh 1st sighting → still not reaped.
        let after = gc.plan_sweep(
            &orphan,
            &TrackedNames::default(),
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
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
                pane_id: None,
                pane_current_path: None,
            },
            &tracked,
            &AlwaysIdleProbe,
            &NoCwdEvidence,
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
                pane_id: None,
                pane_current_path: None,
            },
            &tracked,
            &AlwaysIdleProbe,
            &NoCwdEvidence,
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
                pane_id: None,
                pane_current_path: None,
            },
            &tracked,
            &AlwaysIdleProbe,
            &NoCwdEvidence,
        );
        assert_eq!(
            decision,
            GcDecision::Keep(KeepReason::TrackedManaged),
            "(c) tracked idle session must be Keep, not ReapCandidate"
        );
    }

    /// The same three properties, driven through the REAL listing parser (#6529).
    ///
    /// Why: `gc_key_is_session_name_only_not_composite` above builds every
    /// `PaneInfo` from a literal, so it proved the classifier while the actual
    /// pane rows arrived joined into one field for days. A guard that never
    /// touches the parse cannot see a parse regression, which is precisely how
    /// #6529 stayed invisible after #1813 closed.
    /// What: feeds a verbatim `list-panes -a -F` listing through
    /// [`crate::daemon::tmux::TmuxDriver::parse_managed_pane_rows`], then
    /// classifies the rows it produces. Also asserts the tmux-sanitized form of
    /// the same listing yields no panes — never a pane named for the whole row.
    /// Test: this is the test. RED before the fix on the sanitized half.
    #[test]
    fn gc_classifies_rows_that_came_through_the_real_parse() {
        use crate::daemon::tmux::TmuxDriver;

        let tracked = tracked_with(&[], &["tm-trusty-tools-01"]);
        let listing = "tm-trusty-tools-01\tclaude\t10302\t%2306\t/Users/masa/trusty-tools\n\
                       tm-00b14f3b-dd31-4474-9\tzsh\t74149\t%1860\t/tmp/gone\n\
                       tm-declined-adopt-01\tclaude\t81234\t%1900\t/tmp/gone\n";
        let panes = TmuxDriver::parse_managed_pane_rows(listing);
        assert_eq!(panes.len(), 3, "every row must parse: {panes:?}");

        assert_eq!(
            classify_session(&panes[0], &tracked, &AlwaysIdleProbe, &NoCwdEvidence),
            GcDecision::Keep(KeepReason::TrackedManaged),
            "a tracked session must be kept even when its pane runs an agent"
        );
        assert_eq!(
            classify_session(&panes[1], &tracked, &AlwaysIdleProbe, &NoCwdEvidence),
            GcDecision::ReapCandidate,
            "an untracked idle managed session parsed from a real row must be \
             reapable — the live failure was that it never could be (#6529)"
        );
        // #6118: the third row is the declined-adopt zombie — an agent pane
        // whose worktree is gone. Its cwd must survive the parse and reach the
        // classifier as the evidence that selects it.
        assert_eq!(
            classify_session(&panes[2], &tracked, &AlwaysIdleProbe, &AllCwdGone),
            GcDecision::ReapCandidateCwdGone,
            "a real row's cwd column must carry through to the cwd-gone gate (#6118)"
        );

        // The tmux-sanitized shape of the SAME listing: no panes, so nothing is
        // classified at all rather than one bogus always-busy session per row.
        let sanitized = "tm-trusty-tools-01_claude_10302_%2306\n\
                         tm-00b14f3b-dd31-4474-9_zsh_74149_%1860\n";
        assert!(
            TmuxDriver::parse_managed_pane_rows(sanitized).is_empty(),
            "a delimiter-stripped listing must yield no panes (#6529)"
        );
    }
}
