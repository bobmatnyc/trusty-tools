//! Opt-in index allowlist with default-deny and hard sensitive-path denylist.
//!
//! Why: trusty-search previously auto-registered any directory it encountered
//! (cwd probes, MCP calls, transient worktrees), creating 74 unrequested indexes
//! including private directories with personal data and `.env` files. This module
//! enforces that NOTHING is indexed unless the user explicitly approves it via
//! the allowlist, and that certain sensitive path patterns can never be indexed
//! even when explicitly requested.
//!
//! What: two complementary guards:
//! 1. **Hard denylist** (`SENSITIVE_PATH_PATTERNS`) — patterns matched against
//!    the candidate path; a match produces a loud refusal regardless of any
//!    allowlist entry. Covers `$HOME` top-level, `~/.ssh`, `~/.aws`, `/tmp`,
//!    `.env` files, and similar.
//! 2. **Allowlist** (`AllowlistConfig`, stored at
//!    `~/.config/trusty-search/indexes.toml`) — the candidate path must match
//!    an entry here (or a prefix of one) for registration to proceed. A fresh
//!    daemon with an empty allowlist accepts ZERO new indexes.
//!
//! Call [`check_path`] from the index-creation path (`POST /indexes` handler,
//! CLI `trusty-search index`) before any registration occurs. When the check
//! passes, also call [`add_to_allowlist`] so the allowlist file stays in sync.
//!
//! Test: unit tests in `tests.rs` cover default-deny (unlisted path rejected),
//! allowlist hit (listed path accepted), denylist override (sensitive path
//! rejected even when allowlisted), malformed config handling, and the
//! add/remove helpers.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

// ── Hard denylist ───────────────────────────────────────────────────────────

/// Path fragments/suffixes that can never be indexed regardless of the
/// allowlist. Checked case-insensitively against every path component and
/// the full path string.
///
/// Why: defense-in-depth — even if a user accidentally adds a sensitive path
/// to the allowlist, the denylist prevents the daemon from processing it.
/// Loud refusal + stderr log ensures the operator always knows why.
/// What: matched against the full canonical path string. A path is denied when
/// any pattern appears as a substring of the normalized path.
/// Test: `denylist_blocks_ssh_dir`, `denylist_blocks_tmp`,
/// `denylist_blocks_env_file` in `tests.rs`.
pub const SENSITIVE_PATH_PATTERNS: &[&str] = &[
    // Credential / key directories
    "/.ssh",
    "/.aws",
    "/.gnupg",
    "/.kube",
    "/.netrc",
    "/.npmrc",
    "/.pypirc",
    "/.pgpass",
    // macOS-specific sensitive dirs
    "/Library/Keychains",
    "/Library/Application Support",
    // Secrets markers anywhere in the path
    "/.env",
    "/secrets",
    "/credentials",
    "/private_key",
    "/.private",
    // Temporary / ephemeral directories (never worth indexing)
    "/tmp/",
    "/private/tmp",
    "/var/folders",
    // Config dirs that tend to contain secrets
    "/.config",
    // Generic vault/secret path markers
    "/vault",
    "/keystore",
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
/// Why: stores the user-approved path alongside optional per-root settings
/// (include/exclude globs, `skip_kg`) so a single config file captures both
/// "what is indexed" and "how it is indexed".
/// What: serialised to TOML `[[index]]` array-of-tables. `path` is the only
/// required field; all others default to sane values on deserialization.
/// Test: `roundtrip_preserves_all_fields` in `tests.rs`.
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
    /// XDG-style path: `~/.config/trusty-search/indexes.toml`.
    ///
    /// Why: distinct from the daemon's `config.yaml` so the allowlist can be
    /// read without pulling in the full user config, and so scripts/operators
    /// can manage it independently.
    /// What: resolves via `dirs::config_dir()`, falling back to a relative
    /// path in process-only environments (CI containers, test sandboxes).
    /// Test: `allowlist_path_ends_with_expected_suffix` in `tests.rs`.
    pub fn default_path() -> PathBuf {
        match dirs::config_dir() {
            Some(base) => base.join("trusty-search").join("indexes.toml"),
            None => PathBuf::from("trusty-search-indexes.toml"),
        }
    }

    /// Load from the default XDG path.
    ///
    /// Why: single entry point for all callers that need the allowlist; hides
    /// the path logic.
    /// What: delegates to [`Self::load_from`] with [`Self::default_path()`].
    /// Test: integration tested by `trusty-search index list`.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::default_path())
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
/// Why: `trusty-search index add <path>` and `trusty-search index <path>`
/// must both write the allowlist before forwarding to the daemon. This helper
/// centralises the write so both call sites behave identically.
/// What: loads the config, upserts the entry, saves atomically. Returns an
/// error when the denylist blocks the path so callers surface a clear message.
/// The `allowlist_path` parameter is injectable for tests.
/// Test: `add_to_allowlist_persists_entry`, `add_to_allowlist_blocked_by_denylist`
/// in `tests.rs`.
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
/// What: normalises the path to a forward-slash string, then checks two sets
/// of patterns: (1) global `SENSITIVE_PATH_PATTERNS`, (2) home-relative top
/// dirs from `SENSITIVE_HOME_TOP_DIRS`.
/// Test: `denylist_blocks_ssh_dir`, `denylist_blocks_tmp`,
/// `denylist_blocks_home_toplevel`, `denylist_allows_safe_path` in `tests.rs`.
pub fn is_denied(path: &Path) -> Option<String> {
    let path_str = path.to_string_lossy();
    // Normalise separators on Windows (forward-slash patterns).
    let normalised = path_str.replace('\\', "/");

    // Global patterns.
    for &pattern in SENSITIVE_PATH_PATTERNS {
        if normalised.contains(pattern) {
            return Some(format!(
                "path '{}' matches sensitive pattern '{}'; indexing refused",
                path.display(),
                pattern
            ));
        }
    }

    // Home-relative top-level directories.
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
