//! [`GrantCache`]: the values an unlock grant holds (#10524, owner ruling
//! Dd5).
//!
//! Why: every Keychain read evaluates the item's ACL, and after a
//! `cargo install` re-signs the binary each item prompts again; a CLI-backed
//! value ages out of the session cache after 600 s. After one `tm secrets
//! unlock` the owner wants no prompt for 8 h.
//! What: one optional grant window and the values read under it. A window
//! is live while its age is under its TTL on both `Instant` and the wall
//! clock, its watcher is alive, and no sleep gap was seen. Each check
//! records how the live window ended ([`DropReason`]), once, for the server
//! to audit ([`GrantCache::check`]). Values sit in [`LockedBuf`]s (zeroed on
//! drop, `mlock`'d; a refused lock stores nothing) and leave with the
//! window, by eviction, or with the cache. An epoch makes a read in flight
//! across a close, an eviction or a reopen store nothing. A poisoned lock
//! fails closed: it is cleared, the window ends, and nothing is served.
//! Nothing is ever written to disk. The cache is not a gate: the server
//! consults it only after every check on the read path has passed.
//! Test: `grant_cache_tests.rs` beside this file.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use super::cache::{CacheClock, OpenedUnder, WallClock};
use super::locked::{LockedBuf, MemLock, os_mlock};
use crate::api::{BackendId, SecretKey, SecretValue, VaultName};

/// How far the wall clock may run ahead of `Instant` between two checks
/// before the gap counts as a system sleep.
///
/// Why: `Instant` stops while the machine sleeps (macOS
/// `CLOCK_UPTIME_RAW`); the wall clock does not. A gap after wake is a
/// sleep the power watcher missed, so the grant drops (fail closed).
pub(crate) const SLEEP_GAP: Duration = Duration::from_secs(10);

/// Why a live grant ended. Typed, so an audit line never carries free text.
///
/// Test: `grant_drop_reasons_serialize_as_snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DropReason {
    /// The TTL ran out on either clock.
    Expired,
    /// The system went to sleep, or a sleep-sized clock gap was seen.
    Sleep,
    /// The login session ended (the server was told to shut down).
    Logout,
    /// `secrets.lock`, or a new unlock replacing the grant.
    Revoked,
    /// The watcher died, the clock stepped back, or the lock was poisoned.
    Fault,
    /// A reason a newer server records. Never written by this build.
    #[serde(other)]
    Other,
}

/// The grant's state after one [`GrantCache::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// No grant.
    Off,
    /// A live grant: its generation and the time left.
    Live {
        /// Which unlock opened it.
        generation: u64,
        /// Time left on the shorter clock.
        remaining: Duration,
    },
    /// A live grant ended during this check, or since the last one.
    Ended(DropReason),
}

/// A lookup's answer.
pub(crate) enum Lookup {
    /// No live grant, or a backend the grant does not cache: read as before.
    Off,
    /// The value, from the grant.
    Hit(SecretValue),
    /// Not held: read, then [`GrantCache::store`] under this epoch.
    Miss(u64),
}

/// What one value is filed under.
type Slot = (BackendId, VaultName, SecretKey);

/// A time on both clocks.
#[derive(Clone, Copy)]
struct Stamp {
    at: Instant,
    wall: SystemTime,
}

/// One live grant.
struct Window {
    generation: u64,
    opened: Stamp,
    last_seen: Stamp,
    ttl: Duration,
    /// Lowered when the grant's watcher stops, however it stops.
    watcher: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    window: Option<Window>,
    values: HashMap<Slot, (LockedBuf, Option<OpenedUnder>)>,
    epoch: u64,
    generation: u64,
    /// How the last live window ended, until a check reports it.
    ended: Option<DropReason>,
}

impl Inner {
    /// End the live window, if any, for `reason`: zero every value and
    /// advance the epoch. `true` when a window was live.
    fn end(&mut self, reason: DropReason) -> bool {
        self.epoch = self.epoch.wrapping_add(1);
        self.values.clear();
        let was_live = self.window.take().is_some();
        if was_live {
            self.ended = Some(reason);
        }
        was_live
    }

    /// Settle the window at `now`: end it when its watcher is gone, a clock
    /// stepped back, a sleep gap shows, or its TTL ran out on either clock.
    fn settle(&mut self, now: Stamp) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        let reason = if !window.watcher.load(Ordering::SeqCst) {
            Some(DropReason::Fault)
        } else {
            let wall_gap = now.wall.duration_since(window.last_seen.wall);
            let wall_age = now.wall.duration_since(window.opened.wall);
            let mono_gap = now.at.saturating_duration_since(window.last_seen.at);
            let mono_age = now.at.saturating_duration_since(window.opened.at);
            match (wall_gap, wall_age) {
                // #10524: a stepped-back wall clock is an unknown age.
                (Err(_), _) | (_, Err(_)) => Some(DropReason::Fault),
                (Ok(gap), _) if gap.saturating_sub(mono_gap) >= SLEEP_GAP => {
                    Some(DropReason::Sleep)
                }
                (_, Ok(age)) if age >= window.ttl || mono_age >= window.ttl => {
                    Some(DropReason::Expired)
                }
                _ => None,
            }
        };
        match reason {
            Some(reason) => {
                self.end(reason);
            }
            None => window.last_seen = now,
        }
    }

    /// The live window's time left at `now`, on the shorter clock.
    fn remaining(&self, now: Stamp) -> Option<(u64, Duration)> {
        let window = self.window.as_ref()?;
        let wall = now.wall.duration_since(window.opened.wall).ok()?;
        let mono = now.at.saturating_duration_since(window.opened.at);
        Some((window.generation, window.ttl.saturating_sub(wall.max(mono))))
    }
}

/// The unlock grant's window and values.
///
/// Why: see the module docs.
/// What: owned by the server's [`super::SecretCache`]; one per server
/// process, so a restart never restores a grant. `Debug` shows whether a
/// grant is live and the value count only.
/// Test: `grant_cache_serves_keychain_values_only_while_live`.
pub(crate) struct GrantCache {
    inner: Mutex<Inner>,
    now: CacheClock,
    wall: WallClock,
    lock_mem: MemLock,
}

impl fmt::Debug for GrantCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (live, values) = self.inner.lock().map_or((false, 0), |inner| {
            (inner.window.is_some(), inner.values.len())
        });
        f.debug_struct("GrantCache")
            .field("live", &live)
            .field("values", &values)
            .finish()
    }
}

impl GrantCache {
    /// An empty cache on `now` and `wall`, locking pages with `mlock`.
    pub(crate) fn new(now: CacheClock, wall: WallClock) -> Self {
        Self::with_lock(now, wall, os_mlock)
    }

    /// [`Self::new`] locking pages with `lock_mem`; tests refuse the lock.
    pub(crate) fn with_lock(now: CacheClock, wall: WallClock, lock_mem: MemLock) -> Self {
        Self {
            inner: Mutex::default(),
            now,
            wall,
            lock_mem,
        }
    }

    /// Whether the grant caches values read from `backend`: the Keychain and
    /// the CLI-backed backends.
    pub(crate) fn caches(backend: &BackendId) -> bool {
        matches!(
            backend.as_str(),
            BackendId::KEYCHAIN | BackendId::ONEPASSWORD | BackendId::KEEPER
        )
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            at: (self.now)(),
            wall: (self.wall)(),
        }
    }

    /// The lock; a poisoned one is cleared and its grant ended (fail closed).
    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => {
                // #10524: a half-updated map is never served from.
                let mut inner = poisoned.into_inner();
                inner.end(DropReason::Fault);
                self.inner.clear_poison();
                inner
            }
        }
    }

    /// Open a grant for `ttl`, watched by `watcher`; returns its generation.
    ///
    /// What: a grant already live ends first (recorded as `revoked`), so its
    /// values never carry over. The grant is live only while `watcher`
    /// stays `true`.
    pub(crate) fn open(&self, ttl: Duration, watcher: Arc<AtomicBool>) -> u64 {
        let now = self.stamp();
        let mut inner = self.lock();
        // #10524: the unlock record covers the replaced grant; the new
        // grant's watcher must not read the old one's ending as its own.
        inner.end(DropReason::Revoked);
        inner.ended = None;
        inner.generation = inner.generation.wrapping_add(1);
        inner.window = Some(Window {
            generation: inner.generation,
            opened: now,
            last_seen: now,
            ttl,
            watcher,
        });
        inner.generation
    }

    /// End the live grant for `reason`, when it is `generation` (or any
    /// grant, for `None`). `true` when a live grant ended here; the ending is
    /// then the caller's to audit and [`Self::check`] does not report it.
    pub(crate) fn close(&self, reason: DropReason, generation: Option<u64>) -> bool {
        let mut inner = self.lock();
        let matches = inner
            .window
            .as_ref()
            .is_some_and(|w| generation.is_none_or(|g| g == w.generation));
        if !matches {
            return false;
        }
        inner.end(reason);
        inner.ended = None;
        true
    }

    /// Settle the grant now and report it.
    ///
    /// What: [`Status::Ended`] once for each live grant that ended without a
    /// [`Self::close`] (expiry, a dead watcher, a sleep gap, a fault), so
    /// the server audits each drop exactly once.
    pub(crate) fn check(&self) -> Status {
        let now = self.stamp();
        let mut inner = self.lock();
        inner.settle(now);
        if let Some(reason) = inner.ended.take() {
            return Status::Ended(reason);
        }
        match inner.remaining(now) {
            Some((generation, remaining)) => Status::Live {
                generation,
                remaining,
            },
            None => Status::Off,
        }
    }

    /// The grant's answer for (`backend`, `vault`, `key`) read under `under`.
    ///
    /// What: [`Lookup::Off`] with no live grant, for a backend
    /// [`Self::caches`] refuses, or for a CLI backend whose settings are
    /// unknown. A held value filed under other CLI settings is a miss.
    pub(crate) fn lookup(
        &self,
        backend: &BackendId,
        under: Option<&OpenedUnder>,
        vault: &VaultName,
        key: &SecretKey,
    ) -> Lookup {
        let keychain = backend.as_str() == BackendId::KEYCHAIN;
        if !Self::caches(backend) || (!keychain && under.is_none()) {
            return Lookup::Off;
        }
        let now = self.stamp();
        let mut inner = self.lock();
        inner.settle(now);
        if inner.window.is_none() {
            return Lookup::Off;
        }
        let slot = (backend.clone(), vault.clone(), key.clone());
        match inner.values.get(&slot) {
            Some((held, filed)) if keychain || filed.as_ref() == under => {
                Lookup::Hit(SecretValue::new(held.as_str()))
            }
            _ => Lookup::Miss(inner.epoch),
        }
    }

    /// Keep `value` for (`backend`, `vault`, `key`) when the grant is still
    /// live and nothing moved the epoch since `epoch`. `true` when kept; a
    /// refused page lock keeps nothing.
    pub(crate) fn store(
        &self,
        backend: &BackendId,
        under: Option<&OpenedUnder>,
        vault: &VaultName,
        key: &SecretKey,
        epoch: u64,
        value: &SecretValue,
    ) -> bool {
        let now = self.stamp();
        let mut inner = self.lock();
        inner.settle(now);
        if inner.window.is_none() || inner.epoch != epoch {
            return false;
        }
        // #10524 AC2: no lock, no cache; the read is served uncached.
        let Some(held) = LockedBuf::new(value.expose(), self.lock_mem) else {
            return false;
        };
        let slot = (backend.clone(), vault.clone(), key.clone());
        let filed = (backend.as_str() != BackendId::KEYCHAIN)
            .then(|| under.cloned())
            .flatten();
        inner.values.insert(slot, (held, filed));
        true
    }

    /// Drop `key` in `vault` under every backend and advance the epoch.
    pub(crate) fn evict(&self, vault: &VaultName, key: &SecretKey) {
        let mut inner = self.lock();
        inner.epoch = inner.epoch.wrapping_add(1);
        inner
            .values
            .retain(|(_, held_vault, held_key), _| held_vault != vault || held_key != key);
    }

    /// The number of held values.
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.lock().values.len()
    }
}

#[cfg(test)]
#[path = "grant_cache_tests.rs"]
mod tests;
