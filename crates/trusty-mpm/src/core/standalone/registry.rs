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

/// Compiled alias validation regex — compiled once at program start.
///
/// Why: aliases appear in directory names and CLI output; tightly restricting
/// the character set prevents path traversal and display ambiguity. Compiling
/// once avoids repeated `Regex::new` allocations on every `add` call.
/// What: compiled regex matching `^[a-z0-9][a-z0-9._-]*$`.
/// Test: `test_alias_validation` in this module.
static ALIAS_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-z0-9][a-z0-9._-]*$").expect("alias pattern is valid")
});

/// One entry in the standalone-driver registry.
///
/// Why: stores the alias, clone URL, git ref sentinel, and creation timestamp
/// so every lifecycle verb (`load`, `run`, `path`, `ls`, `rm`) can resolve
/// the alias to its repository without further user input.
/// What: a flat struct serialized as a JSON object within the registry array.
/// Test: round-tripped by `test_registry_add_and_list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryEntry {
    /// Short human-readable identifier (e.g. `my-project`).
    pub alias: String,
    /// Clone-able URL (HTTPS or SSH form).
    pub url: String,
    /// Sentinel describing the branch/ref intent.  Currently always
    /// `"default"`, meaning `clone_repo` clones the remote's default branch
    /// without `-b`.  Branch selection is a future enhancement; this field
    /// must not be interpreted as a literal Git ref name in MVP code.
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
    /// their first step so they always operate on up-to-date state. A present
    /// but malformed file returns an error rather than silently discarding all
    /// aliases — that data-loss path is the bug fixed in #1548 review F1.
    /// What: creates `managed_root` if absent; returns empty entries when the
    /// file is missing (normal first run); returns `RegistryError::Json` when
    /// the file is present but not valid JSON so callers can surface a clear
    /// message to the user instead of wiping the registry.
    /// Test: `test_registry_add_and_list`, `test_load_malformed_errors_not_empty`,
    /// `test_load_missing_is_empty`.
    pub fn load(managed_root: &Path) -> Result<Self, RegistryError> {
        let path = managed_root.join("registry.json");
        std::fs::create_dir_all(managed_root).map_err(|source| RegistryError::Io {
            path: managed_root.to_path_buf(),
            source,
        })?;
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => {
                // F1: present-but-malformed → propagate the parse error so the
                // caller can surface "registry.json is malformed; fix or delete
                // <path>" rather than silently overwriting with empty entries.
                serde_json::from_str::<Vec<RegistryEntry>>(&text)?
            }
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

    /// Persist the registry to disk atomically.
    ///
    /// Why: write-back semantics — the registry is mutated in memory and
    /// persisted only on explicit `save()` calls, giving callers control over
    /// the order of mutations. Writing to a temp file then renaming (POSIX
    /// atomic rename) prevents partial-write corruption on crash (F2 fix).
    /// What: serializes `entries` to pretty-printed JSON, writes to a `.tmp`
    /// sibling in the same directory, then `fs::rename`s it over the target so
    /// the update is crash-safe.
    /// Test: `test_atomic_save_round_trip`, plus every test that calls `add` or
    /// `remove` exercises the write path indirectly.
    pub fn save(&self) -> Result<(), RegistryError> {
        let serialized = serde_json::to_string_pretty(&self.entries)?;
        // Write to a sibling temp file in the same directory so that `rename`
        // is on the same filesystem and is therefore atomic on POSIX.
        let tmp_path = self.path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &serialized).map_err(|source| RegistryError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &self.path).map_err(|source| RegistryError::Io {
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
            // "default" is a sentinel meaning "clone the remote's default
            // branch without -b".  Branch selection is not implemented in the
            // MVP; the stored value must not be passed as a literal Git ref.
            git_ref: "default".to_string(),
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
/// traversal, whitespace, and ambiguous display. Using the `LazyLock`-compiled
/// `ALIAS_RE` avoids repeated `Regex::new` allocations on every call (F6 fix).
/// What: returns `InvalidAlias` when the string does not match
/// `^[a-z0-9][a-z0-9._-]*$`.
/// Test: `test_alias_validation`.
fn validate_alias(alias: &str) -> Result<(), RegistryError> {
    if !ALIAS_RE.is_match(alias) {
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
        assert_eq!(
            entries[0].git_ref, "default",
            "git_ref must be the 'default' sentinel, not a literal branch name"
        );
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

    // F1: a present-but-malformed registry.json must return an Err, not empty.
    // A subsequent save() must NOT be called after a load error, so no data
    // loss occurs.
    #[test]
    fn test_load_malformed_errors_not_empty() {
        let (_dir, root) = tmp_root();
        // Write a malformed (truncated) JSON file.
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("registry.json"), b"[{\"alias\":\"broken\"").unwrap();

        let result = ManagedRegistry::load(&root);
        assert!(
            result.is_err(),
            "load of malformed registry.json must return Err, not empty"
        );
        assert!(
            matches!(result.unwrap_err(), RegistryError::Json(_)),
            "error must be RegistryError::Json for a parse failure"
        );

        // The file must remain untouched (no data-loss overwrite happened).
        let still_malformed = std::fs::read_to_string(root.join("registry.json")).unwrap();
        assert!(
            still_malformed.contains("broken"),
            "malformed file must not be overwritten after a failed load"
        );
    }

    // F1: a missing registry.json (first-run) should still succeed with empty.
    #[test]
    fn test_load_missing_is_empty() {
        let (_dir, root) = tmp_root();
        let reg = ManagedRegistry::load(&root).unwrap();
        assert!(reg.list().is_empty(), "fresh load must have zero entries");
    }

    // F2: save + load must round-trip (atomic write correctness).
    #[test]
    fn test_atomic_save_round_trip() {
        let (_dir, root) = tmp_root();
        let mut reg = ManagedRegistry::load(&root).unwrap();
        reg.add("alpha", "https://github.com/org/alpha", false)
            .unwrap();
        reg.add("beta", "https://github.com/org/beta", false)
            .unwrap();
        reg.save().unwrap();

        // The .tmp sibling must not be left behind.
        assert!(
            !root.join("registry.json.tmp").exists(),
            "no .tmp file should remain after a successful save"
        );

        let reg2 = ManagedRegistry::load(&root).unwrap();
        let entries = reg2.list();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].alias, "alpha");
        assert_eq!(entries[1].alias, "beta");
    }

    // F7: duplicate register without --force must error clearly.
    #[test]
    fn test_register_existing_alias_without_force_errors() {
        let (_dir, root) = tmp_root();
        let mut reg = ManagedRegistry::load(&root).unwrap();
        reg.add("myproj", "https://github.com/org/a", false)
            .unwrap();
        reg.save().unwrap();

        // Reload and try to register the same alias with a different URL.
        let mut reg2 = ManagedRegistry::load(&root).unwrap();
        let err = reg2
            .add("myproj", "https://github.com/org/b", false)
            .unwrap_err();
        assert!(
            matches!(err, RegistryError::DuplicateAlias { ref alias, .. } if alias == "myproj"),
            "must return DuplicateAlias error when alias exists and --force is not set"
        );
        // The original URL must be preserved.
        let reg3 = ManagedRegistry::load(&root).unwrap();
        assert_eq!(reg3.get("myproj").unwrap().url, "https://github.com/org/a");
    }

    // F7: duplicate register WITH --force must succeed and overwrite.
    #[test]
    fn test_register_existing_alias_with_force_succeeds() {
        let (_dir, root) = tmp_root();
        let mut reg = ManagedRegistry::load(&root).unwrap();
        reg.add("myproj", "https://github.com/org/a", false)
            .unwrap();
        reg.save().unwrap();

        let mut reg2 = ManagedRegistry::load(&root).unwrap();
        reg2.add("myproj", "https://github.com/org/b", true)
            .unwrap();
        reg2.save().unwrap();

        let reg3 = ManagedRegistry::load(&root).unwrap();
        assert_eq!(
            reg3.list().len(),
            1,
            "still exactly one entry after force-overwrite"
        );
        assert_eq!(
            reg3.get("myproj").unwrap().url,
            "https://github.com/org/b",
            "URL must be updated after --force overwrite"
        );
    }
}
