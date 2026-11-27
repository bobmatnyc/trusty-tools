//! tm's active-project set, held for the residency sweep (#10533).
//!
//! Why: the resident cap ranked by recency alone, so an index a live tm
//! session depends on parked exactly like one nobody had opened in weeks.
//! trusty-mpm publishes which projects are active (`mpm.residency.active`);
//! this module holds the last pulled set so the sweep can pin its members and
//! park the rest once they have idled past the grace period.
//! What: [`ResidencyPins`] wraps the shared
//! [`trusty_common::residency::ResidencySnapshot`] with the bookkeeping a
//! grace period needs — when each index was last seen in the set, and when
//! the current fresh stretch began. [`grace_expired`] is the pure park rule.
//! Every time input is caller-supplied, so a test drives the clock.
//! A stale, never-fetched or empty set answers `None` from
//! [`ResidencyPins::members`], and the sweep then runs exactly as before.
//! Test: `residency_pins_10533_tests`.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use trusty_common::residency::{
    residency_grace_secs, residency_stale_secs, ActiveProjectSet, ResidencySnapshot,
    RESIDENCY_GRACE_SECS_ENV, RESIDENCY_STALE_SECS_ENV,
};

/// The pulled active set plus the per-index "last seen" clock.
///
/// Why: "absent for the grace period" needs to know since when an index has
/// been absent, which one snapshot cannot say. A set that is not trusted must
/// never start that clock, so `fresh_since` restarts after every stale gap.
/// What: `Mutex` over plain data; every method takes the lock once and never
/// holds it across an `.await`.
/// Test: `members_is_none_for_a_stale_absent_or_empty_set`,
/// `absence_restarts_after_a_stale_gap`.
pub struct ResidencyPins {
    grace: Duration,
    inner: Mutex<Inner>,
}

struct Inner {
    snapshot: ResidencySnapshot,
    /// Start of the current run of fresh observations.
    fresh_since: Option<Instant>,
    /// Last observation that listed each index id.
    member_seen: HashMap<String, Instant>,
}

impl ResidencyPins {
    /// Pins with explicit windows. `stale_after` is the snapshot's staleness
    /// window; `grace` is how long a non-member idles before it parks.
    pub fn new(stale_after: Duration, grace: Duration) -> Self {
        Self {
            grace,
            inner: Mutex::new(Inner {
                snapshot: ResidencySnapshot::new(stale_after),
                fresh_since: None,
                member_seen: HashMap::new(),
            }),
        }
    }

    /// Pins whose windows come from `TRUSTY_RESIDENCY_STALE_SECS` (default
    /// 600) and `TRUSTY_RESIDENCY_GRACE_SECS` (default 120), parsed by the
    /// shared `trusty_common::residency` readers.
    pub fn from_env() -> Self {
        let stale = residency_stale_secs(std::env::var(RESIDENCY_STALE_SECS_ENV).ok().as_deref());
        let grace = residency_grace_secs(std::env::var(RESIDENCY_GRACE_SECS_ENV).ok().as_deref());
        Self::new(Duration::from_secs(stale), Duration::from_secs(grace))
    }

    /// The grace period a non-member idles before it parks.
    pub fn grace(&self) -> Duration {
        self.grace
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a successful pull at `now`.
    ///
    /// What: a pull that follows a stale (or no) snapshot starts a new fresh
    /// stretch; every index id in `set` is stamped as seen at `now`.
    pub fn observe(&self, set: ActiveProjectSet, now: Instant) {
        let mut inner = self.lock();
        if !inner.snapshot.is_fresh(now) {
            inner.fresh_since = Some(now);
        }
        for id in set.projects.iter().flat_map(|p| p.index_ids.iter()) {
            inner.member_seen.insert(id.clone(), now);
        }
        inner.snapshot.observe(set, now);
    }

    /// Record a failed pull at `now`; the held set survives while fresh.
    pub fn on_pull_failure(&self, now: Instant) {
        self.lock().snapshot.on_pull_failure(now);
    }

    /// The active set's index ids, or `None` when the set is stale, was never
    /// fetched, or names no index.
    ///
    /// Why `None` for an empty set: design section 3 — "an empty or missing
    /// set means no evictions, only the existing cap sweep".
    pub fn members(&self, now: Instant) -> Option<HashSet<String>> {
        let inner = self.lock();
        let set = inner.snapshot.pinned(now)?;
        let ids: HashSet<String> = set
            .projects
            .iter()
            .flat_map(|p| p.index_ids.iter().cloned())
            .collect();
        (!ids.is_empty()).then_some(ids)
    }

    /// How long `id` has been out of a trusted set, or `None` when it is a
    /// member or the set is not trusted (see [`Self::members`]).
    ///
    /// What: measured from the later of its last sighting and the start of the
    /// current fresh stretch, so an index never seen in a set still waits a
    /// full grace period after the first trusted pull.
    pub fn absent_for(&self, id: &str, now: Instant) -> Option<Duration> {
        let members = self.members(now)?;
        if members.contains(id) {
            return None;
        }
        let inner = self.lock();
        let fresh_since = inner.fresh_since?;
        let since = inner
            .member_seen
            .get(id)
            .map_or(fresh_since, |seen| (*seen).max(fresh_since));
        Some(now.saturating_duration_since(since))
    }
}

/// Should a non-member resident index park now?
///
/// Why: design section 3, ACTIVE to PARKED — absent from the active set for the
/// grace period AND no query in that period.
/// What: `true` only when `absent_for` is `Some` and at least `grace`, and the
/// index's last activity (`last_activity_unix`, the recency key: latest query
/// or index) is at least `grace` before `now_unix`.
/// Test: `non_member_parks_only_after_the_grace_period_with_no_query`.
pub fn grace_expired(
    absent_for: Option<Duration>,
    last_activity_unix: u64,
    grace: Duration,
    now_unix: u64,
) -> bool {
    let Some(absent) = absent_for else {
        return false;
    };
    absent >= grace && now_unix.saturating_sub(last_activity_unix) >= grace.as_secs()
}
