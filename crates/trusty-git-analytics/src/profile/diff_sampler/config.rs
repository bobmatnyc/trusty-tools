//! Configuration and limits for the diff sampler.
//!
//! Why: how many diffs to sample and where the repositories live are per-run
//! choices, and threading them through as positional arguments would force a
//! signature change every time a knob is added.
//! What: defines [`DiffSamplerConfig`] plus the [`MAX_DIFF_CHARS`] and
//! [`DEFAULT_MAX_DIFFS`] limits.
//! Test: `config_repo_path_resolution`.

use std::collections::HashMap;
use std::path::PathBuf;

/// Maximum characters of diff text kept per sampled commit.
///
/// Why: one unusually large commit would otherwise consume the whole context
/// budget a period gets, crowding out every other diff in that period.
/// What: 20,000 UTF-8 characters, roughly 5–10K tokens. This sits above tga's
/// own `DIFF_BYTE_CAP`, which `diff_for_commit` applies first — this is the
/// second, profile-layer limit.
/// Test: `diff_sampler_truncates_long_diff`.
pub const MAX_DIFF_CHARS: usize = 20_000;

/// Default number of diffs sampled per period.
///
/// Five is enough for qualitative coverage without a period's token cost
/// scaling with how busy that period happened to be. Override via
/// [`DiffSamplerConfig::max_diffs`].
///
/// Test: `config_repo_path_resolution`.
pub const DEFAULT_MAX_DIFFS: usize = 5;

/// Per-run diff-sampler settings.
///
/// Why: the database records a repository *name*, but `diff_for_commit` needs a
/// local path — and only the caller knows where the clones are.
/// What: the per-period cap plus two ways to answer "where is this repository":
/// an explicit `repo_paths` entry, or `repos_root` joined with the name.
/// Test: `config_repo_path_resolution`.
#[derive(Debug, Clone)]
pub struct DiffSamplerConfig {
    /// Maximum diffs sampled per period. Defaults to [`DEFAULT_MAX_DIFFS`].
    pub max_diffs: usize,

    /// Explicit map from `commits.repository` to a local checkout path.
    ///
    /// A repository absent here — or mapped to a path that does not exist — is
    /// skipped with a warning rather than aborting the run.
    pub repo_paths: HashMap<String, PathBuf>,

    /// Root directory under which repositories are laid out by name.
    ///
    /// Used as `repos_root / repository_name` when `repo_paths` has no entry.
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
    /// An explicit `repo_paths` entry wins over `repos_root`; with neither set,
    /// returns `None` and the sampler skips that repository.
    ///
    /// Test: `config_repo_path_resolution`.
    pub fn repo_path(&self, repo_name: &str) -> Option<PathBuf> {
        if let Some(p) = self.repo_paths.get(repo_name) {
            return Some(p.clone());
        }
        self.repos_root.as_ref().map(|root| root.join(repo_name))
    }
}
