//! Configuration and limits for the diff sampler.
//!
//! Why: the sampler's two tunables — how many diffs per period and where the
//! repositories live on disk — belong to the caller, not the function
//! signature.
//! What: defines [`DiffSamplerConfig`] plus the [`MAX_DIFF_CHARS`] and
//! [`DEFAULT_MAX_DIFFS`] limits.
//! Test: `config_repo_path_resolution` in the sibling `tests` module.

use std::collections::HashMap;
use std::path::PathBuf;

/// Maximum characters of diff text kept per sampled commit.
///
/// Why: one 40,000-line refactor would otherwise consume the entire budget a
/// downstream narrative pass has for the whole profile.
/// What: 20,000 UTF-8 characters, applied on top of the byte cap
/// [`diff_for_commit`](crate::collect::git::diff::diff_for_commit) already
/// enforces.
/// Test: `diff_sampler_truncates_long_diff`.
pub const MAX_DIFF_CHARS: usize = 20_000;

/// Default number of diffs sampled per period batch.
///
/// Why: enough commits to see a pattern, few enough that a busy period does not
/// dominate the profile.
/// What: 5. Override via [`DiffSamplerConfig::max_diffs`].
/// Test: `diff_sampler_respects_max_diffs` exercises a non-default value.
pub const DEFAULT_MAX_DIFFS: usize = 5;

/// Configuration for the diff sampler.
///
/// Why: see the module doc.
/// What: holds the per-period diff cap and the mapping from the repository
/// name stored in `commits.repository` to a local checkout. Repositories with
/// no resolvable path are skipped.
/// Test: `config_repo_path_resolution`.
#[derive(Debug, Clone)]
pub struct DiffSamplerConfig {
    /// Maximum diffs sampled per period batch. Defaults to
    /// [`DEFAULT_MAX_DIFFS`].
    pub max_diffs: usize,

    /// Explicit repository-name → local-path map. Takes precedence over
    /// [`repos_root`](Self::repos_root).
    pub repo_paths: HashMap<String, PathBuf>,

    /// Root directory under which repositories are laid out by name.
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
    /// Resolve the local path for a repository name.
    ///
    /// Why: callers configure either an explicit map or a root directory, and
    /// the precedence between them has to be stated once.
    /// What: returns the explicit entry if present, else `repos_root/<name>`,
    /// else `None`.
    /// Test: `config_repo_path_resolution`.
    pub fn repo_path(&self, repo_name: &str) -> Option<PathBuf> {
        if let Some(p) = self.repo_paths.get(repo_name) {
            return Some(p.clone());
        }
        self.repos_root.as_ref().map(|root| root.join(repo_name))
    }
}
