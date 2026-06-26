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
/// Subdirectory (under the framework root) that holds the synced catalog.
const CATALOG_SUBDIR: &str = "catalog";

/// The canonical catalog root directory for a given framework root.
///
/// Why: BEFORE this helper, two launch paths computed the catalog root
/// independently — `session_launch::prepare_session` used `fw.root.join("catalog")`
/// while the `tm catalog` CLI used `dirs::home_dir().join(".trusty-mpm/catalog")`.
/// They agree in production but the duplication invited drift, and nothing tied
/// the manifest path the launcher reads to the checkout `CatalogSync` actually
/// populates. Centralizing the join here makes "where the catalog lives" a single
/// definition both paths share.
/// What: returns `<framework_root>/catalog`. The framework root is `~/.trusty-mpm`
/// in production ([`crate::core::paths::FrameworkPaths::root`]); in tests it is a
/// tempdir root, so the catalog stays hermetic.
/// Test: `catalog_root_for_joins_subdir`.
pub fn catalog_root_for(framework_root: &Path) -> PathBuf {
    framework_root.join(CATALOG_SUBDIR)
}
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
        Self::with_config(git, catalog_dir, None)
    }

    /// Construct a CatalogSync resolving repo/ref/TTL with the HR-2 precedence:
    /// `[manifest]` config > `TRUSTY_MPM_CATALOG_*` env > compiled-in default.
    ///
    /// Why: HR-2 adds a persistent, file-based way to control the catalog source
    /// (a user fork, a pinned ref, a shorter TTL) via the `[manifest]` section of
    /// `config.toml`, on top of the existing env knobs. Threading the optional
    /// config through one constructor keeps the precedence in a single place and
    /// preserves the env-only behavior of [`new`] (which passes `None`).
    /// What: for each of repo, ref, and TTL, takes the config value when set,
    /// else the env var, else the compiled-in default.
    /// Test: `catalog_config_overrides_env_and_default`.
    pub fn with_config(
        git: G,
        catalog_dir: PathBuf,
        config: Option<&crate::core::config::ManifestConfig>,
    ) -> Self {
        let env_ttl = std::env::var("TRUSTY_MPM_CATALOG_TTL_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok());
        let ttl_hours = config
            .and_then(|c| c.ttl_hours)
            .or(env_ttl)
            .unwrap_or(DEFAULT_TTL_HOURS);

        let env_repo = std::env::var("TRUSTY_MPM_CATALOG_REPO").ok();
        let repo_url = config
            .and_then(|c| c.repo.clone())
            .or(env_repo)
            .unwrap_or_else(|| DEFAULT_CATALOG_REPO.to_owned());

        let env_ref = std::env::var("TRUSTY_MPM_CATALOG_REF").ok();
        let git_ref = config
            .and_then(|c| c.git_ref.clone())
            .or(env_ref)
            .unwrap_or_else(|| DEFAULT_CATALOG_REF.to_owned());

        Self {
            git,
            catalog_dir,
            repo_url,
            git_ref,
            ttl_secs: ttl_hours * 3600,
        }
    }

    /// Construct a CatalogSync rooted at the framework's canonical catalog dir,
    /// threading the `[manifest]` config through `with_config`.
    ///
    /// Why: this is the single constructor BOTH launch paths share so the catalog
    /// root, repo, ref, and TTL come from one place. `session_launch` derives the
    /// manifest path and source dirs from `catalog_root()`; the `tm catalog` CLI
    /// drives `sync`. Building both from the same framework root + config means the
    /// manifest the launcher reads is the same checkout the CLI populates.
    /// What: computes the root via [`catalog_root_for`] and delegates to
    /// [`with_config`](Self::with_config) so the repo/ref/TTL precedence
    /// (`[manifest]` config > `TRUSTY_MPM_CATALOG_*` env > default) is preserved.
    /// Test: `for_framework_uses_catalog_root`.
    pub fn for_framework(
        git: G,
        framework_root: &Path,
        config: Option<&crate::core::config::ManifestConfig>,
    ) -> Self {
        Self::with_config(git, catalog_root_for(framework_root), config)
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
    /// not expired (and force=false), skips the fetch. Otherwise decides whether
    /// to clone fresh, update in place, or recover a corrupt/wrong-remote checkout
    /// (see `ensure_repo`) then writes the sentinel.
    /// Test: catalog_sync_fetches_on_first_call, catalog_sync_skips_on_ttl_valid,
    /// catalog_sync_second_sync_succeeds, catalog_sync_corrupt_dir_reclones,
    /// catalog_sync_wrong_remote_reclones.
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

        // Ensure the catalog parent directory exists before any git operation.
        std::fs::create_dir_all(&self.catalog_dir)?;

        let clone_target = self.catalog_dir.join("repo");
        self.ensure_repo(&clone_target)?;

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

    /// Idempotently bring the repo checkout at `target` up to date.
    ///
    /// Why: this is the core fix for #1751 — the previous code always called
    /// `git clone` which fails when the directory already exists. This function
    /// decides clone-vs-update-vs-reclone so a second sync always succeeds.
    /// What: three cases:
    ///   1. `target` absent → fresh clone.
    ///   2. `target` is a git repo whose `origin` matches `repo_url` → fetch +
    ///      hard-reset (update in place).
    ///   3. `target` is not a git repo, or has the wrong `origin`, or
    ///      fetch/reset fails → remove `target` and re-clone.
    ///
    /// Test: `catalog_sync_second_sync_succeeds` (case 2),
    /// `catalog_sync_corrupt_dir_reclones` (case 3, non-git dir),
    /// `catalog_sync_wrong_remote_reclones` (case 3, wrong remote).
    fn ensure_repo(&self, target: &Path) -> Result<(), CatalogError> {
        if !target.exists() {
            return self.do_clone(target);
        }
        if !self.git.is_git_repo(target) {
            warn!(path = %target.display(), "catalog dir exists but is not a git repo; re-cloning");
            guard_remove(target, &self.catalog_dir)?;
            return self.do_clone(target);
        }
        match self.git.remote_url(target) {
            Ok(ref url) if urls_match(url, &self.repo_url) => {
                info!(dir = %target.display(), "catalog repo up to date remote; fetching updates");
                self.do_update(target)
            }
            Ok(actual) => {
                warn!(
                    actual = %actual,
                    expected = %self.repo_url,
                    "catalog repo has wrong remote; re-cloning"
                );
                guard_remove(target, &self.catalog_dir)?;
                self.do_clone(target)
            }
            Err(e) => {
                warn!(err = %e, "catalog repo appears corrupt; re-cloning");
                guard_remove(target, &self.catalog_dir)?;
                self.do_clone(target)
            }
        }
    }

    /// Clone the repo into `target` (case 1 of `ensure_repo`).
    ///
    /// Why: thin wrapper that translates ProvisionError → CatalogError.
    /// What: delegates to `self.git.clone_repo` with the configured URL and ref.
    /// Test: exercised by catalog_sync_fetches_on_first_call.
    fn do_clone(&self, target: &Path) -> Result<(), CatalogError> {
        self.git
            .clone_repo(&self.repo_url, &self.git_ref, target)
            .map_err(|e| CatalogError::Git(e.to_string()))
    }

    /// Update an existing valid checkout in place (case 2 of `ensure_repo`).
    ///
    /// Why: fetch + hard-reset is idempotent and handles local drift without a
    /// full re-clone; falls back to re-clone if the update itself fails.
    /// What: calls `self.git.fetch_and_reset`; on failure removes `target` and
    /// calls `do_clone` to recover.
    /// Test: exercised by catalog_sync_second_sync_succeeds.
    fn do_update(&self, target: &Path) -> Result<(), CatalogError> {
        match self.git.fetch_and_reset(target, &self.git_ref) {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(err = %e, "catalog fetch/reset failed; re-cloning from scratch");
                guard_remove(target, &self.catalog_dir)?;
                self.do_clone(target)
            }
        }
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

    /// Resolve the directory that holds a given catalog resource type.
    ///
    /// Why: the claude-mpm repo stores agents and skills under `repo/.claude/agents`
    /// and `repo/.claude/skills/`; older forks or mirrors may use the flat
    /// `repo/agents` / `repo/skills` layout. This helper tries the preferred
    /// `.claude/<subdir>` location first and falls back to the legacy flat path.
    /// What: returns the first of (`repo/.claude/<subdir>`, `repo/<subdir>`) that
    /// exists as a directory; falls back to `repo/<subdir>` when neither exists.
    /// Test: catalog_ls_lists_agents uses the `.claude/agents` path after the real
    /// repo layout was confirmed via `find ~/.trusty-mpm/catalog/repo/.claude`.
    fn catalog_subdir(&self, subdir: &str) -> std::path::PathBuf {
        let preferred = self.catalog_dir.join("repo").join(".claude").join(subdir);
        if preferred.is_dir() {
            return preferred;
        }
        // Fall back to the legacy flat layout.
        self.catalog_dir.join("repo").join(subdir)
    }

    /// Count files in a catalog subdirectory (agents or skills).
    ///
    /// Why: the CLI needs to show how many agents and skills are cached.
    /// What: resolves the correct subdir path via `catalog_subdir`, then counts
    /// non-directory entries. Skills may be stored as directories (one dir per
    /// skill containing SKILL.md), so for skills we count directories instead.
    /// Test: catalog_sync_fetches_on_first_call asserts agent_count >= 0.
    fn count_files(&self, subdir: &str) -> usize {
        let dir = self.catalog_subdir(subdir);
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                let entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                // Skills are stored as directories (one dir per skill).
                // Agents are stored as flat .md files. Count whichever is present.
                let file_count = entries
                    .iter()
                    .filter(|e| e.file_type().map(|t| !t.is_dir()).unwrap_or(false))
                    .count();
                let dir_count = entries
                    .iter()
                    .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                    .count();
                // Return whichever is non-zero; prefer files (agents) over dirs (skills).
                if file_count > 0 {
                    file_count
                } else {
                    dir_count
                }
            }
            Err(_) => 0,
        }
    }

    /// List agent file names from the cached catalog.
    ///
    /// Why: `tm catalog ls` needs a listing of available agents.
    /// What: returns file stems from `catalog_subdir("agents")` — preferring
    /// `.claude/agents` over the legacy `agents` flat path.
    /// Test: catalog_ls_lists_agents.
    pub fn list_agents(&self) -> Vec<String> {
        list_names(&self.catalog_subdir("agents"))
    }

    /// List skill directory names from the cached catalog.
    ///
    /// Why: `tm catalog ls` needs a listing of available skills.
    /// What: returns directory names from `catalog_subdir("skills")` — preferring
    /// `.claude/skills` over the legacy `skills` flat path. Skills are stored
    /// as one directory per skill containing a SKILL.md file.
    /// Test: catalog_ls_lists_skills.
    pub fn list_skills(&self) -> Vec<String> {
        list_skill_names(&self.catalog_subdir("skills"))
    }

    /// Path to the harness manifest inside the synced catalog (HR-2).
    ///
    /// Why: HR-2 sources the catalog-layer harness manifest from the synced
    /// claude-mpm checkout; the session-launch resolver
    /// (`crate::core::manifest::ManifestSources`) reads this exact path as its
    /// lowest-of-the-overrides precedence layer. Exposing it on `CatalogSync`
    /// keeps the catalog repo/ref/TTL configuration (the env knobs threaded into
    /// `new`) as the single source of truth for WHERE the catalog manifest lives.
    /// What: `<catalog_dir>/repo/.claude/manifest.toml` — the conventional
    /// location of a `manifest.toml` committed at the catalog repo's `.claude/`
    /// root. The file need not exist; the resolver tolerates absence.
    /// Test: `catalog_manifest_path_points_at_repo_dotclaude`.
    pub fn manifest_path(&self) -> PathBuf {
        self.catalog_dir
            .join("repo")
            .join(".claude")
            .join("manifest.toml")
    }

    /// The catalog root directory (`~/.trusty-mpm/catalog/` in production).
    ///
    /// Why: `crate::core::manifest::ManifestSources::resolve` and
    /// `HarnessPlan::from_manifest` derive the catalog manifest path and the
    /// catalog agent/skill source dirs from this root; exposing it lets a caller
    /// thread the *configured* catalog root (which honours the env knobs) into
    /// manifest resolution instead of recomputing the join.
    /// What: returns the `catalog_dir` this `CatalogSync` was constructed with.
    /// Test: `catalog_root_returns_catalog_dir`.
    pub fn catalog_root(&self) -> &Path {
        &self.catalog_dir
    }
}

/// Safely remove the catalog repo directory, refusing to delete outside the catalog root.
///
/// Why: guards against bugs that could accidentally remove an arbitrary path; the
/// remove target must always be a subdirectory of the catalog root.
/// What: returns an error if `target` is not rooted under `catalog_dir`, otherwise
/// calls `remove_dir_all`.
/// Test: guard_remove_rejects_path_outside_catalog.
fn guard_remove(target: &Path, catalog_dir: &Path) -> Result<(), CatalogError> {
    if !target.starts_with(catalog_dir) {
        return Err(CatalogError::Git(format!(
            "safety: refusing to remove '{}' — not inside catalog dir '{}'",
            target.display(),
            catalog_dir.display()
        )));
    }
    std::fs::remove_dir_all(target).map_err(CatalogError::Io)
}

/// Return true if two git remote URLs refer to the same repository.
///
/// Why: `git remote get-url` and the configured URL may differ by trailing
/// slash or `.git` suffix; normalising before comparison avoids spurious
/// re-clones when the URL forms are equivalent.
/// What: lower-cases both URLs and strips trailing `/` and `.git`.
/// Test: urls_match_normalises_variants.
fn urls_match(a: &str, b: &str) -> bool {
    normalize_repo_url(a) == normalize_repo_url(b)
}

fn normalize_repo_url(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .to_lowercase()
}

/// Return the file stems in a directory (for flat-file layouts like agents).
///
/// Why: catalog listing needs clean names without extensions.
/// What: reads directory entries, skips directories, strips extensions, and
/// returns sorted names.
/// Test: used by list_agents which is tested in unit tests.
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

/// Return the subdirectory names in a skills directory.
///
/// Why: in the claude-mpm repo, each skill is stored as a directory containing
/// a SKILL.md file (e.g. `.claude/skills/trusty-memory/SKILL.md`). The catalog
/// listing should show skill directory names, not file extensions.
/// What: reads directory entries, includes only directories, and returns sorted
/// names. Falls back to flat-file stems when no directories are found (legacy).
/// Test: used by list_skills which is tested in unit tests.
fn list_skill_names(dir: &Path) -> Vec<String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            let all: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            // Prefer directory-per-skill layout (`.claude/skills/<name>/SKILL.md`).
            let dir_names: Vec<String> = all
                .iter()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| {
                    e.path()
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_owned())
                })
                .collect();
            if !dir_names.is_empty() {
                let mut sorted = dir_names;
                sorted.sort();
                return sorted;
            }
            // Fall back: flat .md files (legacy layout).
            let mut names: Vec<String> = all
                .iter()
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

        // Simulate the real claude-mpm repo layout: agents are at
        // repo/.claude/agents/ (not repo/agents/).
        let agents_dir = root.path().join("repo").join(".claude").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("engineer.md"), "# engineer").unwrap();
        std::fs::write(agents_dir.join("qa.md"), "# qa").unwrap();

        let agents = sync.list_agents();
        assert!(
            agents.contains(&"engineer".to_owned()),
            "agents: {agents:?}"
        );
        assert!(agents.contains(&"qa".to_owned()), "agents: {agents:?}");
    }

    #[test]
    fn catalog_ls_lists_skills() {
        let root = TempDir::new().unwrap();
        let sync = make_sync(&root);
        sync.sync(false).unwrap();

        // Simulate the real claude-mpm repo layout: skills are stored as
        // directories under repo/.claude/skills/ (one dir per skill).
        let skills_dir = root.path().join("repo").join(".claude").join("skills");
        std::fs::create_dir_all(skills_dir.join("trusty-memory")).unwrap();
        std::fs::create_dir_all(skills_dir.join("auto-bug-reporter")).unwrap();
        std::fs::write(
            skills_dir.join("trusty-memory").join("SKILL.md"),
            "# trusty-memory",
        )
        .unwrap();

        let skills = sync.list_skills();
        assert!(
            skills.contains(&"trusty-memory".to_owned()),
            "skills: {skills:?}"
        );
        assert!(
            skills.contains(&"auto-bug-reporter".to_owned()),
            "skills: {skills:?}"
        );
    }

    #[test]
    fn catalog_config_overrides_env_and_default() {
        // HR-2: a `[manifest]` config value must win over the env var and the
        // compiled-in default. Config has highest precedence, so this holds
        // regardless of whether the env vars happen to be set in the test host.
        let root = TempDir::new().unwrap();
        let cfg = crate::core::config::ManifestConfig {
            repo: Some("https://github.com/me/fork".to_owned()),
            git_ref: Some("dev".to_owned()),
            ttl_hours: Some(2),
        };
        let sync =
            CatalogSync::with_config(FakeGitBackend::new(), root.path().to_owned(), Some(&cfg));
        assert_eq!(sync.repo_url, "https://github.com/me/fork");
        assert_eq!(sync.git_ref, "dev");
        assert_eq!(sync.ttl_secs, 2 * 3600);
    }

    #[test]
    fn catalog_manifest_path_points_at_repo_dotclaude() {
        // HR-2: the catalog-layer manifest path must be
        // <catalog>/repo/.claude/manifest.toml so it lines up with the synced
        // claude-mpm repo layout the resolver reads.
        let root = TempDir::new().unwrap();
        let sync = make_sync(&root);
        assert_eq!(
            sync.manifest_path(),
            root.path()
                .join("repo")
                .join(".claude")
                .join("manifest.toml")
        );
    }

    #[test]
    fn catalog_root_returns_catalog_dir() {
        // The catalog root accessor must return the constructed catalog_dir so a
        // caller can thread the configured root into manifest resolution.
        let root = TempDir::new().unwrap();
        let sync = make_sync(&root);
        assert_eq!(sync.catalog_root(), root.path());
    }

    #[test]
    fn catalog_root_for_joins_subdir() {
        // The shared root helper must place the catalog under <framework>/catalog
        // so both launch paths agree on the location.
        let fw_root = std::path::Path::new("/home/.trusty-mpm");
        assert_eq!(
            super::catalog_root_for(fw_root),
            std::path::PathBuf::from("/home/.trusty-mpm/catalog")
        );
    }

    #[test]
    fn for_framework_uses_catalog_root() {
        // `for_framework` must root the sync at <framework>/catalog (the same root
        // `catalog_root_for` yields) so the CLI and launcher share one checkout.
        let fw_root = TempDir::new().unwrap();
        let sync = CatalogSync::for_framework(FakeGitBackend::new(), fw_root.path(), None);
        assert_eq!(sync.catalog_root(), fw_root.path().join("catalog"));
    }

    #[test]
    fn catalog_ls_falls_back_to_legacy_agent_path() {
        // When .claude/agents doesn't exist, should fall back to repo/agents/.
        let root = TempDir::new().unwrap();
        let sync = make_sync(&root);
        sync.sync(false).unwrap();

        // Use legacy flat path (no .claude dir).
        let agents_dir = root.path().join("repo").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("legacy-agent.md"), "# legacy").unwrap();

        let agents = sync.list_agents();
        assert!(
            agents.contains(&"legacy-agent".to_owned()),
            "agents from legacy path: {agents:?}"
        );
    }
}

// #1751 idempotency tests (uses public API → lives in crates/trusty-mpm/tests/).
// See: tests/catalog_sync_idempotent.rs
