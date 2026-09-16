//! The cleanup sweep's `gh`-authentication backoff (#8058).
//!
//! Why: [`super::sweep::run_sweep`] asked `gh pr view` once per pending
//! registry entry and, on failure, logged a `warn!` and moved to the next
//! entry. When the failure is `gh auth login` — not a transient network error
//! but a host-wide condition that the next call cannot fix — every entry failed
//! the same way on every tick, so the sweep spawned a `gh` process per entry
//! per tick forever. The 2026-09-15 log is that loop, and the process churn
//! alone degraded `/health`. [`super::super::super::session_manager`]'s reclaim
//! survey hit the same shape in #6867 and solved it the same way; this is that
//! rule applied to the one `gh` caller it did not cover.
//!
//! What: a strike counter with a doubling suspension window. An auth failure
//! adds a strike; [`STRIKES`] consecutive strikes suspend the sweep's `gh`
//! calls entirely for a window that doubles per further failure and saturates
//! at [`BACKOFF_MAX`]. ANY answer at all — a success, or a definite non-auth
//! error — clears the strikes, because those are not the condition this guards.
//! While suspended, [`AuthBackoff::degraded_reason`] is the sentence `/health`
//! publishes, and the sweep logs [`STRIKES`] lines per window rather than one
//! per entry per tick.
//!
//! Deliberately NOT a process-wide static on its own: the gate is a value the
//! sweep is handed, so the tests drive their own instance and never touch the
//! one the daemon uses. [`shared`] is that single production instance.
//!
//! Test: the sibling `auth_backoff_tests.rs`.

use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// How many consecutive auth failures suspend the sweep's `gh` calls.
///
/// Why: two, not three. An auth failure answers instantly and definitively —
/// unlike the timeout the reclaim gate counts, there is no "slow network" arm
/// to give the benefit of the doubt to.
pub const STRIKES: u32 = 2;

/// The first suspension window, doubled per additional failure.
const BACKOFF_BASE: Duration = Duration::from_secs(5 * 60);

/// The ceiling the doubling saturates at.
///
/// Why: an operator who runs `gh auth login` must see the sweep resume without
/// restarting the daemon, so the window cannot grow without bound.
const BACKOFF_MAX: Duration = Duration::from_secs(60 * 60);

/// Does this error text name a `gh` authentication failure?
///
/// Why: only an auth failure earns the suspension. A 404, a parse error, or a
/// network blip is a per-entry problem the next tick may well resolve, and
/// suspending the whole sweep for one of those would turn a single bad registry
/// entry into an outage.
/// What: a case-insensitive match against the literals `gh` itself prints when
/// it has no usable credential. The `gh auth login` remedy string is the one the
/// #8058 log carried.
/// Test: `auth_failure_is_recognised_from_gh_wording`,
/// `a_plain_failure_is_not_an_auth_failure`.
#[must_use]
pub fn is_auth_failure(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "gh auth login",
        "authentication required",
        "requires authentication",
        "gh_token",
        "github_token",
        "bad credentials",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

/// How long a suspension lasts after `consecutive` auth failures.
///
/// What: `BACKOFF_BASE * 2^(consecutive - STRIKES)`, saturating at
/// [`BACKOFF_MAX`]. The shift count is clamped so it cannot overflow.
/// Test: `backoff_grows_and_then_saturates`.
fn backoff_for(consecutive: u32) -> Duration {
    let steps = consecutive.saturating_sub(STRIKES).min(16);
    let secs = BACKOFF_BASE.as_secs().saturating_mul(1u64 << steps);
    Duration::from_secs(secs.min(BACKOFF_MAX.as_secs()))
}

/// Mutable state behind the gate's lock.
#[derive(Default)]
struct State {
    consecutive: u32,
    /// The instant the suspension lifts, when one is in force.
    suspended_until: Option<Instant>,
    /// The operator-facing sentence, published on `/health` while suspended.
    reason: Option<String>,
    /// Whether the CURRENT window has already been logged.
    reported: bool,
}

/// The sweep's auth-failure gate.
///
/// Why: see the module doc. A struct rather than free functions over a static,
/// so its whole behaviour is testable on a local instance with a driven clock.
/// What: a mutex over [`State`]. A poisoned lock is recovered rather than
/// propagated — a backoff gate that panics is strictly worse than one that
/// keeps counting.
/// Test: `auth_backoff_tests`.
#[derive(Default)]
pub struct AuthBackoff {
    state: Mutex<State>,
}

impl AuthBackoff {
    /// A gate with no strikes recorded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// May the sweep spawn a `gh` call right now?
    ///
    /// What: `false` only while a suspension window is open. The window is
    /// cleared on the first call after it expires, so the next failure starts
    /// its own report.
    /// Test: `strikes_suspend_the_sweep_and_the_window_expires`.
    pub fn may_poll(&self, now: Instant) -> bool {
        let mut st = self.lock();
        match st.suspended_until {
            Some(until) if now < until => false,
            Some(_) => {
                st.suspended_until = None;
                st.reason = None;
                st.reported = false;
                true
            }
            None => true,
        }
    }

    /// Record one auth failure; returns whether the caller should LOG it.
    ///
    /// Why: the loop's defect was one warn line per entry per tick. Returning
    /// the report decision here — rather than letting the caller guess — is what
    /// makes "once per backoff window" a property of the gate instead of a
    /// convention the caller has to remember.
    /// What: increments the strike count, opens or extends the suspension once
    /// [`STRIKES`] is reached, and answers `true` for the strikes that precede
    /// the suspension plus the one that opens it — [`STRIKES`] lines per window,
    /// a constant, where the defect was one per entry per tick forever.
    /// Test: `an_auth_failure_is_reported_once_per_window`.
    pub fn record_auth_failure(&self, now: Instant, detail: &str) -> bool {
        let mut st = self.lock();
        st.consecutive = st.consecutive.saturating_add(1);
        if st.consecutive < STRIKES {
            return true;
        }
        let window = backoff_for(st.consecutive);
        st.suspended_until = Some(now + window);
        st.reason = Some(format!(
            "pr-cleanup sweep suspended for {}s after {} consecutive `gh` authentication \
             failures: {}. Run `gh auth login`; the sweep resumes on its own.",
            window.as_secs(),
            st.consecutive,
            detail.trim()
        ));
        let first_report = !st.reported;
        st.reported = true;
        first_report
    }

    /// Record that `gh` answered — success, or any non-auth error.
    ///
    /// Why: the condition this guards is "no usable credential", and an answer
    /// of any kind proves the credential worked. Holding strikes across it would
    /// suspend a healthy sweep for a 404.
    /// Test: `any_answer_clears_the_strikes`.
    pub fn record_answer(&self) {
        let mut st = self.lock();
        st.consecutive = 0;
        st.suspended_until = None;
        st.reason = None;
        st.reported = false;
    }

    /// The sentence `/health` publishes while the sweep is suspended.
    ///
    /// Test: `degraded_reason_is_published_only_while_suspended`.
    #[must_use]
    pub fn degraded_reason(&self) -> Option<String> {
        self.lock().reason.clone()
    }

    /// The lock, with poisoning recovered rather than propagated.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The one gate the daemon's sweep uses.
///
/// Why: the sweep runs on a timer inside one process, so its strike count has
/// to outlive a single tick. Everything else takes its own instance.
/// Test: `shared_is_one_instance`.
pub fn shared() -> &'static AuthBackoff {
    static GATE: OnceLock<AuthBackoff> = OnceLock::new();
    GATE.get_or_init(AuthBackoff::new)
}

#[cfg(test)]
#[path = "auth_backoff_tests.rs"]
mod auth_backoff_tests;
