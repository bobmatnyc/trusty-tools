//! Project registry: lifecycle management and auto-registration.
//!
//! Why: the registry is the single authoritative source for known projects.
//! It seeds from `config.yaml`'s `projects:` list on startup, auto-registers
//! projects implied by session history, and exposes typed register/get/list ops
//! for the MCP tools and CLI.
//! What: [`ProjectRegistry`] wraps a [`ProjectStore`] in an async `RwLock`,
//! exposes `register` (idempotent upsert), `get`, and `list`; `seed_from_config`
//! upserts statically-declared entries; `auto_register_from_sessions` derives
//! implicit entries from existing session records.
//! Test: `registry_register_idempotent`, `registry_list`, `registry_get`,
//! `registry_seed_from_config`, `registry_auto_register_from_sessions`,
//! `registry_derive_name_skips_missing_url`.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::record::{Project, derive_name_from_url};
use super::store::{ProjectStore, ProjectStoreError};
use crate::core::trusty_tools_config::ProjectConfig;
use crate::session_manager::record::SessionRecord;

/// Async project registry backed by an on-disk JSON store.
///
/// Why: the daemon and MCP tools need a single shared, thread-safe view of all
/// known projects. Wrapping the store in an `Arc<RwLock<…>>` follows the same
/// pattern as the session manager.
/// What: wraps a `ProjectStore` with async shared-mutable access; all mutating
/// ops (register, seed, auto-register) take a write lock; reads take a read
/// lock by re-reading through the store.
/// Test: `registry_register_idempotent`, `registry_list`, `registry_get`.
#[derive(Debug, Clone)]
pub struct ProjectRegistry {
    store: Arc<RwLock<ProjectStore>>,
}

impl ProjectRegistry {
    /// Load (or create) the project registry at the given data directory.
    ///
    /// Why: on startup the daemon restores the registry from the previous run;
    /// an absent `projects.json` starts a fresh, empty registry.
    /// What: constructs a `ProjectStore` from `<data_dir>/projects.json` and
    /// wraps it in an `Arc<RwLock>`.
    /// Test: `registry_register_idempotent`.
    pub async fn load(data_dir: &Path) -> Result<Self, ProjectStoreError> {
        let store = ProjectStore::load(data_dir).await?;
        Ok(Self {
            store: Arc::new(RwLock::new(store)),
        })
    }

    /// Idempotent upsert: register or update a project by name.
    ///
    /// Why: registration must be safe to call multiple times — re-registering
    /// with updated fields updates rather than duplicates the entry.
    /// What: acquires a write lock and calls `ProjectStore::upsert`.
    /// Test: `registry_register_idempotent`.
    pub async fn register(&self, project: Project) -> Result<(), ProjectStoreError> {
        let mut guard = self.store.write().await;
        guard.upsert(project).await
    }

    /// Look up a project by name.
    ///
    /// Why: MCP `project_get` and session spawning need a point-lookup by name.
    /// What: acquires a write lock (required by store reload semantics) and
    /// returns a clone of the matching project or `ProjectStoreError::NotFound`.
    /// Test: `registry_get`.
    pub async fn get(&self, name: &str) -> Result<Project, ProjectStoreError> {
        let mut guard = self.store.write().await;
        guard.get(name).await
    }

    /// Return all registered projects.
    ///
    /// Why: MCP `project_list` needs the full set; the auto-registration pass
    /// uses it to avoid re-registering existing entries.
    /// What: acquires a write lock (required by store reload semantics) and
    /// returns all project records.
    /// Test: `registry_list`.
    pub async fn list(&self) -> Result<Vec<Project>, ProjectStoreError> {
        let mut guard = self.store.write().await;
        guard.all().await
    }

    /// Seed the registry from statically-declared `projects:` config entries.
    ///
    /// Why: #1519 MR-1 — operators can declare projects in
    /// `~/.trusty-tools/trusty-mpm/config.yaml` under a `projects:` key so the
    /// registry is pre-populated without manual MCP calls.
    /// What: upserts each `ProjectConfig` entry into the store. Errors on
    /// individual entries are logged and skipped so a bad entry does not abort
    /// the whole seed pass.
    /// Test: `registry_seed_from_config`.
    pub async fn seed_from_config(&self, config_projects: &[ProjectConfig]) {
        for entry in config_projects {
            let project = Project {
                name: entry.name.clone(),
                repo_url: entry.repo_url.clone(),
                default_branch: entry
                    .default_branch
                    .clone()
                    .unwrap_or_else(|| "main".to_string()),
                stack_hint: entry.stack_hint.clone(),
                tags: entry.tags.clone().unwrap_or_default(),
                description: entry.description.clone(),
                gh_user: entry.gh_user.clone(),
                // #2184: mirror the per-project gh identity + commit identity
                // onto the registry record so `project_get`/`project_list`
                // surface it without a separate config-file read.
                github: entry.github.clone(),
                commit_name: entry.commit_name.clone(),
                commit_email: entry.commit_email.clone(),
            };
            let name = project.name.clone();
            if let Err(e) = self.register(project).await {
                warn!(name = %name, "seed_from_config: failed to register project: {e}");
            } else {
                debug!(name = %name, "seed_from_config: registered project");
            }
        }
    }

    /// Auto-register projects implied by existing session records.
    ///
    /// Why: #1519 MR-1 boot auto-registration — the daemon's reconcile pass
    /// already has access to all session records; this pass derives `Project`
    /// entries from records that have a `repo_url` but are not yet in the
    /// registry, populating it without operator effort.
    /// What: for each session record with a `repo_url` not already registered,
    /// derives a name from the repo URL basename (stripping `.git`) and upserts
    /// an implicit `Project`. The `default_branch` comes from the record's
    /// `branch` field, falling back to `"main"`.
    /// Test: `registry_auto_register_from_sessions`,
    /// `registry_derive_name_skips_missing_url`.
    pub async fn auto_register_from_sessions(&self, records: &[SessionRecord]) {
        // Collect existing names to avoid overwriting manually-registered entries.
        let existing: std::collections::HashSet<String> = match self.list().await {
            Ok(projects) => projects.into_iter().map(|p| p.name).collect(),
            Err(e) => {
                warn!("auto_register: failed to list existing projects: {e}");
                return;
            }
        };

        for record in records {
            let Some(repo_url) = &record.repo_url else {
                continue;
            };
            let Some(name) = derive_name_from_url(repo_url) else {
                warn!(
                    repo_url = %repo_url,
                    "auto_register: could not derive project name from URL; skipping"
                );
                continue;
            };
            if existing.contains(&name) {
                debug!(name = %name, "auto_register: project already registered; skipping");
                continue;
            }
            let default_branch = record.branch.clone().unwrap_or_else(|| "main".to_string());
            let project = Project {
                name: name.clone(),
                repo_url: repo_url.clone(),
                default_branch,
                stack_hint: None,
                tags: vec![],
                description: None,
                gh_user: None,
                // No source of a gh/commit identity binding for an implicitly
                // auto-registered project (#2184) — the operator declares
                // these explicitly via `config.projects`/`config_write`.
                github: None,
                commit_name: None,
                commit_email: None,
            };
            if let Err(e) = self.register(project).await {
                warn!(name = %name, "auto_register: failed to register: {e}");
            } else {
                info!(name = %name, repo_url = %repo_url, "auto_register: registered project from session history");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_session_with_repo(repo_url: &str, branch: Option<&str>) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-test".into(),
            cwd: PathBuf::from("/tmp"),
            task: "test".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: Some(repo_url.to_string()),
            branch: branch.map(String::from),
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
        }
    }

    fn make_session_no_repo() -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-no-repo".into(),
            cwd: PathBuf::from("/tmp"),
            task: "no repo".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
        }
    }

    #[tokio::test]
    async fn registry_register_idempotent() {
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");

        let p = Project {
            name: "alpha".into(),
            repo_url: "https://github.com/o/alpha".into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            github: None,
            commit_name: None,
            commit_email: None,
        };
        registry.register(p.clone()).await.expect("register once");
        registry
            .register(p)
            .await
            .expect("register again (idempotent)");

        let all = registry.list().await.expect("list");
        assert_eq!(all.len(), 1, "idempotent registration must not duplicate");
    }

    #[tokio::test]
    async fn registry_get() {
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");

        let p = Project {
            name: "beta".into(),
            repo_url: "https://github.com/o/beta".into(),
            default_branch: "develop".into(),
            stack_hint: Some("rust".into()),
            tags: vec!["backend".into()],
            description: Some("desc".into()),
            gh_user: Some("bob-work".into()),
            github: None,
            commit_name: None,
            commit_email: None,
        };
        registry.register(p.clone()).await.expect("register");

        let got = registry.get("beta").await.expect("get");
        assert_eq!(got.name, "beta");
        assert_eq!(got.default_branch, "develop");
        assert_eq!(got.stack_hint.as_deref(), Some("rust"));
        assert_eq!(got.gh_user.as_deref(), Some("bob-work"));

        let err = registry.get("nonexistent").await;
        assert!(matches!(err, Err(ProjectStoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn registry_list() {
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");

        for name in ["gamma", "delta", "epsilon"] {
            let p = Project {
                name: name.into(),
                repo_url: format!("https://github.com/o/{name}"),
                default_branch: "main".into(),
                stack_hint: None,
                tags: vec![],
                description: None,
                gh_user: None,
                github: None,
                commit_name: None,
                commit_email: None,
            };
            registry.register(p).await.expect("register");
        }

        let all = registry.list().await.expect("list");
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn registry_seed_from_config() {
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");

        let config_entries = vec![
            ProjectConfig {
                name: "from-config-a".into(),
                repo_url: "https://github.com/o/from-config-a".into(),
                default_branch: Some("main".into()),
                stack_hint: Some("python".into()),
                tags: Some(vec!["ml".into()]),
                description: Some("ml project".into()),
                gh_user: Some("bobmatnyc".into()),
                github: Some(crate::core::trusty_tools_config::GithubConfig {
                    config_dir: Some("/home/bob/.config/gh-ml".into()),
                    token_env: None,
                    account: None,
                    host: None,
                }),
                commit_name: Some("ML Bot".into()),
                commit_email: Some("ml-bot@example.com".into()),
                untracked_sync: None,
            },
            ProjectConfig {
                name: "from-config-b".into(),
                repo_url: "https://github.com/o/from-config-b".into(),
                default_branch: None,
                stack_hint: None,
                tags: None,
                description: None,
                gh_user: None,
                github: None,
                commit_name: None,
                commit_email: None,
                untracked_sync: None,
            },
        ];

        registry.seed_from_config(&config_entries).await;

        let all = registry.list().await.expect("list after seed");
        let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"from-config-a"), "{names:?}");
        assert!(names.contains(&"from-config-b"), "{names:?}");

        let a = registry.get("from-config-a").await.expect("get a");
        assert_eq!(a.stack_hint.as_deref(), Some("python"));
        assert_eq!(a.default_branch, "main");
        assert_eq!(a.gh_user.as_deref(), Some("bobmatnyc"));
        // #2184: the github/commit-identity binding must be mirrored onto the
        // registry record verbatim.
        assert_eq!(
            a.github.as_ref().and_then(|g| g.config_dir.as_deref()),
            Some(std::path::Path::new("/home/bob/.config/gh-ml"))
        );
        assert_eq!(a.commit_name.as_deref(), Some("ML Bot"));
        assert_eq!(a.commit_email.as_deref(), Some("ml-bot@example.com"));

        // Entry without default_branch must fall back to "main"; gh_user and
        // the #2184 github/commit-identity fields stay unset (no regression
        // for configs that predate #2081/#2184).
        let b = registry.get("from-config-b").await.expect("get b");
        assert_eq!(b.default_branch, "main");
        assert_eq!(b.gh_user, None);
        assert_eq!(b.github, None);
        assert_eq!(b.commit_name, None);
        assert_eq!(b.commit_email, None);
    }

    #[tokio::test]
    async fn registry_auto_register_from_sessions() {
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");

        let sessions = vec![
            make_session_with_repo("https://github.com/owner/trusty-tools.git", Some("main")),
            make_session_with_repo("https://github.com/owner/another-repo", None),
            make_session_no_repo(), // must be silently skipped
        ];
        registry.auto_register_from_sessions(&sessions).await;

        let all = registry.list().await.expect("list after auto-register");
        let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"trusty-tools"), "{names:?}");
        assert!(names.contains(&"another-repo"), "{names:?}");
        assert_eq!(all.len(), 2, "no-repo session must be skipped");

        let p = registry
            .get("trusty-tools")
            .await
            .expect("get trusty-tools");
        assert_eq!(p.default_branch, "main");
        assert_eq!(p.repo_url, "https://github.com/owner/trusty-tools.git");

        let p2 = registry
            .get("another-repo")
            .await
            .expect("get another-repo");
        assert_eq!(p2.default_branch, "main", "no branch falls back to main");
    }

    #[tokio::test]
    async fn registry_auto_register_skips_already_registered() {
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");

        // Pre-register with explicit description.
        let existing = Project {
            name: "trusty-tools".into(),
            repo_url: "https://github.com/owner/trusty-tools".into(),
            default_branch: "develop".into(),
            stack_hint: Some("rust".into()),
            tags: vec![],
            description: Some("manually registered".into()),
            gh_user: None,
            github: None,
            commit_name: None,
            commit_email: None,
        };
        registry.register(existing).await.expect("pre-register");

        // Auto-register with a different URL for the same name — must not overwrite.
        let sessions = vec![make_session_with_repo(
            "https://github.com/owner/trusty-tools.git",
            Some("feature"),
        )];
        registry.auto_register_from_sessions(&sessions).await;

        let p = registry.get("trusty-tools").await.expect("get");
        assert_eq!(
            p.description.as_deref(),
            Some("manually registered"),
            "auto-register must not overwrite existing entry"
        );
        assert_eq!(p.default_branch, "develop");
    }
}
