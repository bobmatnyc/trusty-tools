//! Bounds on how much of the palace estate startup work may hold open at once
//! (#7106, epic #6802).
//!
//! Why: three startup jobs each walk every palace on disk — hydration
//! (`AppState::load_palaces_from_disk`), the BM25 backfill sweep, and the BM25
//! repair sweep. None of them bounded how many palaces they held open, and each
//! open hydrates that palace's drawer table, HNSW graph and KG adjacency
//! (~90 MB) plus three redb page caches. On the reporter's 94-palace install
//! that is what a 14 GB → 22.7 GB, 614%-CPU spike ~26 minutes after boot with
//! no request in flight looks like. The estate is not going to get smaller; the
//! concurrency has to get bounded.
//!
//! What: [`StartupOpenGate`], a semaphore with an observable high-water mark,
//! shared through `AppState` so all three jobs draw on ONE budget rather than
//! three independent ones; and [`release_after_sweep`], the rule for handing a
//! palace a sweep opened back to the LRU.
//!
//! Residency ruling (#7087): a palace a client used recently stays resident.
//! [`release_after_sweep`] reads the palace's persisted `last_used` stamp —
//! which the sweeps never write, so it is a pure client-use signal — and leaves
//! anything used inside [`DEFAULT_KEEP_RECENT_SECS`] alone. It also refuses to
//! release a handle anything still references, the same `Arc::strong_count`
//! anchor `PalaceRegistry::evict_idle` uses.
//!
//! Test: `gate_never_exceeds_its_limit_under_contention`,
//! `parse_open_limit_warns_and_keeps_the_default_on_garbage`,
//! `release_after_sweep_keeps_a_recently_used_palace`.
//!
//! [`StartupOpenGate`]: crate::startup_budget::StartupOpenGate
//! [`release_after_sweep`]: crate::startup_budget::release_after_sweep
//! [`DEFAULT_KEEP_RECENT_SECS`]: crate::startup_budget::DEFAULT_KEEP_RECENT_SECS

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::PalaceRegistry;

/// Environment variable overriding [`DEFAULT_STARTUP_OPEN_LIMIT`].
pub const STARTUP_OPEN_LIMIT_ENV: &str = "TRUSTY_MEMORY_STARTUP_OPEN_LIMIT";

/// How many palaces startup work may hold open at once.
///
/// Why (#7106): each concurrently-open palace costs roughly 90 MB of hydrated
/// index plus its redb page caches, so the peak is the product of this number
/// and the per-palace cost — the only term the daemon controls. Four keeps
/// hydration meaningfully parallel on a laptop-class host (the documented 16 GB
/// minimum, #6802) while capping the transient peak at a few hundred megabytes
/// instead of the whole estate.
/// What: 4, overridable via [`STARTUP_OPEN_LIMIT_ENV`].
/// Test: `parse_open_limit_warns_and_keeps_the_default_on_garbage`.
pub const DEFAULT_STARTUP_OPEN_LIMIT: usize = 4;

/// How recently a client must have used a palace for a sweep to leave it warm.
///
/// Why (#7087): the owner's ruling is that an active-session palace stays
/// resident. Fifteen minutes is long enough to cover a pause in an interactive
/// session and short enough that a sweep still reclaims a genuinely dormant
/// estate.
/// What: 900 seconds, compared against the palace's persisted `last_used`
/// stamp, which only client operations write.
/// Test: `release_after_sweep_keeps_a_recently_used_palace`.
pub const DEFAULT_KEEP_RECENT_SECS: u64 = 900;

/// Decide the open limit from a raw override string, with the warning to log.
///
/// Why (#7106, Fail-Open Check): a rejected value must not silently restore
/// unbounded startup opens — that is the failure this module exists to prevent,
/// and it would look identical to a working daemon until the host swapped.
/// Separating the decision from the logging makes the fallback assertable
/// without capturing logs.
/// What: `None`, empty, non-numeric and `0` all yield
/// [`DEFAULT_STARTUP_OPEN_LIMIT`]; a rejected non-empty value also yields the
/// warning text naming the variable and the offending value.
/// Test: `parse_open_limit_warns_and_keeps_the_default_on_garbage`.
pub fn parse_open_limit(raw: Option<&str>) -> (usize, Option<String>) {
    let Some(value) = raw else {
        return (DEFAULT_STARTUP_OPEN_LIMIT, None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return (DEFAULT_STARTUP_OPEN_LIMIT, None);
    }
    match trimmed.parse::<usize>() {
        Ok(n) if n > 0 => (n, None),
        _ => (
            DEFAULT_STARTUP_OPEN_LIMIT,
            Some(format!(
                "{STARTUP_OPEN_LIMIT_ENV}={value:?} is not a positive integer; using the \
                 bounded default of {DEFAULT_STARTUP_OPEN_LIMIT} concurrent palace opens \
                 (startup opens are NOT unbounded) — see #7106"
            )),
        ),
    }
}

/// Resolve the startup open limit from the environment, logging a rejection.
pub fn startup_open_limit_from_env() -> usize {
    let raw = std::env::var(STARTUP_OPEN_LIMIT_ENV).ok();
    let (limit, warning) = parse_open_limit(raw.as_deref());
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
    limit
}

/// One shared budget for every startup job that opens palaces (#7106).
///
/// Why: hydration, the BM25 backfill sweep and the BM25 repair sweep all run
/// concurrently at boot. Three independent limits multiply; one shared gate is
/// the only thing that makes "at most N palaces open at once" true of the
/// process rather than of each job separately. Cloning is cheap and shares the
/// same semaphore and counters, so it lives on `AppState`.
/// What: a `tokio::sync::Semaphore` plus a live count and its high-water mark,
/// so the bound is observable rather than merely intended.
/// Test: `gate_never_exceeds_its_limit_under_contention`.
#[derive(Clone, Debug)]
pub struct StartupOpenGate {
    sem: Arc<Semaphore>,
    limit: usize,
    live: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl StartupOpenGate {
    /// Build a gate with an explicit limit (clamped to at least 1).
    pub fn with_limit(limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            sem: Arc::new(Semaphore::new(limit)),
            limit,
            live: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Build a gate from [`startup_open_limit_from_env`].
    pub fn from_env() -> Self {
        Self::with_limit(startup_open_limit_from_env())
    }

    /// The configured ceiling.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The most palaces ever held open at once through this gate.
    ///
    /// Why (#7106): "startup opens are bounded" is not observable from a palace
    /// count or a duration — a bounded run and an unbounded one load the same
    /// palaces and log the same summary. This is the measurement, for a test
    /// and for an operator reading it back.
    /// What: a relaxed load of the high-water mark, never reset.
    /// Test: `gate_never_exceeds_its_limit_under_contention`.
    pub fn peak_concurrent(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    /// Wait for a slot, then hold it until the returned permit is dropped.
    ///
    /// What: acquires one semaphore permit, bumps the live count, and raises the
    /// high-water mark if this acquisition set a new one.
    /// Test: `gate_never_exceeds_its_limit_under_contention`.
    pub async fn acquire(&self) -> StartupOpenPermit {
        let permit = Arc::clone(&self.sem)
            .acquire_owned()
            .await
            // The gate never closes its semaphore, so this cannot fail.
            .expect("startup open gate semaphore is never closed");
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        StartupOpenPermit {
            _permit: permit,
            live: Arc::clone(&self.live),
        }
    }
}

/// A held startup-open slot; releases on drop.
#[derive(Debug)]
pub struct StartupOpenPermit {
    _permit: OwnedSemaphorePermit,
    live: Arc<AtomicUsize>,
}

impl Drop for StartupOpenPermit {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Hand a palace a startup sweep opened back to the LRU (#7106, #7087).
///
/// Why: a sweep that walks the whole estate leaves every palace it touched
/// resident, so a background job nobody asked for pins the daemon's footprint
/// at the LRU cap. Releasing what the sweep brought in is what keeps the sweep's
/// cost transient. The owner's residency ruling (#7087) is the limit on that: a
/// palace a client is using stays resident.
/// What: refuses to release when (a) the palace was already resident before the
/// sweep opened it — something else wanted it warm; (b) the persisted
/// `last_used` stamp is inside `keep_recent`, which only client operations
/// write, never a sweep; or (c) anything still holds a reference to the handle.
/// Otherwise drops the cached handle; the next access transparently reopens
/// from redb, which is the source of truth.
/// Returns whether the handle was released.
/// Test: `release_after_sweep_keeps_a_recently_used_palace`,
/// `release_after_sweep_keeps_an_already_resident_palace`.
pub fn release_after_sweep(
    registry: &PalaceRegistry,
    palace_id: &PalaceId,
    data_dir: &std::path::Path,
    was_resident_before: bool,
    keep_recent: Duration,
) -> bool {
    if was_resident_before {
        return false;
    }
    if let Some(stamp) = crate::palace_last_used::read(data_dir) {
        let now = crate::palace_last_used::now_unix();
        if now.saturating_sub(stamp) < keep_recent.as_secs() {
            // #7087: a client used this palace recently — leave it warm.
            return false;
        }
    }
    registry.release_if_unreferenced(palace_id)
}

#[cfg(test)]
#[path = "startup_budget_tests.rs"]
mod tests;
