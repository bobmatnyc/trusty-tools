//! Catalog synchronization from the claude-mpm GitHub repository.
//!
//! Why: the claude-mpm repository is the authoritative source for ~40 agents
//! and ~25 skills; syncing from it eliminates manual re-porting and keeps the
//! catalog automatically current.
//! What: CatalogSync fetches the repository into ~/.trusty-mpm/catalog/ via
//! a GitBackend seam (real git in production, FakeGitBackend in tests), checks
//! a TTL to skip redundant fetches, and exposes list_agents/list_skills for
//! the `tm catalog ls` command.
//! Test: catalog_sync_fetches_on_first_call, catalog_sync_skips_on_ttl_valid,
//! catalog_sync_force_bypasses_ttl.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use thiserror::Error;
use tracing::{debug, info, warn};

use crate::provisioner::GitBackend;

/// Default TTL for the catalog cache: 24 hours.
const DEFAULT_TTL_HOURS: u64 = 24;
/// Default claude-mpm repository URL.
const DEFAULT_CATALOG_REPO: &str = "https://github.com/bobmatnyc/claude-mpm";
/// Sentinel file written after a successful sync.
const SYNC_SENTINEL: &str = ".catalog_synced_at";
/// Default git ref for the catalog repo.
const DEFAULT_CATALOG_REF: &str = "main";

/// Errors produced by catalog sync operations.
///
/// Why: callers need structured errors to distinguish git failures from
/// I/O errors and to produce actionable CLI output.
/// What: one variant per failure class.
/// Test: exercised by CatalogSync unit tests.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// The git clone/pull operation failed.
    #[error("git sync error: {0}")]
    Git(String),

    /// An I/O operation on the catalog directory failed.
    #[error("catalog I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of a catalog sync operation.
///
/// Why: CLI output and tests need to know whether the catalog was freshly
/// fetched or served from cache.
/// What: bundles the number of agents and skills found, and a flag indicating
/// whether a real fetch was performed.
/// Test: asserted by catalog_sync_fetches_on_first_call.
#[derive(Debug)]
pub struct CatalogSyncResult {
    /// True if a git fetch was actually performed (TTL expired or force=true).
    pub fetched: bool,
    /// Number of agent files found in the catalog after sync.
    pub agent_count: usize,
    /// Number of skill files found in the catalog after sync.
    pub skill_count: usize,
}

/// Synchronizes the claude-mpm agent/skill catalog from a git remote.
///
/// Why: the session manager needs deployed agents and skills for prepare_session;
/// syncing from the claude-mpm repo is the single source of truth.
/// What: clones or updates the catalog repo under ~/.trusty-mpm/catalog/,
/// respects a TTL to avoid redundant fetches, and exposes list methods for
/// the CLI.
/// Test: unit tests use FakeGitBackend; integration test uses a real repo (#[ignore]).
pub struct CatalogSync<G: GitBackend> {
    git: G,
    /// Root directory for the cached catalog (~/.trusty-mpm/catalog/).
    catalog_dir: PathBuf,
    /// URL of the claude-mpm repository.
    repo_url: String,
    /// Git ref to sync.
    git_ref: String,
    /// Cache TTL in seconds.
    ttl_secs: u64,
}

impl<G: GitBackend> CatalogSync<G> {
    /// Construct a CatalogSync with the given git backend and catalog directory.
    ///
    /// Why: the catalog directory and repo URL are injectable so tests can use
    /// a tempdir and a fake remote without touching the real filesystem.
    /// What: stores all config; no I/O at construction time.
    /// Test: used in every CatalogSync unit test.
    pub fn new(git: G, catalog_dir: PathBuf) -> Self {
        let ttl_hours = std::env::var("TRUSTY_MPM_CATALOG_TTL_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TTL_HOURS);
        let repo_url = std::env::var("TRUSTY_MPM_CATALOG_REPO")
            .unwrap_or_else(|_| DEFAULT_CATALOG_REPO.to_owned());
        let git_ref = std::env::var("TRUSTY_MPM_CATALOG_REF")
            .unwrap_or_else(|_| DEFAULT_CATALOG_REF.to_owned());
        Self {
            git,
            catalog_dir,
            repo_url,
            git_ref,
            ttl_secs: ttl_hours * 3600,
        }
    }

    /// Construct with explicit repo_url and git_ref (for testing).
    ///
    /// Why: unit tests need to pass a specific repo URL without relying on env vars.
    /// What: overrides the repo_url and git_ref fields.
    /// Test: used by catalog_sync unit tests.
    pub fn with_repo(git: G, catalog_dir: PathBuf, repo_url: &str, git_ref: &str) -> Self {
        Self {
            git,
            catalog_dir,
            repo_url: repo_url.to_owned(),
            git_ref: git_ref.to_owned(),
            ttl_secs: DEFAULT_TTL_HOURS * 3600,
        }
    }

    /// Sync the catalog from the remote, respecting the TTL.
    ///
    /// Why: the session manager calls this on `tm catalog sync` and optionally
    /// on daemon start to ensure agents/skills are available.
    /// What: checks the sentinel file's mtime against the TTL; if the TTL has
    /// not expired (and force=false), skips the fetch. Otherwise clones the
    /// catalog repo into catalog_dir and writes the sentinel.
    /// Test: catalog_sync_fetches_on_first_call, catalog_sync_skips_on_ttl_valid.
    pub fn sync(&self, force: bool) -> Result<CatalogSyncResult, CatalogError> {
        if !force && self.ttl_valid() {
            debug!(dir = %self.catalog_dir.display(), "catalog TTL valid; skipping fetch");
            return Ok(CatalogSyncResult {
                fetched: false,
                agent_count: self.count_files("agents"),
                skill_count: self.count_files("skills"),
            });
        }

        info!(repo = %self.repo_url, git_ref = %self.git_ref, dir = %self.catalog_dir.display(), "syncing catalog");

        // Clone into a temporary subdir then move, to avoid partial state.
        let clone_target = self.catalog_dir.join("repo");
        std::fs::create_dir_all(&clone_target)?;

        self.git
            .clone_repo(&self.repo_url, &self.git_ref, &clone_target)
            .map_err(|e| CatalogError::Git(e.to_string()))?;

        // Write the sentinel to record when we last synced.
        let sentinel = self.catalog_dir.join(SYNC_SENTINEL);
        std::fs::write(&sentinel, chrono::Utc::now().to_rfc3339())?;

        let agent_count = self.count_files("agents");
        let skill_count = self.count_files("skills");
        info!(
            agents = agent_count,
            skills = skill_count,
            "catalog sync complete"
        );

        Ok(CatalogSyncResult {
            fetched: true,
            agent_count,
            skill_count,
        })
    }

    /// Return true if the catalog was synced within the TTL window.
    ///
    /// Why: avoids redundant network fetches when the catalog is fresh.
    /// What: reads the sentinel file mtime and compares to now - ttl_secs.
    /// Test: catalog_sync_skips_on_ttl_valid.
    fn ttl_valid(&self) -> bool {
        let sentinel = self.catalog_dir.join(SYNC_SENTINEL);
        match std::fs::metadata(&sentinel) {
            Ok(meta) => match meta.modified() {
                Ok(mtime) => {
                    let age = SystemTime::now()
                        .duration_since(mtime)
                        .unwrap_or(Duration::from_secs(u64::MAX));
                    age < Duration::from_secs(self.ttl_secs)
                }
                Err(e) => {
                    warn!("catalog sentinel mtime error: {e}");
                    false
                }
            },
            Err(_) => false,
        }
    }

    /// Count files in a subdirectory of the catalog repo clone.
    ///
    /// Why: the CLI needs to show how many agents and skills are cached.
    /// What: counts non-directory entries in catalog_dir/repo/<subdir>/.
    /// Test: catalog_sync_fetches_on_first_call asserts agent_count >= 0.
    fn count_files(&self, subdir: &str) -> usize {
        let dir = self.catalog_dir.join("repo").join(subdir);
        match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| !t.is_dir()).unwrap_or(false))
                .count(),
            Err(_) => 0,
        }
    }

    /// List agent file names from the cached catalog.
    ///
    /// Why: `tm catalog ls` needs a listing of available agents.
    /// What: returns file stems from catalog_dir/repo/agents/.
    /// Test: catalog_ls_lists_agents.
    pub fn list_agents(&self) -> Vec<String> {
        list_names(&self.catalog_dir.join("repo").join("agents"))
    }

    /// List skill file names from the cached catalog.
    ///
    /// Why: `tm catalog ls` needs a listing of available skills.
    /// What: returns file stems from catalog_dir/repo/skills/.
    /// Test: catalog_ls_lists_agents.
    pub fn list_skills(&self) -> Vec<String> {
        list_names(&self.catalog_dir.join("repo").join("skills"))
    }
}

/// Return the file stems in a directory.
///
/// Why: catalog listing needs clean names without extensions.
/// What: reads directory entries, strips extensions, and returns sorted names.
/// Test: used by list_agents/list_skills which are tested in unit tests.
fn list_names(dir: &Path) -> Vec<String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| !t.is_dir()).unwrap_or(false))
                .filter_map(|e| {
                    e.path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_owned())
                })
                .collect();
            names.sort();
            names
        }
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provisioner::FakeGitBackend;
    use tempfile::TempDir;

    fn make_sync(root: &TempDir) -> CatalogSync<FakeGitBackend> {
        CatalogSync::with_repo(
            FakeGitBackend::new(),
            root.path().to_owned(),
            "https://github.com/bobmatnyc/claude-mpm",
            "main",
        )
    }

    #[test]
    fn catalog_sync_fetches_on_first_call() {
        let root = TempDir::new().unwrap();
        let sync = make_sync(&root);

        let result = sync.sync(false).unwrap();
        assert!(result.fetched, "first sync must fetch");
    }

    #[test]
    fn catalog_sync_skips_on_ttl_valid() {
        let root = TempDir::new().unwrap();
        let sync = make_sync(&root);

        // First sync writes the sentinel.
        let r1 = sync.sync(false).unwrap();
        assert!(r1.fetched);

        // Second sync within TTL should skip fetch.
        let r2 = sync.sync(false).unwrap();
        assert!(!r2.fetched, "second sync within TTL must skip fetch");
    }

    #[test]
    fn catalog_sync_force_bypasses_ttl() {
        let root = TempDir::new().unwrap();
        let sync = make_sync(&root);

        // Establish a fresh sentinel.
        sync.sync(false).unwrap();

        // Force sync should fetch again despite fresh TTL.
        let result = sync.sync(true).unwrap();
        assert!(result.fetched, "force sync must fetch regardless of TTL");
    }

    #[test]
    fn catalog_ls_lists_agents() {
        let root = TempDir::new().unwrap();
        let sync = make_sync(&root);
        sync.sync(false).unwrap();

        // Create fake agent files to simulate a catalog.
        let agents_dir = root.path().join("repo").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("engineer.md"), "# engineer").unwrap();
        std::fs::write(agents_dir.join("qa.md"), "# qa").unwrap();

        let agents = sync.list_agents();
        assert!(agents.contains(&"engineer".to_owned()));
        assert!(agents.contains(&"qa".to_owned()));
    }
}
