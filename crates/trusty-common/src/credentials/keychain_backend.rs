//! Group-namespaced OS-keychain backend for `tm secrets` (issue #7521,
//! [DOC-74](../../../../../docs/specs/DOC-74-secrets-integration.md) §6.3/§8.1),
//! feature `keyring-store`.
//!
//! Why: [`super::KeyringStore`] is single-namespace — one fixed service
//! (`"trusty-tools"`), one flat account per provider — and its `list()` can
//! never enumerate, because the `keyring` crate exposes no portable
//! "list every account for a service" API. `tm secrets` needs the opposite
//! shape: one namespace per project, listable by name. This module supplies
//! exactly that much and no more; multi-backend selection (1Password, Keeper)
//! is #7519's, not this file's.
//!
//! What: [`KeychainBackend`] composes a keychain entry as service
//! `trusty/<owner>/<repo>` (the *group*, DOC-74 §6.3) with the bare KEY as the
//! account, and keeps a `0600` **names-only** index beside it at
//! `~/.trusty-tools/trusty-mpm/secrets-index/<group>.json` so `list` has
//! something to enumerate. The index never holds a value — only names, which
//! are not secret (T-2). Every write path is fail-closed: an unreachable
//! keychain, an unparsable index, or an invalid group/key is an error, never a
//! warning-and-continue. Every index mutation runs under the cross-process
//! advisory lock `json_rmw`/`file_lock` already own, so concurrent
//! `tm secrets` processes cannot lose a row or publish a half-written file.
//! The backing store is the [`KeyStore`] trait, so a test can inject
//! [`super::MemoryKeyStore`] and no unit test ever touches a real keychain.
//!
//! Test: the sibling keychain_backend_tests.rs file —
//! `keychain_backend_namespaces_by_group_and_key`,
//! `keychain_backend_list_reads_index_names_only`,
//! `keychain_backend_remove_deletes_entry_and_index_row`,
//! `keychain_backend_debug_never_contains_a_value`,
//! `keychain_backend_concurrent_writers_never_lose_a_name`,
//! `keychain_backend_index_write_publishes_by_rename`, and the `#[ignore]`
//! `keychain_backend_real_roundtrip_add_list_remove` against the real
//! keychain.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::file_store::write_owner_only;
use super::{KeyStore, KeyStoreError, KeyringStore};

/// Keychain service prefix every `tm secrets` group lives under (DOC-74 §6.3).
///
/// The full service name is `trusty/<group>`, e.g.
/// `trusty/bobmatnyc/trusty-tools`; the account is the bare KEY.
pub const SERVICE_PREFIX: &str = "trusty";

/// Directory (under `$HOME`) holding one names-only index per group.
const INDEX_SUBDIR: &str = ".trusty-tools/trusty-mpm/secrets-index";

/// Compose the keychain service name for `group` (DOC-74 §6.3).
///
/// Why: the naming convention has to be one function, not a format string
/// repeated at each call site, so slice 2's backends (#7519) can match it.
/// What: `trusty/<group>`, where `group` is the `<owner>/<repo>` identity.
/// Test: `keychain_backend_namespaces_by_group_and_key`.
pub fn keychain_service_name(group: &str) -> String {
    format!("{SERVICE_PREFIX}/{group}")
}

/// Reject a group that could escape the index directory or name no vault.
///
/// Why: the group becomes both a keychain service name and a path segment
/// under [`INDEX_SUBDIR`]; a `..` segment or an absolute form would write the
/// index outside the directory it is confined to.
/// What: requires a non-empty group of `[A-Za-z0-9._-]` segments separated by
/// `/`, with no empty, `.`, or `..` segment. Errors are
/// [`KeyStoreError::Keyring`] — the backend refused the operation — and never
/// echo anything but the group, which is not secret.
/// Test: `keychain_backend_rejects_a_traversing_group`.
pub fn validate_group(group: &str) -> Result<(), KeyStoreError> {
    let invalid = |why: &str| {
        Err(KeyStoreError::Keyring(format!(
            "invalid secrets group {group:?}: {why}"
        )))
    };
    if group.is_empty() {
        return invalid("empty");
    }
    for segment in group.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return invalid("every `/`-separated segment must be a plain name");
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return invalid("segments may hold only [A-Za-z0-9._-]");
        }
    }
    Ok(())
}

/// Reject a key name that is empty or carries whitespace/`/`.
///
/// Why: the key is the keychain account and an index row; a blank or
/// whitespace-bearing name makes both unaddressable.
/// What: non-empty, no ASCII whitespace, no `/`.
/// Test: `keychain_backend_rejects_a_blank_key`.
pub fn validate_key(key: &str) -> Result<(), KeyStoreError> {
    if key.is_empty() || key.chars().any(|c| c.is_whitespace() || c == '/') {
        return Err(KeyStoreError::Keyring(format!(
            "invalid secret key {key:?}: must be non-empty with no whitespace or `/`"
        )));
    }
    Ok(())
}

/// Index path for `group` under an explicit base (test seam for `$HOME`).
///
/// Test: `keychain_backend_list_reads_index_names_only`.
pub fn index_path_at(base: &Path, group: &str) -> PathBuf {
    base.join(INDEX_SUBDIR).join(format!("{group}.json"))
}

/// Index path for `group` under the real `$HOME`.
///
/// Test: covered indirectly by `keychain_backend_real_roundtrip_add_list_remove`.
pub fn default_index_path(group: &str) -> Result<PathBuf, KeyStoreError> {
    validate_group(group)?;
    let home = dirs::home_dir().ok_or(KeyStoreError::HomeUnavailable)?;
    Ok(index_path_at(&home, group))
}

/// The on-disk index: key **names** only, never a value (DOC-74 §8.1).
#[derive(Debug, Default, Serialize, Deserialize)]
struct SecretsIndex {
    /// The group these names belong to, for a human reading the file.
    #[serde(default)]
    group: String,
    /// Stored key names, sorted and deduplicated on every write.
    #[serde(default)]
    names: Vec<String>,
}

/// A [`KeyStore`] over one caller-chosen keychain service.
///
/// Why: [`KeyringStore`] hard-codes its service name, so it cannot express a
/// per-group namespace. This is the same logic with the service supplied at
/// construction, reusing `KeyringStore`'s process-wide probe rather than
/// running a second one.
/// What: `get`/`set`/`unset` against `keyring::Entry::new(&self.service, key)`;
/// `list` is empty for the same reason `KeyringStore::list` is — enumeration
/// lives in the index file, not here.
/// Test: `keychain_backend_real_roundtrip_add_list_remove` (ignored).
struct KeyringServiceStore {
    service: String,
}

impl KeyringServiceStore {
    fn new(service: String) -> Self {
        Self { service }
    }

    fn require_available(&self) -> Result<(), KeyStoreError> {
        if KeyringStore::new().probe_available() {
            Ok(())
        } else {
            Err(KeyStoreError::Keyring(
                "keychain backend unavailable".to_string(),
            ))
        }
    }
}

impl KeyStore for KeyringServiceStore {
    fn get(&self, provider: &str) -> Option<String> {
        if self.require_available().is_err() {
            return None;
        }
        keyring::Entry::new(&self.service, provider)
            .ok()?
            .get_password()
            .ok()
    }

    fn set(&self, provider: &str, value: &str) -> Result<(), KeyStoreError> {
        self.require_available()?;
        keyring::Entry::new(&self.service, provider)
            .map_err(|e| KeyStoreError::Keyring(e.to_string()))?
            .set_password(value)
            .map_err(|e| KeyStoreError::Keyring(e.to_string()))
    }

    fn unset(&self, provider: &str) -> Result<(), KeyStoreError> {
        self.require_available()?;
        let entry = keyring::Entry::new(&self.service, provider)
            .map_err(|e| KeyStoreError::Keyring(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeyStoreError::Keyring(e.to_string())),
        }
    }

    fn list(&self) -> Vec<String> {
        Vec::new()
    }
}

/// One project group's keychain vault plus its names-only index.
///
/// Why: see the module docs — a listable, per-project namespace over a
/// backend that can neither namespace nor enumerate on its own.
/// What: holds the group, the index path, and the backing [`KeyStore`]
/// (the real keychain in production, an injected double in tests). Every
/// mutation writes the keychain first and the index second, so an index row
/// can never claim a key the backend refused.
/// Test: the four `keychain_backend_*` tests named in the module docs.
pub struct KeychainBackend {
    group: String,
    index_path: PathBuf,
    store: Arc<dyn KeyStore>,
}

/// Value-free by construction: no field can hold a secret, and the backing
/// store is rendered as its service name only.
///
/// Test: `keychain_backend_debug_never_contains_a_value`.
impl fmt::Debug for KeychainBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeychainBackend")
            .field("group", &self.group)
            .field("service", &self.service())
            .field("index_path", &self.index_path)
            .finish_non_exhaustive()
    }
}

impl KeychainBackend {
    /// Open `group`'s vault against the real OS keychain.
    ///
    /// Why: the production constructor — the one `tm secrets` calls.
    /// What: validates the group, resolves the index path under `$HOME`, and
    /// binds a [`KeyringServiceStore`] on service `trusty/<group>`. Does not
    /// probe the keychain (see [`Self::keychain_reachable`]); an unreachable
    /// backend surfaces at the first `get`/`set`/`remove`.
    /// Test: `keychain_backend_real_roundtrip_add_list_remove` (ignored).
    pub fn new(group: &str) -> Result<Self, KeyStoreError> {
        let index_path = default_index_path(group)?;
        Ok(Self {
            group: group.to_string(),
            index_path,
            store: Arc::new(KeyringServiceStore::new(keychain_service_name(group))),
        })
    }

    /// Open `group`'s vault against an injected store and index path.
    ///
    /// Why: every unit test runs here, against [`super::MemoryKeyStore`] and a
    /// temp-dir index, so no test can write to a real keychain.
    /// What: same validation as [`Self::new`]; the caller supplies both seams.
    /// Test: all four `keychain_backend_*` unit tests.
    pub fn with_store(
        group: &str,
        store: Arc<dyn KeyStore>,
        index_path: PathBuf,
    ) -> Result<Self, KeyStoreError> {
        validate_group(group)?;
        Ok(Self {
            group: group.to_string(),
            index_path,
            store,
        })
    }

    /// The group this vault is namespaced by.
    pub fn group(&self) -> &str {
        &self.group
    }

    /// The keychain service name this vault writes under.
    pub fn service(&self) -> String {
        keychain_service_name(&self.group)
    }

    /// Where this vault's names-only index lives.
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// Whether the OS keychain answers a sentinel probe.
    ///
    /// Why: `tm secrets doctor` reports reachability without creating,
    /// reading, or unlocking any real secret.
    /// What: delegates to [`KeyringStore::probe_available`], whose sentinel
    /// account can never collide with a stored credential and whose result is
    /// cached process-wide.
    /// Test: `probe_does_not_panic` pins the probe itself; trusty-mpm's
    /// secrets_doctor_reports_probe_result_without_prompting covers what
    /// `tm secrets doctor` renders from the result.
    pub fn keychain_reachable() -> bool {
        KeyringStore::new().probe_available()
    }

    /// Store `value` under `key`, then record the name in the index.
    ///
    /// Why: `tm secrets add`'s only write path.
    /// What: validates the key, writes the keychain entry, and only then adds
    /// the name to the `0600` index under the cross-process lock
    /// ([`Self::update_index`]) — a refused backend write leaves the index
    /// untouched. `value` is never logged, returned, or placed in an error.
    /// Test: `keychain_backend_namespaces_by_group_and_key`,
    /// `keychain_backend_debug_never_contains_a_value`,
    /// `keychain_backend_concurrent_writers_never_lose_a_name`.
    pub fn set(&self, key: &str, value: &str) -> Result<(), KeyStoreError> {
        validate_key(key)?;
        self.store.set(key, value)?;
        self.update_index(|names| {
            if !names.iter().any(|n| n == key) {
                names.push(key.to_string());
            }
        })
    }

    /// Read `key`'s value, or `None` when it is absent/unreachable.
    ///
    /// Why: slice 1 has no `tm secrets get` verb — this exists so the ignored
    /// real-keychain round-trip can prove the entry it wrote is retrievable.
    /// What: validates the key, then defers to the backing store, which
    /// returns `None` on any miss or backend failure by [`KeyStore`] contract.
    /// Test: `keychain_backend_real_roundtrip_add_list_remove` (ignored).
    pub fn get(&self, key: &str) -> Result<Option<String>, KeyStoreError> {
        validate_key(key)?;
        Ok(self.store.get(key))
    }

    /// Every stored key **name**, sorted — never a value.
    ///
    /// Why: the keychain cannot enumerate, so `tm secrets list` reads the
    /// index instead.
    /// What: parses the `0600` index; a missing file is an empty vault, a
    /// corrupt one is an error (fail-closed, never a silent reset).
    /// Test: `keychain_backend_list_reads_index_names_only`.
    pub fn list(&self) -> Result<Vec<String>, KeyStoreError> {
        let mut names = self.read_index()?.names;
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Delete `key` from the keychain and from the index.
    ///
    /// Why: `tm secrets remove`'s only path.
    /// What: validates the key, deletes the entry (absent is not an error, per
    /// [`KeyStore::unset`]), then drops the index row. A refused delete leaves
    /// the index row in place, so `list` never under-reports what is stored.
    /// Test: `keychain_backend_remove_deletes_entry_and_index_row`.
    pub fn remove(&self, key: &str) -> Result<(), KeyStoreError> {
        validate_key(key)?;
        self.store.unset(key)?;
        self.update_index(|names| names.retain(|n| n != key))
    }

    /// Read-modify-write the index's name list under the cross-process lock.
    ///
    /// Why: #7521 — `set` and `remove` are each a read → mutate → write cycle
    /// over ONE file that any number of `tm secrets` PROCESSES can run at
    /// once. Unserialised, two writers interleave read/read/write/write and
    /// one name is lost while its keychain entry survives, so `list`
    /// under-reports what is stored. An in-process `Mutex` cannot fix that:
    /// the writers are separate processes.
    /// What: takes the exclusive advisory lock on the index's `.lock` sidecar
    /// via [`crate::file_lock::with_exclusive_lock`] — the same primitive
    /// `json_rmw` uses for `projects.json` — then re-reads the index from disk
    /// UNDER that lock (never trusting a copy read before it), applies
    /// `mutate`, and publishes via [`Self::write_index`]. `json_rmw::update`
    /// itself is not reusable here: it publishes with `File::create`, which
    /// would widen the index past `0600`.
    ///
    /// Errors: a lock that cannot be created or is still held after
    /// [`crate::file_lock::DEFAULT_LOCK_TIMEOUT`] is a
    /// [`KeyStoreError::Io`] naming the sidecar — fail-closed, never a write
    /// that skipped the lock.
    /// Test: `keychain_backend_concurrent_writers_never_lose_a_name`.
    fn update_index(&self, mutate: impl FnOnce(&mut Vec<String>)) -> Result<(), KeyStoreError> {
        let locked = crate::file_lock::with_exclusive_lock(&self.index_path, || {
            let mut index = self.read_index()?;
            mutate(&mut index.names);
            self.write_index(index)
        });
        match locked {
            Ok(result) => result,
            Err(source) => Err(KeyStoreError::Io {
                path: crate::file_lock::lock_path(&self.index_path),
                source,
            }),
        }
    }

    /// Parse the index file; a missing file is an empty index.
    fn read_index(&self) -> Result<SecretsIndex, KeyStoreError> {
        let raw = match std::fs::read_to_string(&self.index_path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SecretsIndex {
                    group: self.group.clone(),
                    names: Vec::new(),
                });
            }
            Err(e) => {
                return Err(KeyStoreError::Io {
                    path: self.index_path.clone(),
                    source: e,
                });
            }
        };
        serde_json::from_str(&raw).map_err(|e| KeyStoreError::Io {
            path: self.index_path.clone(),
            // Sanitized: position only, never the offending bytes.
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "secrets index parse error at line {} column {}",
                    e.line(),
                    e.column()
                ),
            ),
        })
    }

    /// Write the index back at `0600`, atomically, creating its directory if
    /// needed.
    ///
    /// Why: a crash mid-write would otherwise leave truncated JSON at the
    /// final path, which [`Self::read_index`] then refuses forever — a vault
    /// that lists nothing and refuses every add. Same tmp-then-rename shape
    /// `FileKeyStore::write` uses for the credentials file.
    /// What: renders, writes to a per-attempt scratch path ([`scratch_path`])
    /// at `0600` from birth, then renames onto the final path. `rename(2)`
    /// within a filesystem is atomic, so a reader sees the whole previous
    /// index or the whole new one. A failed scratch write leaves the previous
    /// index untouched.
    /// Test: `keychain_backend_index_write_publishes_by_rename`.
    fn write_index(&self, mut index: SecretsIndex) -> Result<(), KeyStoreError> {
        index.group = self.group.clone();
        index.names.sort();
        index.names.dedup();
        if let Some(parent) = self.index_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| KeyStoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let json = serde_json::to_string_pretty(&index).map_err(|e| KeyStoreError::Io {
            path: self.index_path.clone(),
            source: std::io::Error::other(e.to_string()),
        })?;
        let tmp = scratch_path(&self.index_path);
        write_owner_only(&tmp, &json)?;
        std::fs::rename(&tmp, &self.index_path).map_err(|e| {
            // Never leave a half-written scratch file behind.
            let _ = std::fs::remove_file(&tmp);
            KeyStoreError::Io {
                path: self.index_path.clone(),
                source: e,
            }
        })
    }
}

/// Scratch path for ONE publish attempt — unique per writer and per attempt.
///
/// Why: #7521 — the fixed `<index>.json.tmp` this replaced was shared by every
/// process. Two `tm secrets add` runs wrote it at once and each renamed it over
/// the real index, so one rename hit `ENOENT` and the survivor published a
/// document assembled from both writers. Uniqueness removes that class of
/// failure independently of [`KeychainBackend::update_index`]'s lock, the same
/// reasoning `json_rmw::temp_path` records for `projects.json`.
/// What: `<file_name>.<pid>.<nanos>.tmp`, alongside the target so the publish
/// stays a same-filesystem `rename`.
/// Test: `keychain_backend_index_write_publishes_by_rename`,
/// `keychain_backend_concurrent_writers_never_lose_a_name`.
fn scratch_path(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{nanos}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
#[path = "keychain_backend_tests.rs"]
mod tests;
