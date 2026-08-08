//! Who may relay an entry, and when — the two guards that keep one delivery
//! from becoming several relays.
//!
//! Why: without them the retry sweep and the request path race. A sweep tick
//! landing inside the ≤5 s window while `ingest`'s own relay is still in flight
//! lists the same `.json`, relays it again, and both attempts settle — one
//! delivery, two relays, and with at-least-once semantics that is a duplicate
//! the target sees. Separately, a sweep with no backoff re-relays every pending
//! entry every tick and rewrites its whole body plus two `fsync`s each time,
//! forever, which ADR-0034 §2's "Console retries with backoff" exists to
//! prevent.
//!
//! What: [`ClaimSet`] is an in-process exclusion — one relay per entry path at a
//! time, released on drop so a panicking relay cannot wedge the entry.
//! [`BackoffPolicy`] decides whether an entry is *due*: a freshly spooled one is
//! left alone for the relay timeout (the request path may still own it), and a
//! previously-attempted one waits `base × 2^attempts`, capped, before the next
//! try — stopping entirely at `max_attempts`.
//!
//! The two are independent on purpose. Backoff bounds cost over minutes and
//! hours; the claim set closes the sub-second window backoff cannot see, since
//! a first-attempt entry has no `last_attempt_at_unix_ms` to reason from.
//!
//! Test: `webhook/tests.rs` — `backoff_*` and the `sweep_*` concurrency cases,
//! notably `sweep_does_not_relay_an_entry_the_request_path_is_still_relaying`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::spool::SpoolEntry;

/// Exclusive in-process ownership of one spool entry's relay.
///
/// Why: both `ingest` and the sweep can reach the same entry path. A claim held
/// for the duration of the relay makes "someone is already relaying this" a
/// checkable fact rather than a timing hope.
/// What: a shared `HashSet<PathBuf>`. Locks are held only across the insert or
/// remove, never across an `await`.
///
/// In-process only, and deliberately so: two console processes sharing one
/// spool directory would still double-relay. That is out of scope here because
/// console is a singleton per data directory, and the target-side
/// `delivery_id` dedup step 4 owes is the real cross-process answer.
///
/// Test: `claim_set_refuses_a_second_claim_on_the_same_path`,
/// `claim_set_releases_on_drop_even_when_the_holder_panics`.
#[derive(Debug, Clone, Default)]
pub struct ClaimSet {
    held: Arc<Mutex<HashSet<PathBuf>>>,
}

impl ClaimSet {
    /// A fresh, empty claim set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Take exclusive ownership of `path`, or `None` if someone already has it.
    ///
    /// The returned guard releases on drop, including on panic and on an early
    /// `?` return, so a failed relay cannot leave an entry permanently claimed.
    ///
    /// Test: `claim_set_refuses_a_second_claim_on_the_same_path`.
    pub fn claim(&self, path: &Path) -> Option<Claim> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if !held.insert(path.to_path_buf()) {
            return None;
        }
        Some(Claim {
            held: Arc::clone(&self.held),
            path: path.to_path_buf(),
        })
    }

    /// How many entries are claimed right now. Diagnostics and tests only.
    pub fn len(&self) -> usize {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether nothing is claimed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Ownership of one entry's relay, released when dropped.
#[derive(Debug)]
pub struct Claim {
    held: Arc<Mutex<HashSet<PathBuf>>>,
    path: PathBuf,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.path);
    }
}

/// When a pending entry becomes eligible for another relay attempt.
///
/// Why: ADR-0034 §2 says "Console retries with backoff" and the first cut had
/// none — every pending entry was re-relayed every 60 s, each non-ack rewriting
/// the full base64 body plus two `fsync`s. Until step 4 binds a listener that is
/// every delivery, forever.
/// What: a grace period for a never-attempted entry, exponential spacing keyed
/// on `attempts`, a ceiling, and a hard stop. Pure — [`BackoffPolicy::is_due`]
/// takes `now` so the rules are testable without sleeping.
/// Test: `backoff_holds_off_a_freshly_spooled_entry`,
/// `backoff_spacing_grows_with_attempts`, `backoff_respects_the_ceiling`,
/// `backoff_stops_at_max_attempts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// How long a never-attempted entry is left alone after being spooled.
    ///
    /// Set to the relay timeout: within that window the request path that
    /// spooled it may still be relaying it, and its claim has not necessarily
    /// been taken yet at the instant the sweep lists the directory.
    pub first_attempt_grace: Duration,
    /// Spacing after the first failure; doubles per subsequent attempt.
    pub base: Duration,
    /// Upper bound on the spacing, however many attempts have failed.
    pub ceiling: Duration,
    /// After this many failed attempts the entry is never relayed again.
    ///
    /// It is NOT deleted — it stays on disk and keeps the health signal red,
    /// because an undeliverable webhook is an operator problem, not garbage.
    /// The cap exists so a permanently unrelayable entry stops costing a
    /// full-body rewrite and two `fsync`s on every tick.
    pub max_attempts: u32,
}

impl Default for BackoffPolicy {
    /// 5 s grace, 30 s base doubling to a 1 h ceiling, giving up after 24
    /// failures — roughly a day of retries for an entry that never lands.
    fn default() -> Self {
        Self {
            first_attempt_grace: super::relay::DEFAULT_RELAY_TIMEOUT,
            base: Duration::from_secs(30),
            ceiling: Duration::from_secs(3600),
            max_attempts: 24,
        }
    }
}

impl BackoffPolicy {
    /// Spacing required after `attempts` failures.
    ///
    /// `base << (attempts - 1)`, saturating into [`BackoffPolicy::ceiling`].
    /// The shift is bounded before it is applied, so a large attempt count
    /// cannot overflow into a small delay.
    ///
    /// Test: `backoff_spacing_grows_with_attempts`, `backoff_respects_the_ceiling`.
    pub fn delay_after(&self, attempts: u32) -> Duration {
        if attempts == 0 {
            return self.first_attempt_grace;
        }
        let shift = (attempts - 1).min(32);
        let scaled = self
            .base
            .as_millis()
            .saturating_mul(1u128 << shift)
            .min(self.ceiling.as_millis());
        Duration::from_millis(scaled.min(u128::from(u64::MAX)) as u64)
    }

    /// Whether `entry` may be relayed again at `now_unix_ms`.
    ///
    /// Why: the sweep's only admission test. Returning `false` leaves the entry
    /// exactly where it is — pending, durable, and visible to the health scan —
    /// so a "not due" entry is never a dropped one.
    /// What: `false` past [`BackoffPolicy::max_attempts`]; otherwise the elapsed
    /// time since the last attempt (or since receipt, for a never-attempted
    /// entry) must meet [`BackoffPolicy::delay_after`].
    /// Test: `backoff_holds_off_a_freshly_spooled_entry`,
    /// `backoff_stops_at_max_attempts`, `backoff_admits_an_entry_past_its_delay`.
    pub fn is_due(&self, entry: &SpoolEntry, now_unix_ms: u64) -> bool {
        if entry.attempts >= self.max_attempts {
            return false;
        }
        let since = entry
            .last_attempt_at_unix_ms
            .unwrap_or(entry.received_at_unix_ms);
        let elapsed_ms = u128::from(now_unix_ms.saturating_sub(since));
        elapsed_ms >= self.delay_after(entry.attempts).as_millis()
    }

    /// Whether `entry` has exhausted its retries and needs an operator.
    ///
    /// Distinguished from "not due yet" so the sweep can report the two
    /// separately — one resolves itself, the other never will.
    pub fn is_exhausted(&self, entry: &SpoolEntry) -> bool {
        entry.attempts >= self.max_attempts
    }
}
