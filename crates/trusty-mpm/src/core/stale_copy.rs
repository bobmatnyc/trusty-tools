//! The stale-copy guard's config and observed-version store (#10574).
//!
//! Why: the PM rebuilt a revised document from its own copy and lost an
//! unmarked owner edit. Nothing recorded which version of the document the PM
//! had last seen, so nothing could tell a replacement built from a stale copy
//! from one built from the current file.
//! What: [`RawStaleCopy`] is the `[stale_copy]` table of `.trusty-mpm.toml`
//! (owner ruling Dr8 B: registered artifacts are a config list of paths or
//! globs), kept raw so a bad entry disables only this guard.
//! [`stale_copy_for`] resolves it for a directory into a [`StaleCopyState`]:
//! the guard is opt-in, so an absent or empty list is [`StaleCopyState::Inert`]
//! and an unreadable config is [`StaleCopyState::Rejected`], which the caller
//! reports and treats as inactive. [`ObservedStore`] keeps one observed-version
//! record per artifact (path, SHA-256, saved copy) under the project's
//! `.trusty-mpm/stale-copy/`, bounded by [`STORE_BUDGET_BYTES`] and
//! [`MAX_RECORDS`]; [`ObservedStore::freshness`] answers whether a replacing
//! write would start from the version last observed.
//!
//! ```toml
//! [stale_copy]
//! paths = ["docs/**/*.md"]
//! locked = ["docs/approved/*.md"]
//! ```
//!
//! Test: `stale_copy_tests.rs`.

use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::core::project_accounts::project_dir_for;
use crate::core::project_config::ProjectLevelConfig;

/// Directory under the project's `.trusty-mpm/` that holds the records.
pub const STORE_DIR: &str = "stale-copy";

/// Total bytes of saved copies the store keeps before it evicts the oldest.
pub const STORE_BUDGET_BYTES: u64 = 32 * 1024 * 1024;

/// A file larger than this is recorded by hash only; its copy is not saved.
pub const MAX_COPY_BYTES: u64 = 4 * 1024 * 1024;

/// Most records the store keeps before it evicts the oldest.
pub const MAX_RECORDS: usize = 512;

/// Most patterns one list may carry.
pub const MAX_PATTERNS: usize = 128;

/// The raw `[stale_copy]` value as parsed from TOML, validated later.
///
/// Why: the same reason as `RawAccounts` (#11390): a strict parse would fail
/// the whole `.trusty-mpm.toml` on one bad entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawStaleCopy(pub toml::Value);

// A config value holds no float NaN in practice; `ProjectLevelConfig` is `Eq`.
impl Eq for RawStaleCopy {}

/// The validated pattern lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StaleCopyLists {
    /// Registered artifacts: a replacing write needs a fresh observed version.
    pub paths: Vec<String>,
    /// Locked artifacts: every write is refused.
    pub locked: Vec<String>,
}

impl RawStaleCopy {
    /// Validate the table.
    ///
    /// What: `Err(reason)` for a non-table, a key other than `paths`/`locked`,
    /// a non-array or non-string entry, more than [`MAX_PATTERNS`] entries, an
    /// absolute pattern, a `..` segment, or a pattern that does not compile.
    /// Test: `validate_rejects_unknown_keys_and_escaping_patterns`.
    pub fn validate(&self) -> Result<StaleCopyLists, String> {
        let table = self.0.as_table().ok_or("[stale_copy] must be a table")?;
        let mut lists = StaleCopyLists::default();
        for (key, value) in table {
            let slot = match key.as_str() {
                "paths" => &mut lists.paths,
                "locked" => &mut lists.locked,
                other => return Err(format!("[stale_copy] has an unknown key `{other}`")),
            };
            let items = value
                .as_array()
                .ok_or_else(|| format!("[stale_copy] {key} must be an array of strings"))?;
            if items.len() > MAX_PATTERNS {
                return Err(format!(
                    "[stale_copy] {key} lists {} patterns; at most {MAX_PATTERNS} are allowed",
                    items.len()
                ));
            }
            for item in items {
                let pattern = item
                    .as_str()
                    .ok_or_else(|| format!("[stale_copy] {key} must be an array of strings"))?;
                check_pattern(pattern).map_err(|why| format!("[stale_copy] {key}: {why}"))?;
                slot.push(pattern.to_string());
            }
        }
        Ok(lists)
    }
}

/// Refuse a pattern that is empty, absolute, escapes the project or will not compile.
fn check_pattern(pattern: &str) -> Result<(), String> {
    if pattern.trim().is_empty() {
        return Err("a pattern is empty".into());
    }
    if pattern.starts_with('/') || pattern.starts_with('~') {
        return Err(format!("`{pattern}` must be relative to the project root"));
    }
    if pattern.split('/').any(|seg| seg == "..") {
        return Err(format!("`{pattern}` must not contain a `..` segment"));
    }
    compile(&[pattern.to_string()]).map(|_| ())
}

/// Compile `patterns`; `*` does not cross `/`, `**` does; case-insensitive on macOS.
fn compile(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        // #10574: APFS is case-insensitive, so `Docs/X.md` names `docs/x.md`.
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .case_insensitive(cfg!(target_os = "macos"))
            .build()
            .map_err(|e| format!("`{pattern}` is not a valid glob: {e}"))?;
        builder.add(glob);
    }
    builder.build().map_err(|e| e.to_string())
}

/// A registered artifact a tool call names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// The canonical absolute path: the nearest existing ancestor
    /// canonicalised, the rest re-joined. The store is keyed on it.
    pub abs: PathBuf,
    /// The path relative to the project root, `/`-separated.
    pub rel: String,
    /// Whether the path matches the `locked` list.
    pub locked: bool,
}

/// An active guard: the project root and its compiled lists.
#[derive(Debug, Clone)]
pub struct StaleCopyConfig {
    project_dir: PathBuf,
    paths: GlobSet,
    locked: GlobSet,
}

impl StaleCopyConfig {
    /// Compile validated lists for `project_dir`.
    ///
    /// Test: `classify_matches_registered_and_locked_paths`.
    pub fn new(project_dir: &Path, lists: &StaleCopyLists) -> Result<Self, String> {
        Ok(Self {
            project_dir: project_dir.to_path_buf(),
            paths: compile(&lists.paths)?,
            locked: compile(&lists.locked)?,
        })
    }

    /// The project root the lists are relative to.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// The artifact `path` names, resolved against `cwd`, or `None` when it
    /// lies outside the project or matches neither list.
    ///
    /// What: two spellings are matched — the lexical one and the canonical one
    /// ([`resolve`]: symlinks followed, `..` resolved physically) — each
    /// relative to the matching spelling of the project root. Either one
    /// matching `locked` locks the artifact, so a symlink or a case variant
    /// cannot escape it (#10574). `abs` is the canonical path.
    /// Test: `classify_matches_registered_and_locked_paths`,
    /// `classify_ignores_paths_outside_the_project`,
    /// `a_locked_path_is_denied_through_a_symlink`,
    /// `a_stale_write_is_denied_through_a_dotdot_after_a_symlink`.
    pub fn classify(&self, path: &str, cwd: &Path) -> Option<Artifact> {
        let joined = cwd.join(path);
        let lexical = normalize(&joined);
        let abs = resolve(&joined);
        let canon_rel = rel_under(&abs, &resolve(&self.project_dir));
        let lex_rel = rel_under(&lexical, &normalize(&self.project_dir));
        let rels: Vec<&String> = canon_rel.iter().chain(lex_rel.iter()).collect();
        let rel = (*rels.first()?).clone();
        let locked = rels.iter().any(|r| self.locked.is_match(r.as_str()));
        let registered = rels.iter().any(|r| self.paths.is_match(r.as_str()));
        (locked || registered).then_some(Artifact { abs, rel, locked })
    }
}

/// `path` relative to `root`, `/`-separated; `None` outside it or for the root.
fn rel_under(path: &Path, root: &Path) -> Option<String> {
    let rel = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    (!rel.is_empty()).then_some(rel)
}

/// The canonical form of `path`, which need not exist.
///
/// What: canonicalise the longest existing prefix (symlinks followed, `..`
/// resolved by the filesystem), then re-join the remaining components with
/// `.` and `..` resolved lexically — a component that does not exist cannot
/// be a symlink. With no existing prefix, [`normalize`].
fn resolve(path: &Path) -> PathBuf {
    let comps: Vec<Component<'_>> = path.components().collect();
    for split in (1..=comps.len()).rev() {
        let prefix: PathBuf = comps[..split].iter().collect();
        let Ok(mut out) = prefix.canonicalize() else {
            continue;
        };
        for comp in &comps[split..] {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        return out;
    }
    normalize(path)
}

/// Resolve `.` and `..` lexically; a symlink is not followed.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// What the project says about the stale-copy guard.
#[derive(Debug, Clone)]
pub enum StaleCopyState {
    /// No config, no `[stale_copy]` table, or empty lists: the guard is off.
    Inert,
    /// The config or the table could not be used; the guard is off and the
    /// caller reports the reason.
    Rejected(String),
    /// The guard is on.
    Active(StaleCopyConfig),
}

/// The stale-copy state for the project containing `cwd`.
///
/// Why (Dr8 B): the guard is opt-in by config, so an unreadable config must
/// not register every path; it turns the guard off and says so.
/// What: [`StaleCopyState::Inert`] without a `.trusty-mpm.toml`, without the
/// table or with both lists empty; [`StaleCopyState::Rejected`] when the file
/// cannot be read or parsed or the table fails [`RawStaleCopy::validate`];
/// otherwise [`StaleCopyState::Active`].
/// Test: `state_is_inert_without_a_table_or_with_empty_lists`,
/// `state_is_rejected_for_a_malformed_config`, `state_is_active_from_a_subdirectory`.
pub fn stale_copy_for(cwd: &Path) -> StaleCopyState {
    let Some(dir) = project_dir_for(cwd) else {
        return StaleCopyState::Inert;
    };
    let raw = match ProjectLevelConfig::load(&dir) {
        Ok(Some(cfg)) => cfg.stale_copy,
        Ok(None) => None,
        Err(err) => return StaleCopyState::Rejected(err.to_string()),
    };
    let Some(raw) = raw else {
        return StaleCopyState::Inert;
    };
    let lists = match raw.validate() {
        Ok(lists) => lists,
        Err(reason) => return StaleCopyState::Rejected(reason),
    };
    if lists.paths.is_empty() && lists.locked.is_empty() {
        return StaleCopyState::Inert;
    }
    match StaleCopyConfig::new(&dir, &lists) {
        Ok(cfg) => StaleCopyState::Active(cfg),
        Err(reason) => StaleCopyState::Rejected(reason),
    }
}

#[path = "stale_copy_store.rs"]
mod store;
pub use store::{Freshness, ObservedRecord, ObservedStore, StoreError, sha256_hex};
