//! Configuration types for the diff sampler.
//!
//! Why: callers need to tune sampling parameters (number of diffs, repo paths)
//! without changing function signatures; this module isolates the config type
//! so it can be imported independently.
//! What: defines `DiffSamplerConfig` and its resolution helpers; also defines
//! `DEFAULT_MAX_DIFFS` and `MAX_DIFF_CHARS` constants.
//! Test: `config_repo_path_resolution` in the parent `tests` module.

use std::collections::HashMap;
use std::path::PathBuf;

/// Maximum characters of diff text kept per sampled commit.
///
/// Why: LLM context windows are finite; truncating here prevents a single
/// large commit from consuming the entire profile budget.
/// What: 20,000 UTF-8 characters (~5–10K tokens).  The tga `DIFF_BYTE_CAP`
/// (200 KiB) is a separate, lower-level limit applied by `diff_for_commit`
/// itself; this constant is a further truncation applied at the profile layer.
/// Test: `tests::diff_sampler_truncates_long_diff`.
pub const MAX_DIFF_CHARS: usize = 20_000;

/// Maximum number of diffs to sample per period batch.
///
/// Why: the default max protects against periods with many commits all being
/// fed into the LLM in one shot.
/// What: 5 — enough for qualitative coverage without excessive token usage.
/// Callers may override this via `DiffSamplerConfig::max_diffs`.
/// Test: `tests::diff_sampler_respects_max_diffs`.
pub const DEFAULT_MAX_DIFFS: usize = 5;

/// Configuration for the diff sampler.
///
/// Why: callers need to tune sampling parameters (number of diffs, repo paths)
/// without changing function signatures.
/// What: holds the maximum number of diffs per period and the map from
/// repository name to local filesystem path used by `diff_for_commit`.
/// Test: `DiffSamplerConfig::default()` is exercised by all sampler tests.
#[derive(Debug, Clone)]
pub struct DiffSamplerConfig {
    /// Maximum number of diffs to sample per period batch.
    /// Defaults to [`DEFAULT_MAX_DIFFS`].
    pub max_diffs: usize,

    /// Map from `commits.repository` (as stored in tga) to the local path
    /// of that repository.  Used to open the repo for `diff_for_commit`.
    ///
    /// When a repository is not present in this map, or when the mapped path
    /// does not exist, the commit is skipped with a warning.
    pub repo_paths: HashMap<String, PathBuf>,

    /// Optional root directory under which repos are located.
    ///
    /// When set, repo paths are resolved as `repos_root / repository_name`.
    /// `repo_paths` takes precedence over `repos_root` for individual repos.
    pub repos_root: Option<PathBuf>,
}

impl Default for DiffSamplerConfig {
    fn default() -> Self {
        Self {
            max_diffs: DEFAULT_MAX_DIFFS,
            repo_paths: HashMap::new(),
            repos_root: None,
        }
    }
}

impl DiffSamplerConfig {
    /// Resolve the local filesystem path for a given repository name.
    ///
    /// Why: callers may configure either an explicit `repo_paths` map or a
    /// `repos_root`; this method encapsulates the resolution precedence.
    /// What: returns the explicit `repo_paths` entry if present; otherwise
    /// tries `repos_root / repo_name`; returns `None` when neither is set.
    /// Test: `tests::config_repo_path_resolution`.
    pub fn repo_path(&self, repo_name: &str) -> Option<PathBuf> {
        if let Some(p) = self.repo_paths.get(repo_name) {
            return Some(p.clone());
        }
        self.repos_root.as_ref().map(|root| root.join(repo_name))
    }
}
