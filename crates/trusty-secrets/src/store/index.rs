//! The names-only index: which keys each vault holds, never their values.
//!
//! Why: the OS keychain cannot enumerate a service's accounts portably, and
//! `list` must never read values to learn names. The index is the listable
//! record. It also carries per-key metadata no backend stores: value length,
//! `updated_at`, and the "agents may use" flag (DOC-74 §15.3, §15.8).
//! What: one JSON file per vault under
//! `~/.trusty-tools/trusty-secrets/index/` (root injectable), named by
//! [`VaultName::file_stem`]. Files are 0600 in a 0700 directory, published by
//! temp + rename under an exclusive cross-process lock. A file that does not
//! parse, names another vault, carries an unknown version or field, or holds
//! an invalid key is [`SecretsError::IndexCorrupt`] — never reset.
//! Test: `index_tests.rs` beside this file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::platform;
use crate::api::methods::{KeyMeta, SetOutcome};
use crate::api::{SecretKey, SecretsError, VaultName};

/// Index directory under `$HOME`.
pub const INDEX_SUBDIR: &str = ".trusty-tools/trusty-secrets/index";

/// The on-disk format version this build reads and writes.
const INDEX_VERSION: u32 = 1;

/// How long a writer waits for the index lock by default.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// One vault's index file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexFile {
    version: u32,
    vault: String,
    keys: BTreeMap<String, IndexRow>,
}

/// One key's metadata. There is no field that could hold a value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexRow {
    length: usize,
    updated_at: u64,
    #[serde(default)]
    agents_may_use: bool,
}

impl IndexFile {
    fn empty(vault: &VaultName) -> Self {
        Self {
            version: INDEX_VERSION,
            vault: vault.to_string(),
            keys: BTreeMap::new(),
        }
    }
}

/// The names-only index rooted at one directory.
///
/// Why: see the module docs.
/// What: stateless apart from its root and lock bound; every call reads the
/// file fresh, so two processes always see each other's published writes.
/// Test: `index_tests.rs`.
#[derive(Debug, Clone)]
pub struct NamesIndex {
    root: PathBuf,
    lock_timeout: Duration,
}

impl NamesIndex {
    /// An index rooted at `root`. Tests pass a temp directory.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    /// The index at `~/.trusty-tools/trusty-secrets/index/`.
    ///
    /// Test: covered by the ignored `keychain_real_roundtrip_store_list_remove`
    /// through [`NamesIndex::at`]; this only resolves `$HOME`.
    pub fn default_location() -> Result<Self, SecretsError> {
        Ok(Self::at(platform::home_dir()?.join(INDEX_SUBDIR)))
    }

    /// Replace the lock wait bound.
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    /// The index root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file holding `vault`'s rows.
    ///
    /// Test: `index_file_names_are_flat_and_distinct`.
    pub fn path_for(&self, vault: &VaultName) -> PathBuf {
        self.root.join(format!("{}.json", vault.file_stem()))
    }

    /// Every key in `vault`, sorted by name. A missing file is an empty vault.
    ///
    /// Test: `index_rows_round_trip_without_plaintext`,
    /// `index_corrupt_file_fails_closed_and_is_never_reset`.
    pub fn list(&self, vault: &VaultName) -> Result<Vec<KeyMeta>, SecretsError> {
        let path = self.path_for(vault);
        let file = read_file(&path, vault)?;
        file.keys
            .iter()
            .map(|(name, row)| to_meta(&path, name, row))
            .collect()
    }

    /// One key's row, or `None` when the vault does not index it.
    ///
    /// Test: `index_rows_round_trip_without_plaintext`.
    pub fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<KeyMeta>, SecretsError> {
        let path = self.path_for(vault);
        let file = read_file(&path, vault)?;
        file.keys
            .get(key.as_str())
            .map(|row| to_meta(&path, key.as_str(), row))
            .transpose()
    }

    /// Record a write of `length` characters to `key` at `now`.
    ///
    /// What: a new row starts with "agents may use" OFF; an existing row keeps
    /// its flag. Returns whether the row was new.
    /// Test: `index_upsert_preserves_the_agents_flag`.
    pub fn upsert(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        length: usize,
        now: u64,
    ) -> Result<SetOutcome, SecretsError> {
        self.upsert_with(vault, key, length, now, |_| Ok(()))
    }

    /// [`NamesIndex::upsert`], running `write` under the index lock first.
    ///
    /// Why: #9064 — the backend write must happen only once the index is
    /// known to be readable and is locked. Writing the backend first left an
    /// orphaned Keychain entry whenever the index was corrupt or locked.
    /// What: takes the lock, re-reads the index (a corrupt file or a lock
    /// timeout fails here, before `write` runs), calls `write` with the
    /// outcome the upsert will have, and only on its success mutates and
    /// publishes the row. A `write` error leaves the index untouched. A
    /// publish error after `write` succeeded is returned to the caller, which
    /// owns any compensation.
    /// Test: `store_set_fails_closed_on_the_index_before_the_backend_write`,
    /// `store_set_compensates_a_new_key_when_the_index_publish_fails`.
    pub(crate) fn upsert_with(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        length: usize,
        now: u64,
        write: impl FnOnce(SetOutcome) -> Result<(), SecretsError>,
    ) -> Result<SetOutcome, SecretsError> {
        self.update(vault, |file| {
            let outcome = if file.keys.contains_key(key.as_str()) {
                SetOutcome::Updated
            } else {
                SetOutcome::New
            };
            write(outcome)?;
            let row = file.keys.entry(key.to_string()).or_insert(IndexRow {
                length,
                updated_at: now,
                agents_may_use: false,
            });
            row.length = length;
            row.updated_at = now;
            Ok(outcome)
        })
    }

    /// Drop `key`'s row. Returns whether a row existed.
    ///
    /// Test: `store_delete_removes_entry_and_row`.
    pub fn remove(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.update(vault, |file| Ok(file.keys.remove(key.as_str()).is_some()))
    }

    /// Set the "agents may use" flag on an indexed key.
    ///
    /// What: a key the vault does not index is [`SecretsError::NotFound`].
    /// Test: `index_upsert_preserves_the_agents_flag`.
    pub fn set_agents_may_use(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        self.update(vault, |file| match file.keys.get_mut(key.as_str()) {
            Some(row) => {
                row.agents_may_use = allowed;
                Ok(())
            }
            None => Err(SecretsError::NotFound {
                key: key.to_string(),
                searched: vault.to_string(),
            }),
        })
    }

    /// Read-modify-write `vault`'s file under the exclusive lock.
    ///
    /// Why: separate processes write the same file; the re-read happens under
    /// the lock so no writer publishes over a row it never saw.
    /// What: ensures the 0700 root, locks, re-reads (a corrupt file aborts
    /// here, before any write), applies `mutate`, and publishes atomically
    /// only if `mutate` succeeded.
    /// Test: `index_concurrent_writers_never_lose_a_name`,
    /// `index_corrupt_file_fails_closed_and_is_never_reset`.
    fn update<R>(
        &self,
        vault: &VaultName,
        mutate: impl FnOnce(&mut IndexFile) -> Result<R, SecretsError>,
    ) -> Result<R, SecretsError> {
        platform::create_private_dir(&self.root)?;
        let path = self.path_for(vault);
        platform::with_exclusive_lock(&path, self.lock_timeout, || {
            let mut file = read_file(&path, vault)?;
            let result = mutate(&mut file)?;
            let json = serde_json::to_vec_pretty(&file).map_err(|e| SecretsError::Io {
                path: path.clone(),
                source: std::io::Error::other(e.to_string()),
            })?;
            platform::write_private_atomic(&path, &json)?;
            Ok(result)
        })
    }
}

fn corrupt(path: &Path, reason: impl Into<String>) -> SecretsError {
    SecretsError::IndexCorrupt {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

/// Parse one index file, failing closed on anything unexpected.
///
/// What: a missing file is an empty index. A parse error is reported by line
/// and column only, never by content. A version or vault mismatch, or a row
/// whose name is not a valid key, is corrupt.
/// Test: `index_corrupt_file_fails_closed_and_is_never_reset`.
fn read_file(path: &Path, vault: &VaultName) -> Result<IndexFile, SecretsError> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(IndexFile::empty(vault));
        }
        Err(source) => {
            return Err(SecretsError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let file: IndexFile = serde_json::from_slice(&raw).map_err(|e| {
        corrupt(
            path,
            format!("does not parse at line {} column {}", e.line(), e.column()),
        )
    })?;
    if file.version != INDEX_VERSION {
        return Err(corrupt(
            path,
            format!("unsupported version {}", file.version),
        ));
    }
    if file.vault != vault.as_str() {
        return Err(corrupt(path, "names a different vault"));
    }
    if file.keys.keys().any(|name| SecretKey::new(name).is_err()) {
        return Err(corrupt(path, "holds an invalid key name"));
    }
    Ok(file)
}

fn to_meta(path: &Path, name: &str, row: &IndexRow) -> Result<KeyMeta, SecretsError> {
    let name = SecretKey::new(name).map_err(|_| corrupt(path, "holds an invalid key name"))?;
    Ok(KeyMeta {
        name,
        length: row.length,
        updated_at: row.updated_at,
        agents_may_use: row.agents_may_use,
    })
}
