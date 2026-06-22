//! Alias → {url, ref, created_at} registry persisted to
//! `~/.trusty-mpm/registry.json`.
//!
//! Why: a name-first registry decouples *declaring* a repo from the cost of
//! cloning it, letting a user register their whole fleet cheaply and `load`
//! lazily (DOC-24 SPEC-STANDALONE-MPM-01). The registry lives under
//! `~/.trusty-mpm/`, never under `~/.claude*`, to honour the isolation
//! invariant.
//! What: [`ManagedRegistry`] wraps a `Vec<RegistryEntry>` serialized to JSON.
//! Provides load, save, add, remove, get, list, and is_loaded.
//! Test: `test_registry_add_and_list`, `test_registry_duplicate_rejects_without_force`,
//! `test_registry_duplicate_allows_with_force` in this module.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Alias validation regex — only safe identifier characters are allowed.
///
/// Why: aliases appear in directory names and CLI output; tightly restricting
/// the character set prevents path traversal and display ambiguity.
/// What: matches `^[a-z0-9][a-z0-9._-]*$`.
/// Test: `test_alias_validation` in this module.
const ALIAS_PATTERN: &str = r"^[a-z0-9][a-z0-9._-]*$";

/// One entry in the standalone-driver registry.
///
/// Why: stores the alias, clone URL, git ref, and creation timestamp so every
/// lifecycle verb (`load`, `run`, `path`, `ls`, `rm`) can resolve the alias to
/// its repository without further user input.
/// What: a flat struct serialized as a JSON object within the registry array.
/// Test: round-tripped by `test_registry_add_and_list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryEntry {
    /// Short human-readable identifier (e.g. `my-project`).
    pub alias: String,
    /// Clone-able URL (HTTPS or SSH form).
    pub url: String,
    /// Git branch/ref to check out (default `"main"`).
    pub git_ref: String,
    /// RFC-3339 creation timestamp.
    pub created_at: String,
}

/// In-memory + on-disk registry of managed aliases.
///
/// Why: all standalone lifecycle verbs share this registry as their single
/// source of truth; centralizing persistence here keeps every verb's happy
/// path to one `load()` + one mutating call + one `save()`.
/// What: a thin wrapper around `Vec<RegistryEntry>` with a `path` pointing to
/// `~/.trusty-mpm/registry.json`. Mutations are written by explicit `save()`
/// calls so callers control the write order.
/// Test: `test_registry_add_and_list`, `test_registry_duplicate_rejects_without_force`.
#[derive(Debug, Default)]
pub struct ManagedRegistry {
    entries: Vec<RegistryEntry>,
    path: PathBuf,
}

/// Errors raised by registry operations.
///
/// Why: callers need typed error discrimination — duplicate-alias conflicts are
/// user-facing hints; IO failures are surfaced separately so the CLI can print
/// the right diagnostic.
/// What: `DuplicateAlias`, `InvalidAlias`, `NotFound`, `Io` variants.
/// Test: each variant is exercised by the corresponding test cases.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The alias is already registered with a different URL and `--force` was
    /// not passed.
    #[error("alias '{alias}' is already registered as '{existing_url}'; use --force to overwrite")]
    DuplicateAlias {
        /// The alias that collides.
        alias: String,
        /// The URL already stored for that alias.
        existing_url: String,
    },
    /// The alias string does not match the allowed pattern.
    #[error("invalid alias '{alias}': must match ^[a-z0-9][a-z0-9._-]*$")]
    InvalidAlias {
        /// The rejected alias string.
        alias: String,
    },
    /// The alias was not found in the registry.
    #[error("alias '{alias}' is not registered; run `tm register {alias} <url>` first")]
    NotFound {
        /// The alias that was looked up.
        alias: String,
    },
    /// A filesystem or JSON serialization error.
    #[error("registry io error for {path}: {source}")]
    Io {
        /// The path the failing operation targeted.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// JSON parse/serialize error.
    #[error("registry json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl ManagedRegistry {
    /// Load or create the registry at `<managed_root>/registry.json`.
    ///
    /// Why: all lifecycle verbs (register, load, run, ls, rm) call this as
    /// their first step so they always operate on up-to-date state.
    /// What: creates `managed_root` if absent, reads `registry.json` (starting
    /// from an empty list when the file is absent or malformed), and returns
    /// the populated [`ManagedRegistry`].
    /// Test: `test_registry_add_and_list`.
    pub fn load(managed_root: &Path) -> Result<Self, RegistryError> {
        let path = managed_root.join("registry.json");
        std::fs::create_dir_all(managed_root).map_err(|source| RegistryError::Io {
            path: managed_root.to_path_buf(),
            source,
        })?;
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Vec<RegistryEntry>>(&text).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(RegistryError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        Ok(Self { entries, path })
    }

    /// Persist the registry to disk.
    ///
    /// Why: write-back semantics — the registry is mutated in memory and
    /// persisted only on explicit `save()` calls, giving callers control over
    /// the order of mutations.
    /// What: serializes `entries` to pretty-printed JSON and writes to `self.path`.
    /// Test: exercised by every test that calls `add` or `remove`.
    pub fn save(&self) -> Result<(), RegistryError> {
        let serialized = serde_json::to_string_pretty(&self.entries)?;
        std::fs::write(&self.path, serialized).map_err(|source| RegistryError::Io {
            path: self.path.clone(),
            source,
        })
    }

    /// Register an alias with the given URL.
    ///
    /// Why: adding an alias without cloning lets users declare their fleet
    /// cheaply and `load` lazily.
    /// What: validates the alias, rejects duplicates (unless `force`), and
    /// appends (or replaces) the entry in memory. Caller must call `save()`.
    /// Test: `test_registry_add_and_list`, `test_registry_duplicate_rejects_without_force`,
    /// `test_registry_duplicate_allows_with_force`.
    pub fn add(&mut self, alias: &str, url: &str, force: bool) -> Result<(), RegistryError> {
        validate_alias(alias)?;
        if let Some(existing) = self.entries.iter().find(|e| e.alias == alias) {
            if existing.url == url {
                return Ok(());
            }
            if !force {
                return Err(RegistryError::DuplicateAlias {
                    alias: alias.to_string(),
                    existing_url: existing.url.clone(),
                });
            }
            self.entries.retain(|e| e.alias != alias);
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.entries.push(RegistryEntry {
            alias: alias.to_string(),
            url: url.to_string(),
            git_ref: "main".to_string(),
            created_at: now,
        });
        Ok(())
    }

    /// Remove an alias from the registry.
    ///
    /// Why: `tm rm <alias>` should clean up the registry entry so `tm ls` no
    /// longer lists a deregistered project.
    /// What: removes the entry by alias name; returns `NotFound` if absent.
    /// Caller must call `save()`.
    /// Test: removing a registered alias succeeds; removing an absent one fails.
    pub fn remove(&mut self, alias: &str) -> Result<(), RegistryError> {
        let before = self.entries.len();
        self.entries.retain(|e| e.alias != alias);
        if self.entries.len() == before {
            return Err(RegistryError::NotFound {
                alias: alias.to_string(),
            });
        }
        Ok(())
    }

    /// Look up a single alias.
    ///
    /// Why: `load`, `run`, and `path` all need to resolve an alias to its URL
    /// before acting.
    /// What: returns a reference to the matching entry, or `NotFound`.
    /// Test: exercised by load/run tests.
    pub fn get(&self, alias: &str) -> Result<&RegistryEntry, RegistryError> {
        self.entries
            .iter()
            .find(|e| e.alias == alias)
            .ok_or_else(|| RegistryError::NotFound {
                alias: alias.to_string(),
            })
    }

    /// Return all entries sorted by alias.
    ///
    /// Why: `tm ls` output should be stable and predictable regardless of
    /// insertion order.
    /// What: clones all entries and sorts by `alias` lexicographically.
    /// Test: `test_registry_add_and_list`.
    pub fn list(&self) -> Vec<RegistryEntry> {
        let mut v = self.entries.clone();
        v.sort_by(|a, b| a.alias.cmp(&b.alias));
        v
    }

    /// Check whether the alias has been loaded (the managed marker exists).
    ///
    /// Why: `tm ls` reports a loaded/unloaded status so users know which
    /// aliases are ready to `run` without a clone step.
    /// What: returns `true` if
    /// `<managed_root>/projects/<alias>/repo/.trusty-mpm/managed.toml` exists.
    /// Test: exercised by the ls integration path.
    pub fn is_loaded(&self, alias: &str, managed_root: &Path) -> bool {
        managed_root
            .join("projects")
            .join(alias)
            .join("repo")
            .join(".trusty-mpm")
            .join("managed.toml")
            .exists()
    }
}

/// Validate an alias string against the allowed character set.
///
/// Why: aliases appear in directory names; enforcing the regex prevents path
/// traversal, whitespace, and ambiguous display.
/// What: returns `InvalidAlias` when the string does not match
/// `^[a-z0-9][a-z0-9._-]*$`.
/// Test: `test_alias_validation`.
fn validate_alias(alias: &str) -> Result<(), RegistryError> {
    let re = regex::Regex::new(ALIAS_PATTERN).expect("alias pattern is valid");
    if !re.is_match(alias) {
        return Err(RegistryError::InvalidAlias {
            alias: alias.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_root() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    #[test]
    fn test_registry_add_and_list() {
        let (_dir, root) = tmp_root();
        let mut reg = ManagedRegistry::load(&root).unwrap();
        reg.add("my-project", "https://github.com/org/repo", false)
            .unwrap();
        reg.save().unwrap();

        let reg2 = ManagedRegistry::load(&root).unwrap();
        let entries = reg2.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alias, "my-project");
        assert_eq!(entries[0].url, "https://github.com/org/repo");
    }

    #[test]
    fn test_registry_duplicate_rejects_without_force() {
        let (_dir, root) = tmp_root();
        let mut reg = ManagedRegistry::load(&root).unwrap();
        reg.add("proj", "https://github.com/org/a", false).unwrap();
        let err = reg
            .add("proj", "https://github.com/org/b", false)
            .unwrap_err();
        assert!(matches!(err, RegistryError::DuplicateAlias { .. }));
    }

    #[test]
    fn test_registry_duplicate_allows_with_force() {
        let (_dir, root) = tmp_root();
        let mut reg = ManagedRegistry::load(&root).unwrap();
        reg.add("proj", "https://github.com/org/a", false).unwrap();
        reg.add("proj", "https://github.com/org/b", true).unwrap();
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.list()[0].url, "https://github.com/org/b");
    }

    #[test]
    fn test_alias_validation_rejects_invalid() {
        assert!(validate_alias("My-Project").is_err());
        assert!(validate_alias("").is_err());
        assert!(validate_alias("-bad").is_err());
        assert!(validate_alias("ok").is_ok());
        assert!(validate_alias("ok-1.2").is_ok());
    }

    #[test]
    fn test_registry_same_url_is_noop() {
        let (_dir, root) = tmp_root();
        let mut reg = ManagedRegistry::load(&root).unwrap();
        reg.add("proj", "https://github.com/org/a", false).unwrap();
        reg.add("proj", "https://github.com/org/a", false).unwrap();
        assert_eq!(reg.list().len(), 1);
    }
}
