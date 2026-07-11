//! `~/.trusty-tools/credentials.toml` `KeyStore` backend (issue #2401).
//!
//! Why: headless/CI environments (and any operator without an OS keychain
//! session — SSH, containers) need a durable credential store that doesn't
//! require desktop-session keychain access. Bob-approved: a `0600`-permission
//! flat file directly under `~/.trusty-tools/` (a sibling of the per-crate
//! `~/.trusty-tools/<crate>/` directories the `crate_config` module manages,
//! not nested under one, since credentials are cross-crate) is an acceptable
//! **permanent** posture for that case, not just a bootstrap fallback.
//! What: [`FileKeyStore`] persists a `[keys]` TOML table
//! (`fireworks = "..."` etc.) atomically (write-to-`.tmp` + rename, mirroring
//! `crate_config::save_at`'s convention) and re-asserts `0600` permissions on
//! every write. The base directory is injectable via [`FileKeyStore::at`] so
//! tests never touch the real `$HOME`; [`FileKeyStore::new`] resolves the
//! real one via `dirs::home_dir()`.
//! Test: `file_store_tests` (sibling file).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{KeyStore, KeyStoreError};

/// Cross-crate credentials directory name, sibling to the per-crate
/// `crate_config` directories, both living directly under `$HOME`.
const CREDENTIALS_DIR: &str = ".trusty-tools";

/// Credential file name within [`CREDENTIALS_DIR`].
const CREDENTIALS_FILE: &str = "credentials.toml";

/// On-disk shape: a single `[keys]` table, `provider = "value"` entries.
///
/// Why: a nested table (vs. a flat top-level table) leaves room for future
/// non-key metadata (e.g. a schema-version stamp) without a breaking format
/// change; documented here per the ticket's "pick simple, document" note.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CredentialsFile {
    #[serde(default)]
    keys: BTreeMap<String, String>,
}

/// File-backed credential store. See module docs.
pub struct FileKeyStore {
    path: PathBuf,
    // Serialises read-modify-write cycles within this process; the atomic
    // tmp+rename write additionally protects readers in other processes
    // from ever observing a torn file.
    lock: Mutex<()>,
}

impl FileKeyStore {
    /// Production constructor: `~/.trusty-tools/credentials.toml`.
    ///
    /// Why/What: fails with [`KeyStoreError::HomeUnavailable`] only in a
    /// stripped environment where `dirs::home_dir()` returns `None`; callers
    /// (the resolver's `default_store`) fall back to `MemoryKeyStore` in
    /// that case rather than panicking.
    pub fn new() -> Result<Self, KeyStoreError> {
        let home = dirs::home_dir().ok_or(KeyStoreError::HomeUnavailable)?;
        Ok(Self::at(&home))
    }

    /// Hermetic constructor: `<base>/.trusty-tools/credentials.toml`.
    ///
    /// Why: tests must never write to the real `$HOME`; pointing `base` at a
    /// tempdir gives byte-identical on-disk layout without touching it.
    pub fn at(base: &Path) -> Self {
        let path = base.join(CREDENTIALS_DIR).join(CREDENTIALS_FILE);
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<CredentialsFile, KeyStoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => toml::from_str(&raw).map_err(|e| KeyStoreError::Toml {
                path: self.path.clone(),
                message: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CredentialsFile::default()),
            Err(e) => Err(KeyStoreError::Io {
                path: self.path.clone(),
                source: e,
            }),
        }
    }

    fn write(&self, data: &CredentialsFile) -> Result<(), KeyStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| KeyStoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let toml_str = toml::to_string_pretty(data).map_err(|e| KeyStoreError::Toml {
            path: self.path.clone(),
            message: e.to_string(),
        })?;
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, &toml_str).map_err(|e| KeyStoreError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        set_permissions_0600(&tmp)?;
        std::fs::rename(&tmp, &self.path).map_err(|e| KeyStoreError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        // Re-assert 0600 on the final path too: some platforms/filesystems
        // don't preserve permissions across rename, and a future writer
        // could have loosened them out-of-band.
        set_permissions_0600(&self.path)?;
        Ok(())
    }
}

/// Set `path`'s permissions to owner-read-write-only (`0600`).
///
/// Why: `credentials.toml` holds plaintext API keys; group/world-readable
/// permissions would leak them to any other local user.
/// What: unix-only via `std::os::unix::fs::PermissionsExt`; on non-unix
/// targets this is a documented best-effort no-op (see module docs — no
/// portable POSIX-mode equivalent exists on Windows).
/// Test: `file_store_tests::file_is_created_with_0600_perms` (unix only).
#[cfg(unix)]
fn set_permissions_0600(path: &Path) -> Result<(), KeyStoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
        KeyStoreError::Io {
            path: path.to_path_buf(),
            source: e,
        }
    })
}

/// Non-unix best-effort no-op — see [`set_permissions_0600`] docs.
#[cfg(not(unix))]
fn set_permissions_0600(_path: &Path) -> Result<(), KeyStoreError> {
    Ok(())
}

impl KeyStore for FileKeyStore {
    fn get(&self, provider: &str) -> Option<String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read().ok()?.keys.get(provider).cloned()
    }

    fn set(&self, provider: &str, value: &str) -> Result<(), KeyStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut data = self.read()?;
        data.keys.insert(provider.to_string(), value.to_string());
        self.write(&data)
    }

    fn unset(&self, provider: &str) -> Result<(), KeyStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut data = self.read()?;
        data.keys.remove(provider);
        self.write(&data)
    }

    fn list(&self) -> Vec<String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read()
            .map(|d| d.keys.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the store must round-trip a value through set/get exactly.
    /// Test: itself.
    #[test]
    fn set_then_get_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        assert_eq!(store.get("fireworks"), None);
        store.set("fireworks", "fw-secret").unwrap();
        assert_eq!(store.get("fireworks"), Some("fw-secret".to_string()));
    }

    /// Why: `unset` must remove the entry and be a no-op when absent.
    /// Test: itself.
    #[test]
    fn unset_removes_and_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        store.set("openai", "sk-abc").unwrap();
        store.unset("openai").unwrap();
        assert_eq!(store.get("openai"), None);
        store.unset("openai").unwrap();
    }

    /// Why: `list` must return names only, and multiple entries must all
    /// survive a write/read cycle.
    /// Test: itself.
    #[test]
    fn list_returns_all_provider_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        store.set("anthropic", "sk-ant-secret").unwrap();
        store.set("openrouter", "or-secret").unwrap();
        let mut names = store.list();
        names.sort();
        assert_eq!(
            names,
            vec!["anthropic".to_string(), "openrouter".to_string()]
        );
    }

    /// Why: the acceptance criterion requires `credentials.toml` to exist
    /// with exactly `0600` permissions after a write.
    /// Test: itself (unix only — see `set_permissions_0600` docs for the
    /// non-unix no-op).
    #[cfg(unix)]
    #[test]
    fn file_is_created_with_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        store.set("fireworks", "fw-secret").unwrap();
        let path = tmp.path().join(".trusty-tools").join("credentials.toml");
        assert!(path.is_file());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    /// Why: perms must be re-asserted on every write, not just the first.
    /// Test: itself.
    #[cfg(unix)]
    #[test]
    fn perms_are_reasserted_on_every_write() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        store.set("fireworks", "fw-secret").unwrap();
        let path = tmp.path().join(".trusty-tools").join("credentials.toml");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.set("openai", "sk-abc").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected re-asserted 0600, got {mode:o}");
    }

    /// Why: an absent file must read as an empty store, not an error.
    /// Test: itself.
    #[test]
    fn absent_file_reads_as_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        assert_eq!(store.list(), Vec::<String>::new());
        assert_eq!(store.get("anything"), None);
    }
}
