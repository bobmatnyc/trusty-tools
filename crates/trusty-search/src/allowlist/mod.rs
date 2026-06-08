//! Opt-in index allowlist with default-deny and hard sensitive-path denylist.
//!
//! Why: trusty-search previously auto-registered any directory it encountered
//! (cwd probes, MCP calls, transient worktrees), creating 74 unrequested indexes
//! including private directories with personal data and `.env` files.
//!
//! What: two complementary guards:
//! 1. **Hard denylist** — patterns matched at path-component boundaries; a
//!    match produces a loud refusal regardless of any allowlist entry.
//! 2. **Allowlist** (`AllowlistConfig`, stored at
//!    `~/.config/trusty-search/allowlist.toml`) — default-deny; a fresh daemon
//!    accepts ZERO new indexes. File is `allowlist.toml` (not `indexes.toml`) to
//!    avoid the macOS collision where `config_dir()==data_local_dir()`.
//!    On first load, [`migration`] attempts a one-time copy from the old
//!    `indexes.toml` path; if that file is the daemon registry it will fail to
//!    parse and the migration is silently skipped.
//!
//! Call [`check_path`] from every index-creation path before registration.
//! Then call [`add_to_allowlist`] to keep the file in sync.
//!
//! Test: `tests.rs` (unit); `collision_tests.rs` (collision + migration).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod migration;
pub use migration::{legacy_allowlist_path, try_migrate_legacy};

#[cfg(test)]
mod collision_tests;
#[cfg(test)]
mod tests;

// ── Hard denylist ───────────────────────────────────────────────────────────

/// Path component names that are never indexable. Each entry is compared
/// against every individual component of the candidate path (not as a
/// substring of the full path string).
///
/// Why: raw substring matching wrongly denies legitimate paths such as
/// `/home/user/Projects/secrets-manager` or `/srv/app/credentials-validator`
/// because "secrets" and "credentials" appear as substrings. Anchoring at
/// component boundaries ensures only paths that contain `secrets`, `.ssh`,
/// etc. as a complete path segment are denied.
/// What: `is_denied` splits the canonicalised path via `Path::components()`
/// and checks each `Normal` component (directory name or filename) against
/// this list using exact equality. Dotfile components like `.ssh` are also
/// matched by exact equality.
/// Test: `denylist_blocks_ssh_dir`, `denylist_blocks_env_file`,
/// `denylist_allows_path_with_sensitive_word_in_name` in `tests.rs`.
pub const SENSITIVE_COMPONENT_NAMES: &[&str] = &[
    // Credential / key directories (exact directory names)
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".pgpass",
    ".private",
    // Env-file directory (Python virtualenvs often name their dir ".venv",
    // not ".env", so ".env" directories are typically secrets-bearing).
    ".env",
    // Secrets-bearing directory names (whole-component match only)
    "secrets",
    "credentials",
    "private_key",
    // Config dirs that tend to contain secrets
    ".config",
    // Generic vault/secret directory names
    "vault",
    "keystore",
    // macOS sensitive subdirectory names
    "Keychains",
];

/// Filename suffixes / exact file names that are never indexable, matched
/// against the final path component only.
///
/// Why: `.env` files store environment secrets but a *directory* named
/// `.env` is legitimate (e.g. Python virtualenvs). Matching against only
/// the last component lets us catch `project/.env` (a file) without
/// denying `project/.env/` if the path happens to refer to a directory
/// named `.env` — though in practice both are sensitive.
/// What: `is_denied` calls `path.file_name()` and checks the result
/// against this list using exact equality.
/// Test: `denylist_blocks_env_file_in_path` in `tests.rs`.
pub const SENSITIVE_FILE_NAMES: &[&str] = &[".env"];

/// Path prefixes matched against the full forward-slash-normalised path.
/// Used only for well-known ephemeral or system directories whose *entire
/// subtree* is unsafe — not for user-space directory names, which are
/// covered by `SENSITIVE_COMPONENT_NAMES` to avoid false positives.
///
/// Why: `/tmp`, `/private/tmp`, and `/var/folders` are OS-managed
/// temporaries that should never be indexed. Their paths are fixed and
/// do not appear as user project names, so prefix matching is safe here.
/// What: `is_denied` checks `normalised.starts_with(prefix)` for each entry.
/// Test: `denylist_blocks_tmp` in `tests.rs`.
pub const SENSITIVE_PATH_PREFIXES: &[&str] = &[
    "/tmp/",
    "/private/tmp",
    // macOS canonicalises /var → /private/var, so tempdirs at
    // /var/folders/… resolve to /private/var/folders/… after
    // std::fs::canonicalize.  Both prefixes are required.
    "/var/folders",
    "/private/var/folders",
    "/Library/Application Support",
];

/// Additional top-level home subdirectories that are never indexable.
/// Matched against the path when it starts with the user's home directory.
///
/// Why: indexing your entire `~/Downloads`, `~/Desktop`, or `~` itself would
/// catch an enormous amount of private data. These prefixes block the most
/// dangerous cases.
/// What: when the candidate is `$HOME/<segment>` and `segment` matches one of
/// these, the path is denied.
/// Test: `denylist_blocks_home_toplevel` in `tests.rs`.
pub const SENSITIVE_HOME_TOP_DIRS: &[&str] = &[
    "", // $HOME itself
    "Desktop",
    "Downloads",
    "Documents",
    "Pictures",
    "Movies",
    "Music",
    "Library",
];

// ── Allowlist config ─────────────────────────────────────────────────────────

/// One allowlisted root entry.
///
/// Why: stores the user-approved path alongside optional per-root settings.
/// What: TOML `[[index]]` array entry. Only `path` is accepted; the former
/// `root_path` alias was removed so daemon-registry entries cannot parse as
/// allowlist approvals (they would bypass the opt-in security gate).
/// Test: `roundtrip_preserves_all_fields`; `migration_daemon_registry_is_not_migrated`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AllowlistEntry {
    /// Absolute path to the approved project root.
    pub path: PathBuf,

    /// Optional override for the index name. When absent the CLI/daemon
    /// derive the name from the directory basename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Additional glob patterns to exclude during indexing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,

    /// Explicit file extensions to include (without leading `.`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,

    /// Skip knowledge-graph construction for this index.
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_kg: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

/// Top-level allowlist document.
///
/// Why: a single file at a stable XDG location is the user-visible "what is
/// indexed" source of truth. Every index-creation path writes here; every
/// index-removal path deletes from here. Operators can also edit the file by
/// hand and restart the daemon.
/// What: TOML `[[index]]` array under key `index`. An absent or empty file
/// means the allowlist is empty and nothing may be indexed (default-deny).
/// Test: `load_returns_empty_when_missing`, `roundtrip_preserves_all_fields`
/// in `tests.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AllowlistConfig {
    /// All explicitly approved roots.
    #[serde(default, rename = "index")]
    pub entries: Vec<AllowlistEntry>,
}

impl AllowlistConfig {
    /// XDG-style path: `~/.config/trusty-search/allowlist.toml`.
    ///
    /// Why: `allowlist.toml` (not `indexes.toml`) prevents the macOS collision
    /// where `config_dir()==data_local_dir()`; daemon registry stays `indexes.toml`.
    /// What: resolves via `dirs::config_dir()`; falls back to a relative path.
    /// Test: `allowlist_path_ends_with_expected_suffix`,
    /// `allowlist_path_does_not_collide_with_daemon_registry`.
    pub fn default_path() -> PathBuf {
        match dirs::config_dir() {
            Some(base) => base.join("trusty-search").join("allowlist.toml"),
            None => PathBuf::from("trusty-search-allowlist.toml"),
        }
    }

    /// Load from the default XDG path, running the one-time legacy migration
    /// when needed.
    ///
    /// Why: single entry point for all callers that need the allowlist; hides
    /// the path logic and the migration handshake.
    /// What: attempts a one-time migration from the pre-rename `indexes.toml`
    /// path (safe no-op when `allowlist.toml` already exists or the legacy file
    /// is the daemon registry), then delegates to [`Self::load_from`].
    /// Test: `migration_real_allowlist_is_migrated` in `collision_tests.rs`;
    /// integration-tested by `trusty-search index list`.
    pub fn load() -> Result<Self> {
        let new_path = Self::default_path();
        migration::try_migrate_legacy(&new_path, &migration::legacy_allowlist_path());
        Self::load_from(&new_path)
    }

    /// Load from an explicit path (used by tests to avoid touching the real config).
    ///
    /// Why: the path must be injectable for unit tests running in parallel.
    /// What: returns `AllowlistConfig::default()` when the file does not exist
    /// (first-run, no entries yet). Propagates I/O and TOML parse errors so a
    /// corrupt file surfaces loudly rather than silently granting an empty list.
    /// Test: `load_returns_empty_when_missing`, `load_errors_on_malformed` in
    /// `tests.rs`.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("could not read allowlist {}", path.display()))?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        toml::from_str::<Self>(&raw)
            .with_context(|| format!("could not parse allowlist TOML at {}", path.display()))
    }

    /// Save to the default path (atomic write: temp-file + rename).
    ///
    /// Why: atomic writes ensure a crash mid-write never leaves a corrupt file.
    /// What: creates parent directories if needed, writes TOML to a `.tmp`
    /// sibling, then renames atomically.
    /// Test: `roundtrip_preserves_all_fields`, `save_creates_parent_dirs` in
    /// `tests.rs`.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::default_path())
    }

    /// Save to an explicit path (injectable for tests).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let toml_str =
            toml::to_string_pretty(self).context("could not serialise allowlist as TOML")?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, &toml_str)
            .with_context(|| format!("could not write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("could not rename {} to {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Add or update an entry in the allowlist (matched by canonical path).
    ///
    /// Why: `trusty-search index add <path>` must be idempotent — re-adding
    /// the same path must update any changed settings rather than duplicate
    /// the entry.
    /// What: linear scan over `entries` by canonical path; replaces in place
    /// or pushes a new entry.
    /// Test: `upsert_replaces_existing_by_path`, `upsert_appends_new` in
    /// `tests.rs`.
    pub fn upsert(&mut self, entry: AllowlistEntry) {
        let target = canonicalise(&entry.path);
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|e| canonicalise(&e.path) == target)
        {
            *slot = entry;
        } else {
            self.entries.push(entry);
        }
    }

    /// Remove the entry matching `path` (after canonicalisation).
    ///
    /// Why: `trusty-search index remove <path>` must also remove the allowlist
    /// entry so the path can no longer be re-registered without re-adding.
    /// What: retains all entries whose canonical path differs from `path`;
    /// returns the removed entry when found.
    /// Test: `remove_by_path`, `remove_returns_none_for_unknown` in `tests.rs`.
    pub fn remove(&mut self, path: &Path) -> Option<AllowlistEntry> {
        let target = canonicalise(path);
        let pos = self
            .entries
            .iter()
            .position(|e| canonicalise(&e.path) == target)?;
        Some(self.entries.remove(pos))
    }

    /// Check whether `path` has an entry in the allowlist.
    ///
    /// Why: the index-creation path calls this after the denylist check to
    /// enforce default-deny: only paths explicitly present in the allowlist
    /// may be registered.
    /// What: returns `true` when the canonical form of `path` exactly matches
    /// the canonical form of any entry's path.
    /// Test: `allowlist_contains_known_path`, `allowlist_misses_unknown_path`
    /// in `tests.rs`.
    pub fn contains(&self, path: &Path) -> bool {
        let target = canonicalise(path);
        self.entries.iter().any(|e| canonicalise(&e.path) == target)
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Result of the allowlist check, indicating why a path was rejected or
/// which allowlist path matched.
///
/// Why: typed result lets callers produce different error messages for
/// denylist vs. "not in allowlist" rejections, keeping the check logic in
/// one place.
/// What: three variants — `Allowed` (path is safe and in the allowlist),
/// `Denied` (hard denylist hit), `NotAllowlisted` (safe but not in the
/// allowlist).
/// Test: `check_path_*` tests in `tests.rs` assert each variant.
#[derive(Debug, PartialEq, Eq)]
pub enum AllowlistCheck {
    /// Path passes the denylist and is present in the allowlist.
    Allowed,
    /// Path matches a hard-denylist pattern and must never be indexed.
    Denied { reason: String },
    /// Path is not in the sensitive denylist but is not in the allowlist.
    NotAllowlisted,
}

/// Check `path` against both the hard denylist and the allowlist.
///
/// Why: single call site for all index-creation paths (HTTP handler, CLI, MCP)
/// so the policy is enforced uniformly and cannot be bypassed by using a
/// different entry point.
/// What: first applies `is_denied` (hard denylist); if clear, loads the
/// allowlist config and calls `AllowlistConfig::contains`. Returns
/// `AllowlistCheck::Denied`, `AllowlistCheck::NotAllowlisted`, or
/// `AllowlistCheck::Allowed` accordingly.
///
/// The `allowlist_path` parameter is injectable for tests; pass `None` to use
/// the default XDG path.
///
/// Test: `check_path_denied_by_denylist`, `check_path_not_allowlisted`,
/// `check_path_allowed` in `tests.rs`.
pub fn check_path(path: &Path, allowlist_path: Option<&Path>) -> Result<AllowlistCheck> {
    // Hard denylist runs first — no file I/O needed.
    if let Some(reason) = is_denied(path) {
        return Ok(AllowlistCheck::Denied { reason });
    }

    // Load the allowlist config (missing file = empty allowlist = default-deny).
    let cfg = match allowlist_path {
        Some(p) => AllowlistConfig::load_from(p)?,
        None => AllowlistConfig::load()?,
    };

    if cfg.contains(path) {
        Ok(AllowlistCheck::Allowed)
    } else {
        Ok(AllowlistCheck::NotAllowlisted)
    }
}

/// Add `path` to the allowlist file atomically, after validating it against
/// the hard denylist.
///
/// Why: `trusty-search index add/index` must write the allowlist before
/// forwarding to the daemon; this helper centralises the write.
/// What: loads, upserts, saves atomically. Errors when denylist blocks path.
/// `allowlist_path` is injectable for tests.
/// Test: `add_to_allowlist_persists_entry`, `add_to_allowlist_blocked_by_denylist`.
pub fn add_to_allowlist(entry: AllowlistEntry, allowlist_path: Option<&Path>) -> Result<()> {
    // Denylist check before touching the file.
    if let Some(reason) = is_denied(&entry.path) {
        anyhow::bail!("cannot add to allowlist: {reason}");
    }
    let path = match allowlist_path {
        Some(p) => p.to_path_buf(),
        None => AllowlistConfig::default_path(),
    };
    let mut cfg = AllowlistConfig::load_from(&path)?;
    cfg.upsert(entry);
    cfg.save_to(&path)
}

/// Remove `path` from the allowlist file.
///
/// Why: `trusty-search index remove <path>` must strip both the daemon
/// registry entry and the allowlist entry so the path cannot be silently
/// re-added on the next daemon restart.
/// What: loads, removes, saves. No-op when the path is absent. The
/// `allowlist_path` parameter is injectable for tests.
/// Test: `remove_from_allowlist_removes_entry`,
/// `remove_from_allowlist_noop_when_absent` in `tests.rs`.
pub fn remove_from_allowlist(path: &Path, allowlist_path: Option<&Path>) -> Result<()> {
    let cfg_path = match allowlist_path {
        Some(p) => p.to_path_buf(),
        None => AllowlistConfig::default_path(),
    };
    let mut cfg = AllowlistConfig::load_from(&cfg_path)?;
    cfg.remove(path);
    cfg.save_to(&cfg_path)
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Return `Some(reason)` when `path` matches a hard-denylist pattern, or
/// `None` when the path is safe.
///
/// Why: extracted from `check_path` so tests can assert against the raw
/// denylist logic without constructing a full config file.
/// What: applies four anchored checks in order — (1) fixed path-prefix
/// patterns from `SENSITIVE_PATH_PREFIXES`; (2) whole-component matching
/// against `SENSITIVE_COMPONENT_NAMES` using `Path::components()` so that
/// `/Projects/secrets-manager` is NOT denied but `/etc/secrets` IS denied;
/// (3) exact file-name match against `SENSITIVE_FILE_NAMES`; (4) home-relative
/// top-level dirs from `SENSITIVE_HOME_TOP_DIRS`.
/// Test: `denylist_blocks_ssh_dir`, `denylist_blocks_tmp`,
/// `denylist_blocks_home_toplevel`, `denylist_allows_safe_path`,
/// `denylist_allows_path_with_sensitive_word_in_name` in `tests.rs`.
pub fn is_denied(path: &Path) -> Option<String> {
    let path_str = path.to_string_lossy();
    // Normalise separators so prefix checks work on Windows too.
    let normalised = path_str.replace('\\', "/");

    // 1. Fixed path-prefix patterns (OS-managed temporaries, macOS system dirs).
    for &prefix in SENSITIVE_PATH_PREFIXES {
        if normalised.starts_with(prefix) {
            return Some(format!(
                "path '{}' is under sensitive prefix '{}'; indexing refused",
                path.display(),
                prefix
            ));
        }
    }

    // 2. Whole-component matching: deny when any path component is an exact
    //    sensitive name.  This prevents `/Projects/secrets-manager` from
    //    being denied while still blocking `/etc/secrets` or `~/.ssh`.
    for component in path.components() {
        // Only check Normal components (skip Root, Prefix, CurDir, ParentDir).
        if let std::path::Component::Normal(os_name) = component {
            let name = os_name.to_string_lossy();
            if SENSITIVE_COMPONENT_NAMES.contains(&&*name) {
                return Some(format!(
                    "path '{}' contains sensitive component '{}'; indexing refused",
                    path.display(),
                    name
                ));
            }
        }
    }

    // 3. Exact file-name match (e.g. ".env" as the final path component).
    if let Some(fname) = path.file_name() {
        let name = fname.to_string_lossy();
        if SENSITIVE_FILE_NAMES.contains(&&*name) {
            return Some(format!(
                "path '{}' has sensitive file name '{}'; indexing refused",
                path.display(),
                name
            ));
        }
    }

    // 4. Home-relative top-level directories.
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy().replace('\\', "/");
        // Strip the home prefix to get the relative part.
        if let Some(rel) = normalised.strip_prefix(&*home_str) {
            // rel is "" (for home itself) or "/" + something
            let segment = rel.trim_start_matches('/');
            // Top-level if there is zero or one path component remaining.
            let first_component = segment.split('/').next().unwrap_or("");
            if SENSITIVE_HOME_TOP_DIRS.contains(&first_component) {
                return Some(format!(
                    "path '{}' is a sensitive home directory; indexing refused",
                    path.display()
                ));
            }
        }
    }

    None
}

/// Best-effort canonicalisation (mirrors `config.rs`).
fn canonicalise(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}
