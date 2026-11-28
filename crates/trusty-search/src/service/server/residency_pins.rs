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
//! the current trusted stretch began. [`grace_expired`] is the pure park rule.
//! Every time input is caller-supplied, so a test drives the clock, and every
//! membership read takes a `registered` predicate ([`registered_in`]) so an id
//! this daemon does not know is never trusted.
//! A stale, never-fetched or empty set, or one naming no registered index,
//! answers `None` from [`ResidencyPins::members`], and the sweep then runs
//! exactly as before.
//! Test: `residency_pins_10533_tests`.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use trusty_common::residency::{
    residency_grace_secs, residency_stale_secs, ActiveProjectSet, ResidencySnapshot,
    RESIDENCY_GRACE_SECS_ENV, RESIDENCY_STALE_SECS_ENV,
};

use super::state::SearchAppState;
use crate::core::registry::IndexId;

/// The pulled active set plus the per-index "last seen" clock.
///
/// Why: "absent for the grace period" needs to know since when an index has
/// been absent, which one snapshot cannot say. A set that is not trusted must
/// never start that clock, so `fresh_since` restarts after every untrusted
/// stretch.
/// What: `Mutex` over plain data; every method takes the lock once and never
/// holds it across an `.await`.
/// Test: `members_is_none_for_a_stale_absent_or_empty_set`,
/// `absence_restarts_after_a_stale_gap`,
/// `absence_restarts_after_an_empty_set`.
pub struct ResidencyPins {
    grace: Duration,
    inner: Mutex<Inner>,
}

struct Inner {
    snapshot: ResidencySnapshot,
    /// Start of the current run of trusted observations.
    fresh_since: Option<Instant>,
    /// Last observation that listed each index id.
    member_seen: HashMap<String, Instant>,
    /// Snapshot freshness at the last [`ResidencyPins::freshness_change`].
    last_fresh: bool,
    /// Members filled the cap at the last [`ResidencyPins::note_over_cap`].
    over_cap: bool,
}

/// The active-set fields `/health` reports (#10533).
///
/// Why: an operator needs to see whether the sweep is pinning anything, and
/// how much of the cap the pins take, without reading the daemon log.
/// What: `trusted` is whether [`ResidencyPins::members`] answers `Some`;
/// `pinned_members` is the size of that set, `0` when untrusted.
/// Test: `health_reports_the_active_set_pin_count_and_trust`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ActiveSetHealth {
    pub trusted: bool,
    pub pinned_members: usize,
}

/// "Is `id` an index this daemon knows?" — resident or cold-parked.
///
/// Why (#10533): tm can publish an id that matches no registration (#10530);
/// a set made only of such ids must not count as trusted.
/// Test: `a_set_naming_only_unknown_ids_parks_nothing_under_the_cap`.
pub(super) fn registered_in(state: &SearchAppState) -> impl Fn(&str) -> bool + '_ {
    move |id: &str| {
        let id = IndexId::new(id.to_string());
        state.registry.get(&id).is_some() || state.cold_store.contains(&id)
    }
}

/// The trusted, registered member ids of `inner`'s set at `now`.
fn trusted_members(
    inner: &Inner,
    now: Instant,
    registered: &dyn Fn(&str) -> bool,
) -> Option<HashSet<String>> {
    let set = inner.snapshot.pinned(now)?;
    let ids: HashSet<String> = set
        .projects
        .iter()
        .flat_map(|p| p.index_ids.iter())
        .filter(|id| registered(id))
        .cloned()
        .collect();
    (!ids.is_empty()).then_some(ids)
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
                last_fresh: false,
                over_cap: false,
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
    /// What: when the set held before this pull was not trusted — stale,
    /// never fetched, empty, or naming no registered index — a new trusted
    /// stretch starts at `now`. Every index id in `set` is stamped as seen at
    /// `now`.
    /// Test: `absence_restarts_after_a_stale_gap`,
    /// `absence_restarts_after_an_empty_set`.
    pub fn observe(&self, set: ActiveProjectSet, now: Instant, registered: &dyn Fn(&str) -> bool) {
        let mut inner = self.lock();
        // #10533: an untrusted previous set never ran the absence clock.
        if trusted_members(&inner, now, registered).is_none() {
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

    /// The active set's registered index ids, or `None` when the set is not
    /// trusted: stale, never fetched, or naming no registered index.
    ///
    /// Why `None` for an empty set: design section 3 — "an empty or missing
    /// set means no evictions, only the existing cap sweep". #10533: an id
    /// `registered` rejects is dropped, and a set left empty is untrusted.
    /// Test: `members_is_none_for_a_stale_absent_or_empty_set`,
    /// `a_set_naming_only_unknown_ids_parks_nothing_under_the_cap`.
    pub fn members(
        &self,
        now: Instant,
        registered: &dyn Fn(&str) -> bool,
    ) -> Option<HashSet<String>> {
        trusted_members(&self.lock(), now, registered)
    }

    /// How long `id` has been out of a trusted set, or `None` when it is a
    /// member or the set is not trusted (see [`Self::members`]).
    ///
    /// What: measured from the later of its last sighting and the start of the
    /// current trusted stretch, so an index never seen in a set still waits a
    /// full grace period after the first trusted pull.
    /// Test: `absence_restarts_after_a_stale_gap`,
    /// `absence_restarts_after_an_empty_set`.
    pub fn absent_for(
        &self,
        id: &str,
        now: Instant,
        registered: &dyn Fn(&str) -> bool,
    ) -> Option<Duration> {
        let inner = self.lock();
        let members = trusted_members(&inner, now, registered)?;
        if members.contains(id) {
            return None;
        }
        let fresh_since = inner.fresh_since?;
        let since = inner
            .member_seen
            .get(id)
            .map_or(fresh_since, |seen| (*seen).max(fresh_since));
        Some(now.saturating_duration_since(since))
    }

    /// `Some(fresh)` when the snapshot's freshness at `now` differs from the
    /// last call's, else `None`. Drives one info line per change.
    /// Test: `freshness_change_reports_each_transition_once`.
    pub fn freshness_change(&self, now: Instant) -> Option<bool> {
        let mut inner = self.lock();
        let fresh = inner.snapshot.is_fresh(now);
        (fresh != inner.last_fresh).then(|| {
            inner.last_fresh = fresh;
            fresh
        })
    }

    /// Record whether members fill the cap; `true` only on entering that
    /// state, so the caller warns once per change.
    /// Test: `over_cap_warns_once_per_state_change`.
    pub fn note_over_cap(&self, over: bool) -> bool {
        let mut inner = self.lock();
        let entered = over && !inner.over_cap;
        inner.over_cap = over;
        entered
    }

    /// The `/health` view of the set at `now`.
    /// Test: `health_reports_the_active_set_pin_count_and_trust`.
    pub fn health(&self, now: Instant, registered: &dyn Fn(&str) -> bool) -> ActiveSetHealth {
        let members = self.members(now, registered);
        ActiveSetHealth {
            trusted: members.is_some(),
            pinned_members: members.map_or(0, |m| m.len()),
        }
    }
}

/// Should a non-member resident index park now?
///
/// Why: design section 3, ACTIVE to PARKED — absent from the active set for the
/// grace period AND no query in that period.
/// What: `true` only when `absent_for` is `Some` and at least `grace`, and the
/// index's last activity (`last_activity_unix`: the latest of its query,
/// read and index stamps) is at least `grace` before `now_unix`.
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
