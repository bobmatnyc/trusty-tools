//! Alert dedup: `fleet-poll.py`'s `dedup` and its `REEMIT` table.
//!
//! Why: the poller runs every cycle; without dedup the Architect is woken for
//! the same open dialog every poll.
//! What: [`dedup()`] keys each candidate by `session|pane|kind|fingerprint`.
//! A key absent this cycle is forgotten, so its next appearance is new; a kind
//! in [`REEMIT`] re-emits after its interval. [`DedupState`] keeps keys in
//! insertion order so it serializes like the Python dict.
//! Test: `test_dedup_suppresses_until_cleared_for_one_cycle`,
//! `test_dedup_reemits_idle_every_30_minutes_but_not_dialog`,
//! `dedup_matches_python_goldens`.

use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

use super::Alert;

/// Kinds that re-emit while still present, and their interval in seconds.
/// `band` is absent on purpose: it fires once per change.
// #9171: fleet-poll.py's table, which adds the architect_* and token kinds.
pub const REEMIT: [(&str, f64); 10] = [
    ("idle", 1800.0),
    ("ctx", 1800.0),
    ("load", 1800.0),
    ("disk", 1800.0),
    ("swap", 1800.0),
    ("self_ctx", 1800.0),
    ("architect_dead", 1800.0),
    ("architect_silent", 1800.0),
    ("architect_frozen", 1800.0),
    ("token", 3600.0),
];

/// When a dedup key was first and last emitted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Seen {
    /// First emission of the current stretch.
    pub first: f64,
    /// Latest emission.
    pub last: f64,
}

/// `state["alerts"]`: dedup keys in insertion order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DedupState {
    alerts: Vec<(String, Seen)>,
}

impl DedupState {
    /// The record for `key`, if it was active last cycle.
    pub fn get(&self, key: &str) -> Option<Seen> {
        self.alerts.iter().find(|(k, _)| k == key).map(|(_, s)| *s)
    }

    /// Active keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.alerts.iter().map(|(k, _)| k.as_str())
    }

    /// True when no key is active.
    pub fn is_empty(&self) -> bool {
        self.alerts.is_empty()
    }

    /// Every `(key, seen)` pair in insertion order (#9171 A3a: the state file).
    pub(crate) fn entries(&self) -> &[(String, Seen)] {
        &self.alerts
    }

    /// A state holding `entries` in the given order.
    pub(crate) fn from_entries(entries: Vec<(String, Seen)>) -> Self {
        Self { alerts: entries }
    }
}

/// Serializes as `{"alerts": {key: {"first": …, "last": …}, …}}`.
impl Serialize for DedupState {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        struct Ordered<'a>(&'a [(String, Seen)]);
        impl Serialize for Ordered<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let mut map = s.serialize_map(Some(self.0.len()))?;
                for (k, v) in self.0 {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
        let mut map = s.serialize_map(Some(1))?;
        map.serialize_entry("alerts", &Ordered(&self.alerts))?;
        map.end()
    }
}

fn reemit_interval(kind: &str) -> Option<f64> {
    REEMIT.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
}

/// Splits `candidates` into `(new, suppressed_count)` and replaces the state.
pub fn dedup(state: &mut DedupState, candidates: &[Alert], now: f64) -> (Vec<Alert>, usize) {
    let mut active: Vec<(String, Seen)> = Vec::new();
    let mut new = Vec::new();
    let mut suppressed = 0;
    for a in candidates {
        let key = [a.session.as_str(), &a.pane, &a.kind, &a.fingerprint].join("|");
        if active.iter().any(|(k, _)| *k == key) {
            continue;
        }
        let prev = state.get(&key);
        let due = reemit_interval(&a.kind)
            .zip(prev)
            .is_some_and(|(every, p)| every != 0.0 && now - p.last >= every);
        match prev {
            Some(p) if !due => {
                suppressed += 1;
                active.push((key, p));
            }
            _ => {
                new.push(a.clone());
                let first = prev.map_or(now, |p| p.first);
                active.push((key, Seen { first, last: now }));
            }
        }
    }
    state.alerts = active;
    (new, suppressed)
}
