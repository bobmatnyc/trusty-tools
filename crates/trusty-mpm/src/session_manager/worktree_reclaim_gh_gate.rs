//! Decides WHETHER the reclaim survey may spawn a `gh` poll at all (#6867).
//!
//! Why: [`super::worktree_reclaim_gh::run_with_timeout`] bounds one call; it
//! cannot see the call before it or the ten after. On 2026-09-04 a wedged
//! `securityd` made every `gh pr list` hang, and the survey kept firing new
//! polls roughly every ten seconds — each one blocked, each one killed at its
//! own ten-second ceiling, none of them ever answering. Two rules stop that,
//! and neither can be expressed inside a single call:
//!
//! 1. SINGLE-FLIGHT. A second poll for a call already outstanding spawns no
//!    process; it blocks on a condition variable and takes the first call's
//!    result. Keyed by registry root AND by the exact call (the bulk index, or
//!    one named branch), because two branches do not have the same answer —
//!    handing branch B the JSON fetched for branch A would be a correctness
//!    bug, not a saving.
//! 2. BACKOFF. After [`TIMEOUT_STRIKES`] consecutive TIMEOUTS for one registry
//!    root, polls for that root are skipped for a doubling interval, and the
//!    skip's reason is the operator-facing sentence. Only a timeout counts: an
//!    exit-4 auth failure answers instantly and is not the hang this guards, so
//!    any answer at all — success or a definite error — clears the strikes.
//!
//! The suspension reason travels the channel #6561 already built:
//! `BranchPrState::LookupFailed` → `ReclaimSurvey::lookup_failure` → the
//! `worktree_disk` doctor check's "the pull-request lookup FAILED for N
//! worktree(s) — …" clause, which renders it verbatim. No new doctor plumbing.
//!
//! Test: `worktree_reclaim_gh_gate_tests`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use super::worktree_reclaim_gh::GhFailure;

/// How many consecutive timeouts for one registry root suspend its polling.
///
/// Why: one timeout is a slow network; three in a row with nothing in between
/// is a wedged dependency, and continuing to poll it is what produced ~200
/// orphaned process pairs.
pub(crate) const TIMEOUT_STRIKES: u32 = 3;

/// The first suspension interval, doubled per additional timeout.
const BACKOFF_BASE: Duration = Duration::from_secs(60);

/// The ceiling the doubling saturates at — long enough to stop the leak, short
/// enough that a recovered host resumes reclaiming without a daemon restart.
const BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);

/// How long a root stays suspended after `consecutive` timeouts.
///
/// Why: growing, because a dependency that has failed six times running is
/// less likely to answer the seventh poll than the fourth. Capped, because a
/// suspension nobody lifts is a permanently disabled feature.
/// What: `BACKOFF_BASE * 2^(consecutive - TIMEOUT_STRIKES)`, saturating at
/// [`BACKOFF_MAX`]. The shift count is clamped so it can never overflow.
/// Test: `backoff_grows_and_then_saturates`.
fn backoff_for(consecutive: u32) -> Duration {
    let steps = consecutive.saturating_sub(TIMEOUT_STRIKES).min(16);
    let secs = BACKOFF_BASE.as_secs().saturating_mul(1u64 << steps);
    Duration::from_secs(secs.min(BACKOFF_MAX.as_secs()))
}

/// A wall-clock time an operator can compare against their own clock.
///
/// Why: "next retry in 480s" makes the reader do arithmetic against a log line
/// whose own timestamp they must also find. `Instant` cannot be formatted, so
/// the deadline is recorded in both clocks.
fn format_local(at: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(at)
        .format("%H:%M:%S %Z")
        .to_string()
}

/// One outstanding-or-recently-finished call.
///
/// `last` is populated ONLY while at least one waiter is parked on it: a
/// finished call with nobody waiting drops its stdout immediately, so the map
/// never accumulates 400-PR JSON payloads for the daemon's lifetime.
#[derive(Default)]
struct CallSlot {
    running: bool,
    waiters: usize,
    /// Incremented on every completion, so a waiter can tell "the call I
    /// waited for finished" from a spurious condition-variable wakeup.
    seq: u64,
    last: Option<Result<String, GhFailure>>,
}

impl CallSlot {
    /// Nothing is running and nobody is waiting, so the entry can be dropped.
    fn is_idle(&self) -> bool {
        !self.running && self.waiters == 0
    }
}

/// One registry root's consecutive-timeout record.
#[derive(Default)]
struct RootBackoff {
    consecutive_timeouts: u32,
    /// The deadline in both clocks — monotonic for the comparison, wall-clock
    /// for the message.
    suspended_until: Option<(Instant, SystemTime)>,
}

#[derive(Default)]
struct GateState {
    calls: HashMap<String, CallSlot>,
    roots: HashMap<PathBuf, RootBackoff>,
}

/// The single-flight and backoff gate every daemon `gh` poll passes through.
///
/// Why: see the module doc. Kept as a STRUCT rather than a set of free
/// functions over a static so its whole behaviour is testable on a local
/// instance — the tests build their own gate and never touch the process-wide
/// one, which is what makes the backoff test deterministic.
/// What: a mutex over the per-call and per-root state, plus the condition
/// variable a waiting caller parks on. [`shared`] is the one process-wide
/// instance the production call sites use.
/// Test: `worktree_reclaim_gh_gate_tests`.
pub(crate) struct GhPollGate {
    state: Mutex<GateState>,
    finished: Condvar,
}

impl GhPollGate {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            finished: Condvar::new(),
        }
    }

    /// Lock the state, recovering from poisoning.
    ///
    /// Why: a panic inside one caller's `run` closure must not permanently
    /// disable pull-request polling for the rest of the daemon's life. The
    /// guarded state is bookkeeping only — no on-disk or process invariant
    /// rides on it — so taking the inner guard is safe and is what
    /// `core::claude_json_guard` does for the same reason.
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `run` for `root`'s `call`, unless the gate says not to (#6867).
    ///
    /// Why: the two rules in the module doc.
    /// What: returns the suspension refusal without spawning when `root` is
    /// backing off; joins an identical in-flight call and returns ITS result
    /// when one is outstanding; otherwise runs `run`, records the outcome
    /// against `root`'s strike count, and hands the result to every waiter that
    /// arrived meanwhile. `call` must name the exact query — the caller's
    /// branch, or the bulk index — never just its kind.
    /// Test: `two_concurrent_polls_spawn_one_child`,
    /// `a_fourth_call_is_skipped_after_three_timeouts`,
    /// `an_answer_clears_the_strikes`, `a_panicking_run_releases_its_waiters`.
    pub(crate) fn poll<F>(&self, root: &Path, call: &str, run: F) -> Result<String, GhFailure>
    where
        F: FnOnce() -> Result<String, GhFailure>,
    {
        let key = format!("{}\u{1}{call}", root.display());
        let mut state = self.lock();
        if let Some(reason) = take_suspension(&mut state, root, Instant::now()) {
            return Err(GhFailure::new(reason));
        }
        let slot = state.calls.entry(key.clone()).or_default();
        if slot.running {
            let waited_for = slot.seq;
            slot.waiters += 1;
            return self.wait_for(state, &key, waited_for);
        }
        slot.running = true;
        drop(state);

        // Disarmed by `finish`; on an unwinding `run` its `Drop` publishes a
        // failure instead, so waiters are never parked forever.
        let flight = InFlight {
            gate: self,
            root: root.to_path_buf(),
            key,
            armed: true,
        };
        flight.finish(run())
    }

    /// Park until the call identified by `waited_for` completes, then take its
    /// result.
    fn wait_for(
        &self,
        state: MutexGuard<'_, GateState>,
        key: &str,
        waited_for: u64,
    ) -> Result<String, GhFailure> {
        let mut state = self
            .finished
            .wait_while(state, |s| {
                s.calls
                    .get(key)
                    .is_some_and(|c| c.running || c.seq == waited_for)
            })
            .unwrap_or_else(PoisonError::into_inner);
        let mut result = None;
        if let Some(slot) = state.calls.get_mut(key) {
            result = slot.last.clone();
            slot.waiters = slot.waiters.saturating_sub(1);
            if slot.waiters == 0 {
                // The payload is held only for the waiters that asked for it.
                slot.last = None;
            }
            if slot.is_idle() {
                state.calls.remove(key);
            }
        }
        result.unwrap_or_else(|| {
            Err(GhFailure::new(
                "the in-flight `gh` poll ended without a result",
            ))
        })
    }

    /// Publish `result` to every parked waiter, fold it into `root`'s strike
    /// count, and hand it back to the caller that produced it.
    ///
    /// The result is CLONED only when a waiter is actually parked — a 400-PR
    /// JSON payload is not copied for the common uncontended call.
    fn complete(
        &self,
        root: &Path,
        key: &str,
        result: Result<String, GhFailure>,
    ) -> Result<String, GhFailure> {
        let mut state = self.lock();
        note_outcome(&mut state.roots, root, &result);
        if let Some(slot) = state.calls.get_mut(key) {
            slot.running = false;
            slot.seq = slot.seq.wrapping_add(1);
            slot.last = (slot.waiters > 0).then(|| result.clone());
            if slot.is_idle() {
                state.calls.remove(key);
            }
        }
        drop(state);
        self.finished.notify_all();
        result
    }
}

/// Marks a call as in flight and guarantees its slot is released.
struct InFlight<'a> {
    gate: &'a GhPollGate,
    root: PathBuf,
    key: String,
    armed: bool,
}

impl InFlight<'_> {
    fn finish(mut self, result: Result<String, GhFailure>) -> Result<String, GhFailure> {
        self.armed = false;
        self.gate.complete(&self.root, &self.key, result)
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if self.armed {
            // `run` unwound. Waiters must be released with a failure rather
            // than left parked on a call that will never complete.
            let _ = self.gate.complete(
                &self.root,
                &self.key,
                Err(GhFailure::new("the `gh` poll panicked before answering")),
            );
        }
    }
}

/// The suspension refusal for `root`, if it is still in force at `now`.
///
/// Why: the sentence is the operator's whole diagnosis, so it names the strike
/// count and the wall-clock time polling resumes.
/// What: `Some(reason)` while the deadline is in the future; `None` otherwise,
/// clearing an expired deadline on the way out so the next call runs for real.
/// Test: `a_fourth_call_is_skipped_after_three_timeouts`,
/// `an_expired_suspension_lets_the_next_call_through`.
fn take_suspension(state: &mut GateState, root: &Path, now: Instant) -> Option<String> {
    let entry = state.roots.get_mut(root)?;
    let (until, until_wall) = entry.suspended_until?;
    if now < until {
        return Some(format!(
            "gh polling suspended: {} consecutive timeouts, next retry at {}",
            entry.consecutive_timeouts,
            format_local(until_wall)
        ));
    }
    entry.suspended_until = None;
    None
}

/// Fold one call's outcome into `root`'s strike count.
///
/// Why: only a TIMEOUT is evidence of the hang this backs off from. A non-zero
/// exit is an answer — it came back in milliseconds and leaked nothing — so it
/// clears the record rather than counting toward a suspension that would then
/// hide a fixable auth failure behind a "suspended" message.
/// What: a timeout increments and, at [`TIMEOUT_STRIKES`], arms the deadline
/// from [`backoff_for`]; anything else drops the root's record entirely, which
/// also keeps the map bounded by the number of currently-failing roots.
/// Test: `an_answer_clears_the_strikes`,
/// `a_fourth_call_is_skipped_after_three_timeouts`.
fn note_outcome(
    roots: &mut HashMap<PathBuf, RootBackoff>,
    root: &Path,
    result: &Result<String, GhFailure>,
) {
    let timed_out = matches!(result, Err(f) if f.timed_out());
    if !timed_out {
        roots.remove(root);
        return;
    }
    let entry = roots.entry(root.to_path_buf()).or_default();
    entry.consecutive_timeouts = entry.consecutive_timeouts.saturating_add(1);
    if entry.consecutive_timeouts >= TIMEOUT_STRIKES {
        let wait = backoff_for(entry.consecutive_timeouts);
        entry.suspended_until = Some((Instant::now() + wait, SystemTime::now() + wait));
        tracing::warn!(
            root = %root.display(),
            timeouts = entry.consecutive_timeouts,
            "worktree-reclaim: gh polling suspended for {}s after consecutive timeouts — \
             a hung `gh` leaks a process pair per poll (#6867)",
            wait.as_secs()
        );
    }
}

/// The one process-wide gate every production `gh` poll passes through.
///
/// Why: the poll sites are free functions called from the sweep, the prune
/// route, and the doctor probe, none of which own a gate to thread through.
/// Threading one would change five signatures across three modules to express
/// a fact that is genuinely process-wide: how many `gh` children this PROCESS
/// currently has outstanding. The same reasoning — and the same `static` shape
/// — as `core::claude_json_guard`'s process-wide `.claude.json` lock.
/// What: a lazily created [`GhPollGate`]. Tests use their own instances.
pub(crate) fn shared() -> &'static GhPollGate {
    static GATE: OnceLock<GhPollGate> = OnceLock::new();
    GATE.get_or_init(GhPollGate::new)
}

#[cfg(test)]
#[path = "worktree_reclaim_gh_gate_tests.rs"]
mod worktree_reclaim_gh_gate_tests;
