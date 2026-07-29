//! Project registry: lifecycle management and auto-registration.
//!
//! Why: the registry is the single authoritative source for known projects.
//! It seeds from `config.yaml`'s `projects:` list on startup, auto-registers
//! projects implied by session history, and exposes typed register/get/list ops
//! for the MCP tools and CLI.
//! What: [`ProjectRegistry`] wraps a [`ProjectStore`] in an async `RwLock`,
//! exposes `register` (idempotent upsert), `get`, and `list`; `seed_from_config`
//! upserts statically-declared entries; `auto_register_from_sessions` derives
//! implicit entries from existing session records. [`update_with`](ProjectRegistry::update_with)
//! is the atomic read-mutate-persist seam a PATCH handler MUST use (#2481 review
//! HIGH): it holds ONE write-lock guard across the whole fetch→mutate→persist
//! sequence, mirroring [`crate::deliverable::manager::DeliverableManager::update_deliverable_with`]
//! (#2395) — `get` followed later by a separate `register` call (two distinct
//! lock acquisitions) leaves a window where a concurrent caller's own
//! get-then-register can land in between, and whichever `register` runs last
//! silently discards the other's update because each call persists a FULL
//! record built from its own (by-then-stale) snapshot.
//! Test: `registry_register_idempotent`, `registry_list`, `registry_get`,
//! `registry_seed_from_config`, `registry_auto_register_from_sessions`,
//! `registry_derive_name_skips_missing_url`,
//! `get_then_register_pattern_reproduces_lost_update_pre_fix`,
//! `update_with_serializes_concurrent_field_edits`,
//! `update_with_unknown_project_errors`.

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

    /// Atomically read-mutate-persist a project under ONE lock.
    ///
    /// Why (fixes the #2481 review HIGH): `PATCH /api/v1/projects/{name}`
    /// originally called [`get`](Self::get) (one write-lock acquisition),
    /// released the lock, mutated a CLONE in the caller, then separately
    /// called [`register`](Self::register) (a SECOND, later acquisition) to
    /// persist the FULL replacement record. Two concurrent PATCHes to the
    /// SAME project — even ones editing completely DIFFERENT fields — can
    /// both fetch the same stale snapshot in the gap between those two calls;
    /// each then persists a full record built from that stale snapshot, so
    /// whichever `register` lands last silently overwrites the other's
    /// already-"successful" field edit with no error ever surfacing to that
    /// caller. This is the exact lost-update hazard #2395 already fixed for
    /// `DeliverableManager::update_deliverable_with`.
    /// What: acquires `self.store.write().await` ONCE (serialising tasks in THIS
    /// process) and delegates the whole fetch→mutate→persist sequence to
    /// [`ProjectStore::update_with`], which performs it inside a single
    /// cross-process file-lock critical section. The in-process guard alone was
    /// never sufficient: it cannot see a `tm` CLI or MCP process editing the same
    /// `projects.json`, so the identical lost-update hazard survived across
    /// processes until the store's write path was serialised. `NotFound`
    /// propagates unchanged, without writing.
    /// Test: `update_with_serializes_concurrent_field_edits` (the concurrency
    /// regression test — proves two concurrent edits to DIFFERENT fields are
    /// both present in the final persisted record, never one lost),
    /// `update_with_unknown_project_errors`, and the cross-process
    /// `projects_json_multiprocess_upsert_no_lost_updates`.
    pub async fn update_with<F>(&self, name: &str, f: F) -> Result<Project, ProjectStoreError>
    where
        F: FnOnce(Project) -> Project + Send + 'static,
    {
        let mut guard = self.store.write().await;
        guard.update_with(name, f).await
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
                // #3025: mirror the pinned spawn-time gh account the same way.
                gh_account: entry.gh_account.clone(),
                // #2184: mirror the per-project gh identity + commit identity
                // onto the registry record so `project_get`/`project_list`
                // surface it without a separate config-file read.
                github: entry.github.clone(),
                commit_name: entry.commit_name.clone(),
                commit_email: entry.commit_email.clone(),
                // #3455: mirror the static "launch on main" opt-out so a
                // project declared with `worktree: false` in config.yaml is
                // seeded with the setting already in effect, no separate
                // `tm projects config` call required.
                worktree: entry.worktree,
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
    /// registry, populating it without operator effort. This is a ONE-SHOT
    /// pass (run only from `DaemonState::project_registry()`'s
    /// `OnceCell::get_or_init`, against whatever session snapshot existed at
    /// the moment ANY caller first touched the registry) — it does NOT run
    /// again for sessions created later in the daemon's lifetime. See
    /// [`Self::register_from_session`] for the per-session, spawn-time
    /// counterpart that closes that gap (#3822).
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
            self.register_implicit_from_record(record, &existing, "auto_register")
                .await;
        }
    }

    /// Register the project implied by ONE newly created session's
    /// `repo_url`, if it is not already registered (#3822).
    ///
    /// Why: [`Self::auto_register_from_sessions`] only ever runs ONCE per
    /// daemon process — whichever of the 15+ `project_registry()` call sites
    /// (a status poll, an MCP tool, `resolve_gh_env`, …) happens to touch the
    /// registry FIRST permanently freezes its snapshot via `OnceCell`. A
    /// session created via `tm sessions new <url>` after that point was
    /// registered nowhere: `daemon::managed_routes::lifecycle`'s spawn paths
    /// never called an explicit register, so `tm project list` reported it
    /// missing forever — even though the daemon plainly knew about the
    /// session (stop/resume by explicit name still worked, since those go
    /// through the SEPARATE session-manager store, not the project
    /// registry). Call sites: `spawn_managed_cloned`, `spawn_managed_inproject`,
    /// `spawn_managed_local` (`daemon::managed_routes::lifecycle`), each
    /// right after `SessionManager::create_with_id`/`create_with_reserved_name`
    /// returns the freshly persisted record.
    /// What: reuses the exact same derivation
    /// [`Self::auto_register_from_sessions`] uses (`derive_name_from_url` +
    /// skip-if-already-registered via a freshly-fetched existing-name set) —
    /// the two entry points never drift on what "implicit registration"
    /// means. A no-op when `record.repo_url` is `None`, a name cannot be
    /// derived, or a project with that name is already registered
    /// (never overwrites a manually-configured entry).
    /// Test: `registry_register_from_session_registers_new_project`,
    /// `registry_register_from_session_skips_when_repo_url_missing`,
    /// `registry_register_from_session_does_not_overwrite_existing`.
    pub async fn register_from_session(&self, record: &SessionRecord) {
        let existing: std::collections::HashSet<String> = match self.list().await {
            Ok(projects) => projects.into_iter().map(|p| p.name).collect(),
            Err(e) => {
                warn!("register_from_session: failed to list existing projects: {e}");
                return;
            }
        };
        self.register_implicit_from_record(record, &existing, "register_from_session")
            .await;
    }

    /// Shared tail of [`Self::auto_register_from_sessions`] (looped) and
    /// [`Self::register_from_session`] (single-record): derive an implicit
    /// `Project` from `record`'s `repo_url` and upsert it unless a project
    /// with that name is already in `existing` (#3822 — extracted so the two
    /// entry points can never drift on what "implicit registration" means).
    ///
    /// `log_prefix` names the caller in the `warn!`/`info!`/`debug!` lines so
    /// a daemon log line still identifies which pass produced it.
    async fn register_implicit_from_record(
        &self,
        record: &SessionRecord,
        existing: &std::collections::HashSet<String>,
        log_prefix: &str,
    ) {
        let Some(repo_url) = &record.repo_url else {
            return;
        };
        let Some(name) = derive_name_from_url(repo_url) else {
            warn!(
                repo_url = %repo_url,
                "{log_prefix}: could not derive project name from URL; skipping"
            );
            return;
        };
        if existing.contains(&name) {
            debug!(name = %name, "{log_prefix}: project already registered; skipping");
            return;
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
            gh_account: None,
            // No source of a gh/commit identity binding for an implicitly
            // auto-registered project (#2184) — the operator declares
            // these explicitly via `config.projects`/`config_write`.
            github: None,
            commit_name: None,
            commit_email: None,
            // #3455: no declared preference; defaults to worktree-ON.
            worktree: None,
        };
        if let Err(e) = self.register(project).await {
            warn!(name = %name, "{log_prefix}: failed to register: {e}");
        } else {
            info!(name = %name, repo_url = %repo_url, "{log_prefix}: registered project from session history");
        }
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
