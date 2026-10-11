//! [`SecretCache`]: one server session's cache of values read from a
//! CLI-backed backend (#7520).
//!
//! Why: each `op` or `keeper` read can cost an unlock prompt, and DOC-74
//! §8.4 asks for at most one unlock per project per session. Only the
//! CLI-backed sources are cached (§15.5); a Keychain or file read is cheap
//! and is never kept.
//! What: a mutex-guarded map from (backend, vault, key) to [`SecretValue`],
//! filled lazily by [`SecretCache::read_through`] with successful reads
//! only. [`SecretCache::evict`] drops a (vault, key) under every backend and
//! advances an epoch, so a read already in flight when a value changed can
//! never store the old value. Each backend's entries belong to the
//! [`OpenedUnder`] settings they were filled under, and to the account
//! config file's stat fingerprint; a read under other settings, or after
//! that file changed, drops them and advances the epoch the same way. An
//! entry as old as the max age, by `Instant` or by wall time, is never
//! served (#7520 S2). Each value is held in a [`Held`] buffer that is
//! zeroed when it leaves the map, however it leaves (#7520 S2). A poisoned
//! lock or unknown settings fail closed: every lookup misses and nothing is
//! stored. `Debug` shows the entry count and the max age only. The
//! cache is not a gate: [`super::SecretStore`] consults it only after the
//! grant, ancestry, agents-flag and audit checks have passed.
//! Test: the tests at the end of this file, and `session_cache_tests.rs`
//! beside `server_tests.rs`.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use zeroize::Zeroizing;

use super::config::{self, CliSettings};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// The CLI settings a backend was opened under: its section of the account
/// machine config, and that file's stat fingerprint.
///
/// Why: #7520 M1 — `backends_with` rereads that file on every open, so
/// `onepassword.account`, `keeper.config_path` or `program` can change
/// mid-session; a value read under one account must not answer under
/// another. #7520 S2 — the server reads the file before and after the open;
/// a file that went A→B→A in between reads the same both times, so the
/// fingerprint tells the two reads apart.
/// What: the section and the fingerprint, compared whole, so any change to
/// the file counts as other settings. No `Debug`, so it never prints.
/// Test: `cache_cleared_when_cli_account_settings_change`,
/// `cache_read_started_under_other_settings_is_not_stored`,
/// `cache_not_filled_across_account_file_flip`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OpenedUnder {
    settings: Option<CliSettings>,
    // #7520 S2: `None` when the file does not exist.
    file: Option<Fingerprint>,
}

impl OpenedUnder {
    /// `backend`'s section of the machine config at `path`, with the file's
    /// fingerprint.
    ///
    /// What: the fingerprint is taken before the file is read, so a change
    /// made after it shows in the next fingerprint. A missing file or section
    /// is a known "no settings"; no path, a file that cannot be stat'd, or one
    /// that does not load is `None`, which the cache treats as unknown (fail
    /// closed). Only `onepassword` and `keeper` have a section.
    pub(crate) fn read(path: Option<&Path>, backend: &BackendId) -> Option<Self> {
        let path = path?;
        // #7520 S2: stamp first; an unreadable stamp is unknown settings.
        let file = Fingerprint::of(path).ok()?;
        // #7520 M1: an unreadable config is unknown settings, never "none".
        let machine = config::load_machine_at(path).ok()?;
        let section = machine.and_then(|machine| match backend.as_str() {
            BackendId::ONEPASSWORD => machine.onepassword,
            BackendId::KEEPER => machine.keeper,
            _ => None,
        });
        Some(Self {
            settings: section,
            file,
        })
    }
}

/// One `stat` of a file: device, inode, length, mtime and ctime.
#[cfg_attr(not(unix), allow(dead_code, reason = "built on unix only"))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

/// The account config file as two [`Stamp`]s: the path itself (`lstat`) and
/// what it resolves to (`stat`), so replacing a symlink or its target both
/// show (#7520 S2).
///
/// What: a rename or rewrite changes the inode or ctime, which no caller can
/// set. A filesystem with coarse timestamps can miss an in-place rewrite of
/// the same length within one tick; DOC-74 §8.4 records that limit.
#[cfg_attr(not(unix), allow(dead_code, reason = "built on unix only"))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint([Stamp; 2]);

impl Fingerprint {
    /// `path`'s fingerprint; `Ok(None)` when it does not exist.
    #[cfg(unix)]
    fn of(path: &Path) -> io::Result<Option<Self>> {
        use std::os::unix::fs::MetadataExt;
        let stamp = |meta: std::fs::Metadata| Stamp {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.size(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        };
        let link = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let target = std::fs::metadata(path)?;
        Ok(Some(Self([stamp(link), stamp(target)])))
    }

    /// No fingerprint off unix: the settings are unknown, so the cache is
    /// bypassed (fail closed).
    #[cfg(not(unix))]
    fn of(_path: &Path) -> io::Result<Option<Self>> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// The cache's clock; [`Instant::now`] outside tests.
pub(crate) type CacheClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The cache's wall clock; [`SystemTime::now`] outside tests (#7520 S2).
pub(crate) type WallClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// What one cached value is filed under.
type Slot = (BackendId, VaultName, SecretKey);

/// One cached plaintext; zeroed when dropped.
///
/// Why: #7520 S2, owner ruling Dd8 — a value the cache lets go of must not
/// stay readable in freed memory. A [`SecretValue`] does not zero itself.
/// What: expiry, eviction, a settings change, a replacing insert and the
/// cache's own drop all drop the entry, and so zero these bytes.
/// Test: `cache_zeroizes_every_value_it_drops`,
/// `cached_value_type_zeroizes_on_drop`.
type Held = Zeroizing<String>;

/// When the read that filled an entry started, on both clocks (#7520 S2).
///
/// Why: `Instant` does not advance while the machine sleeps (macOS
/// `CLOCK_UPTIME_RAW`, Linux `CLOCK_MONOTONIC`), and the wall clock can be
/// stepped; each covers the other's gap.
/// What: [`Self::younger_than`] holds only while both ages are under the
/// bound; a wall clock now earlier than the fill fails closed.
/// Test: `entry_expires_after_max_age`,
/// `entry_expires_on_wall_time_while_instant_stands_still`.
#[derive(Clone, Copy)]
struct Filled {
    at: Instant,
    wall: SystemTime,
}

impl Filled {
    /// Whether an entry filled at `self` is still under `max_age` at `now`.
    fn younger_than(&self, now: &Filled, max_age: Duration) -> bool {
        // #7520 S2: a stepped-back wall clock is an unknown age, a miss.
        now.at.saturating_duration_since(self.at) < max_age
            && now
                .wall
                .duration_since(self.wall)
                .is_ok_and(|age| age < max_age)
    }
}

/// The map and the eviction epoch, under one lock.
#[derive(Default)]
struct Entries {
    /// Each value with the time the read that filled it started.
    values: HashMap<Slot, (Held, Filled)>,
    /// Advanced by every eviction; a read stores its value only when the
    /// epoch it started under is still current.
    epoch: u64,
    /// The settings each backend's entries were filled under (#7520 M1).
    opened_under: HashMap<BackendId, OpenedUnder>,
}

impl Entries {
    /// Make `under` the settings `backend`'s entries belong to.
    ///
    /// What: when they differ from the recorded ones, `backend`'s entries
    /// are dropped and the epoch advances, so a read in flight under the
    /// old settings stores nothing.
    fn adopt(&mut self, backend: &BackendId, under: &OpenedUnder) {
        if self.opened_under.get(backend) == Some(under) {
            return;
        }
        // #7520 M1: another account's values must never answer this one.
        self.epoch = self.epoch.wrapping_add(1);
        self.values.retain(|(held, _, _), _| held != backend);
        self.opened_under.insert(backend.clone(), under.clone());
    }
}

/// One server session's cache of CLI-backed values.
///
/// Why: see the module docs.
/// What: shared through an `Arc` in the server's `State`; lives as long as
/// the server process. Holds no value from a Keychain or file backend, and
/// no error.
/// Test: `resolve_second_call_does_not_reread_cli_backend`,
/// `cache_is_namespaced_per_vault`, `cache_debug_never_prints_value`.
pub(crate) struct SecretCache {
    entries: Mutex<Entries>,
    // #7520 S2: no entry answers once this old.
    max_age: Duration,
    now: CacheClock,
    wall: WallClock,
}

impl fmt::Debug for SecretCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // #7520 AC3: a count and the max age, never a key or a value.
        f.debug_struct("SecretCache")
            .field("entries", &self.count())
            .field("max_age", &self.max_age)
            .finish()
    }
}

impl SecretCache {
    /// An empty cache whose entries answer for at most `max_age`.
    pub(crate) fn new(max_age: Duration) -> Self {
        Self::with_clocks(max_age, Arc::new(Instant::now), Arc::new(SystemTime::now))
    }

    /// [`Self::new`] reading the time from `now` and the wall clock `wall`.
    pub(crate) fn with_clocks(max_age: Duration, now: CacheClock, wall: WallClock) -> Self {
        Self {
            entries: Mutex::default(),
            max_age,
            now,
            wall,
        }
    }

    /// Whether values read from `backend` are cached: `onepassword` and
    /// `keeper` only.
    ///
    /// Test: `keychain_and_file_reads_are_never_cached`.
    pub(crate) fn caches(backend: &BackendId) -> bool {
        matches!(backend.as_str(), BackendId::ONEPASSWORD | BackendId::KEEPER)
    }

    /// The lock, or `None` when a panic poisoned it.
    fn lock(&self) -> Option<MutexGuard<'_, Entries>> {
        // #7520: a poisoned map may be half-updated; never serve from it.
        self.entries.lock().ok()
    }

    /// `read`'s answer for (`backend`, `vault`, `key`), from the cache when
    /// it holds one filled under the settings `under`.
    ///
    /// Why: #7520 AC1 — the second read of a CLI-backed key in one session
    /// must not run the backend again.
    /// What: for a backend [`Self::caches`] refuses, or with `under` unknown
    /// (`None`), `read` runs every time and nothing is stored. Otherwise
    /// `backend`'s entries are first dropped if they were filled under other
    /// settings, then a cached value is returned and `read` is not called. On a
    /// miss `read` runs outside the lock, so one slow unlock does not block
    /// other keys; an `Ok(Some(value))` is stored if no eviction happened
    /// since the miss. An error and an `Ok(None)` are returned and never
    /// stored. With a poisoned lock, `read` runs and nothing is stored.
    /// #7520 S2: every entry as old as the max age is dropped before the
    /// lookup, so it is never served; a value's age counts from the start of
    /// the read that filled it, on both `Instant` and the wall clock
    /// ([`Filled`]).
    /// Test: `failed_read_is_not_cached`,
    /// `cache_read_started_before_an_eviction_is_not_stored`,
    /// `cache_read_started_under_other_settings_is_not_stored`,
    /// `poisoned_cache_misses_and_stores_nothing`,
    /// `entry_expires_after_max_age`,
    /// `entry_expires_on_wall_time_while_instant_stands_still`.
    pub(crate) fn read_through(
        &self,
        backend: &BackendId,
        under: Option<&OpenedUnder>,
        vault: &VaultName,
        key: &SecretKey,
        read: impl FnOnce() -> Result<Option<SecretValue>, SecretsError>,
    ) -> Result<Option<SecretValue>, SecretsError> {
        // #7520 M1: settings that could not be read or compared bypass it.
        let Some(under) = under.filter(|_| Self::caches(backend)) else {
            return read();
        };
        let slot = (backend.clone(), vault.clone(), key.clone());
        let now = Filled {
            at: (self.now)(),
            wall: (self.wall)(),
        };
        let epoch = match self.lock() {
            Some(mut entries) => {
                entries.adopt(backend, under);
                // #7520 S2: a busy server never idles out; age bounds a value.
                let max_age = self.max_age;
                entries
                    .values
                    .retain(|_, (_, filled)| filled.younger_than(&now, max_age));
                match entries.values.get(&slot) {
                    Some((held, _)) => return Ok(Some(SecretValue::new(held.as_str()))),
                    None => Some(entries.epoch),
                }
            }
            None => None,
        };
        let value = read()?;
        // #7520: a write evicted this key while the read ran when the epoch
        // moved; the value read may be the old one, so it is not kept.
        if let (Some(value), Some(epoch)) = (&value, epoch)
            && let Some(mut entries) = self.lock()
            && entries.epoch == epoch
        {
            // #7520 S2: the cache's own copy; a replaced one is zeroed.
            let held = Held::new(value.expose().to_owned());
            entries.values.insert(slot, (held, now));
        }
        Ok(value)
    }

    /// Drop `key` in `vault` under every backend.
    ///
    /// Why: #7520 — `secrets.set`, `secrets.delete` and `secrets.copy`
    /// change a value; the next resolve must read the backend again.
    /// What: removes every entry for (`vault`, `key`), whichever backend
    /// filled it, and advances the epoch so no read already running stores
    /// its value. With a poisoned lock nothing changes; every lookup then
    /// misses, so no stale value is served.
    /// Test: `set_delete_copy_evict_cached_value`,
    /// `cache_read_started_before_an_eviction_is_not_stored`.
    pub(crate) fn evict(&self, vault: &VaultName, key: &SecretKey) {
        if let Some(mut entries) = self.lock() {
            entries.epoch = entries.epoch.wrapping_add(1);
            entries
                .values
                .retain(|(_, held_vault, held_key), _| held_vault != vault || held_key != key);
        }
    }

    /// The number of cached values; 0 when the lock is poisoned.
    pub(crate) fn count(&self) -> usize {
        self.lock().map_or(0, |entries| entries.values.len())
    }
}

// #7520 S2: its own file, because it installs this test binary's allocator.
#[cfg(test)]
#[path = "cache_zeroize_tests.rs"]
mod zeroize_tests;

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;

    fn vault(name: &str) -> VaultName {
        VaultName::new(name).unwrap()
    }

    fn key(name: &str) -> SecretKey {
        SecretKey::new(name).unwrap()
    }

    fn found(value: &str) -> Result<Option<SecretValue>, SecretsError> {
        Ok(Some(SecretValue::new(value)))
    }

    /// Settings naming 1Password account `account`.
    fn account(account: &str) -> OpenedUnder {
        OpenedUnder {
            settings: Some(CliSettings {
                account: Some(account.to_owned()),
                ..CliSettings::default()
            }),
            file: None,
        }
    }

    /// Why: #7520 M1 — a read started under account A while a resolve under
    /// account B ran must not store A's value, and B never sees A's value;
    /// unknown settings read every time and store nothing.
    /// Test: itself.
    #[test]
    fn cache_read_started_under_other_settings_is_not_stored() {
        let cache = SecretCache::new(Duration::from_secs(600));
        let (op, web, name) = (BackendId::onepassword(), vault("trusty/acme/web"), key("K"));
        let (a, b) = (account("a"), account("b"));
        let raced = cache.read_through(&op, Some(&a), &web, &name, || {
            let inner = cache.read_through(&op, Some(&b), &web, &name, || found("b-value"));
            assert_eq!(inner.unwrap().unwrap().expose(), "b-value");
            found("a-value")
        });
        assert_eq!(raced.unwrap().unwrap().expose(), "a-value");
        let hit = cache.read_through(&op, Some(&b), &web, &name, || panic!("read on a hit"));
        assert_eq!(hit.unwrap().unwrap().expose(), "b-value");
        let mut reads = 0;
        for _ in 0..2 {
            let value = cache.read_through(&op, None, &web, &name, || {
                reads += 1;
                found("unknown")
            });
            assert_eq!(value.unwrap().unwrap().expose(), "unknown");
        }
        assert_eq!(reads, 2, "unknown settings were served from the cache");
        // #7520 S2 (LOW on #9660): the unknown reads stored nothing — B's
        // entry still answers, and it is the only one.
        let still_b = cache.read_through(&op, Some(&b), &web, &name, || panic!("read on a hit"));
        assert_eq!(still_b.unwrap().unwrap().expose(), "b-value");
        assert_eq!(cache.count(), 1, "an unknown-settings read was stored");
        let back = cache.read_through(&op, Some(&a), &web, &name, || found("a-again"));
        assert_eq!(back.unwrap().unwrap().expose(), "a-again");
        assert_eq!(cache.count(), 1, "account B's entry outlived the switch");
    }

    /// Why: #7520 — a value read before a write must not be stored after
    /// the write's eviction, or the old value would outlive the write.
    /// Test: itself.
    #[test]
    fn cache_read_started_before_an_eviction_is_not_stored() {
        let cache = SecretCache::new(Duration::from_secs(600));
        let (op, web, name) = (BackendId::onepassword(), vault("trusty/acme/web"), key("K"));
        let under = account("a");
        let stale = cache.read_through(&op, Some(&under), &web, &name, || {
            cache.evict(&web, &name);
            found("old")
        });
        assert_eq!(stale.unwrap().unwrap().expose(), "old");
        assert_eq!(cache.count(), 0, "a read that raced an eviction was stored");
        cache
            .read_through(&op, Some(&under), &web, &name, || found("new"))
            .unwrap();
        let hit = cache.read_through(&op, Some(&under), &web, &name, || panic!("read on a hit"));
        assert_eq!(hit.unwrap().unwrap().expose(), "new");
    }

    /// Why: #7520 — lock poisoning fails closed: a poisoned cache never
    /// serves a value and stores nothing.
    /// Test: itself.
    #[test]
    fn poisoned_cache_misses_and_stores_nothing() {
        let cache = SecretCache::new(Duration::from_secs(600));
        let (op, web, name) = (BackendId::onepassword(), vault("trusty/acme/web"), key("K"));
        let under = account("a");
        cache
            .read_through(&op, Some(&under), &web, &name, || found("v"))
            .unwrap();
        let poison = catch_unwind(AssertUnwindSafe(|| {
            let _held = cache.entries.lock();
            panic!("poison the cache");
        }));
        assert!(poison.is_err());

        let mut reads = 0;
        for _ in 0..2 {
            let value = cache.read_through(&op, Some(&under), &web, &name, || {
                reads += 1;
                found("fresh")
            });
            assert_eq!(value.unwrap().unwrap().expose(), "fresh");
        }
        assert_eq!(reads, 2, "a poisoned cache served a value");
        cache.evict(&web, &name);
        assert_eq!(
            format!("{cache:?}"),
            "SecretCache { entries: 0, max_age: 600s }"
        );
    }

    /// Why: #7520 S2 — `Instant` does not advance while the machine sleeps
    /// (macOS `CLOCK_UPTIME_RAW`, Linux `CLOCK_MONOTONIC`), so after a
    /// lid-close an entry would outlive its max age by `Instant` alone. An
    /// entry whose wall-clock age reaches the max age is a miss, and so is
    /// one whose wall clock stepped back (fail closed).
    /// Red while age is counted on `Instant` alone (one read for the run).
    /// Test: itself.
    #[test]
    fn entry_expires_on_wall_time_while_instant_stands_still() {
        let (instant, wall) = (Instant::now(), SystemTime::now());
        let offset = Arc::new(AtomicI64::new(0));
        let at = Arc::clone(&offset);
        let cache = SecretCache::with_clocks(
            Duration::from_secs(5),
            Arc::new(move || instant),
            Arc::new(move || {
                let secs = at.load(Ordering::SeqCst);
                let step = Duration::from_secs(secs.unsigned_abs());
                if secs < 0 { wall - step } else { wall + step }
            }),
        );
        let (op, web, name, under) = (
            BackendId::onepassword(),
            vault("trusty/acme/web"),
            key("K"),
            account("a"),
        );
        let mut reads = 0;
        // Asleep past the cap at 5 s; the wall clock steps back to 2 s.
        for (secs, expected) in [(0, 1), (4, 1), (5, 2), (9, 2), (2, 3), (3, 3)] {
            offset.store(secs, Ordering::SeqCst);
            let value = cache.read_through(&op, Some(&under), &web, &name, || {
                reads += 1;
                found("v")
            });
            assert_eq!(value.unwrap().unwrap().expose(), "v");
            assert_eq!(reads, expected, "backend reads at wall {secs}s");
        }
    }
}
