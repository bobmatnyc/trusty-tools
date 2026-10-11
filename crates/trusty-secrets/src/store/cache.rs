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
//! [`OpenedUnder`] settings they were filled under; a read under other
//! settings drops them and advances the epoch the same way. A poisoned lock
//! or unknown settings fail closed: every lookup misses and nothing is
//! stored. `Debug` shows the entry count only. The
//! cache is not a gate: [`super::SecretStore`] consults it only after the
//! grant, ancestry, agents-flag and audit checks have passed.
//! Test: the tests at the end of this file, and `session_cache_tests.rs`
//! beside `server_tests.rs`.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use super::config::{self, CliSettings};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// The CLI settings a backend was opened under: its section of the account
/// machine config.
///
/// Why: #7520 M1 — `backends_with` rereads that file on every open, so
/// `onepassword.account`, `keeper.config_path` or `program` can change
/// mid-session; a value read under one account must not answer under
/// another.
/// What: the section, compared whole. No `Debug`, so it never prints.
/// Test: `cache_cleared_when_cli_account_settings_change`,
/// `cache_read_started_under_other_settings_is_not_stored`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OpenedUnder(Option<CliSettings>);

impl OpenedUnder {
    /// `backend`'s section of the machine config at `path`.
    ///
    /// What: a missing file or section is a known "no settings"; no path or
    /// a file that does not load is `None`, which the cache treats as
    /// unknown (fail closed). Only `onepassword` and `keeper` have a section.
    pub(crate) fn read(path: Option<&Path>, backend: &BackendId) -> Option<Self> {
        // #7520 M1: an unreadable config is unknown settings, never "none".
        let machine = config::load_machine_at(path?).ok()?;
        let section = machine.and_then(|machine| match backend.as_str() {
            BackendId::ONEPASSWORD => machine.onepassword,
            BackendId::KEEPER => machine.keeper,
            _ => None,
        });
        Some(Self(section))
    }
}

/// What one cached value is filed under.
type Slot = (BackendId, VaultName, SecretKey);

/// The map and the eviction epoch, under one lock.
#[derive(Default)]
struct Entries {
    values: HashMap<Slot, SecretValue>,
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
#[derive(Default)]
pub(crate) struct SecretCache {
    entries: Mutex<Entries>,
}

impl fmt::Debug for SecretCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // #7520 AC3: a count, never a key or a value.
        f.debug_struct("SecretCache")
            .field("entries", &self.count())
            .finish()
    }
}

impl SecretCache {
    /// An empty cache.
    pub(crate) fn new() -> Self {
        Self::default()
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
    /// Test: `failed_read_is_not_cached`,
    /// `cache_read_started_before_an_eviction_is_not_stored`,
    /// `cache_read_started_under_other_settings_is_not_stored`,
    /// `poisoned_cache_misses_and_stores_nothing`.
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
        let epoch = match self.lock() {
            Some(mut entries) => {
                entries.adopt(backend, under);
                match entries.values.get(&slot) {
                    Some(value) => return Ok(Some(value.clone())),
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
            entries.values.insert(slot, value.clone());
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

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

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
        OpenedUnder(Some(CliSettings {
            account: Some(account.to_owned()),
            ..CliSettings::default()
        }))
    }

    /// Why: #7520 M1 — a read started under account A while a resolve under
    /// account B ran must not store A's value, and B never sees A's value;
    /// unknown settings read every time and store nothing.
    /// Test: itself.
    #[test]
    fn cache_read_started_under_other_settings_is_not_stored() {
        let cache = SecretCache::new();
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
        let back = cache.read_through(&op, Some(&a), &web, &name, || found("a-again"));
        assert_eq!(back.unwrap().unwrap().expose(), "a-again");
        assert_eq!(cache.count(), 1, "account B's entry outlived the switch");
    }

    /// Why: #7520 — a value read before a write must not be stored after
    /// the write's eviction, or the old value would outlive the write.
    /// Test: itself.
    #[test]
    fn cache_read_started_before_an_eviction_is_not_stored() {
        let cache = SecretCache::new();
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
        let cache = SecretCache::new();
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
        assert_eq!(format!("{cache:?}"), "SecretCache { entries: 0 }");
    }
}
