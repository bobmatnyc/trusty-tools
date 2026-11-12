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

    /// Insert `project` unless a record already covers it, under ONE lock.
    ///
    /// Why (#10586): `tm project init` must never overwrite or duplicate a
    /// record another writer owns. A `list` then a later `register` takes the
    /// lock twice, so a concurrent `tm projects register` or a session
    /// auto-registration landing between them was overwritten with defaults —
    /// the #2481 lost-update shape [`Self::update_with`] fixed for PATCH.
    /// What: derives the incoming `owner/repo` key outside the lock, then inside
    /// one [`ProjectStore::mutate`] critical section answers `Ok(Some(kept))`,
    /// a clone of the matching record, without writing when a record has the
    /// same name, the same `repo_url` string, or a `project_key` equal to that
    /// key (so `git@github.com:o/r.git` matches a record stored as
    /// `https://github.com/o/r`). A legacy record with no stored key is matched
    /// on the key [`origin_key`] derives from its `repo_url`. Otherwise it stores `project` with the key
    /// (or a fresh `local:<uuid>` one) and answers `Ok(None)`.
    /// Test: `register_unless_present_matches_on_project_key`,
    /// `register_unless_present_keeps_a_legacy_record_without_a_key`,
    /// `project_init_with_ssh_origin_keeps_the_https_record_10586`,
    /// `project_init_reports_the_kept_record_for_a_different_origin_10586`.
    pub async fn register_unless_present(
        &self,
        mut project: Project,
    ) -> Result<Option<Project>, ProjectStoreError> {
        let key = super::project_key::project_key_from_repo_url(&project.repo_url);
        let mut guard = self.store.write().await;
        guard
            .mutate(move |projects| {
                // #10586: hand the kept record back, so init can name it.
                // #10586: a legacy record stores no key; derive it, as #10696 does.
                let present = projects.values().find(|p| {
                    p.name == project.name
                        || p.repo_url == project.repo_url
                        || (key.is_some()
                            && p.project_key.clone().or_else(|| origin_key(&p.repo_url)) == key)
                });
                if let Some(kept) = present {
                    return Ok(Some(kept.clone()));
                }
                // No record holds `key` (checked above), so the key is unique.
                let key = key
                    .unwrap_or_else(|| super::project_key::assign_project_key(&project.repo_url));
                project.project_key = Some(key);
                projects.insert(project.name.clone(), project);
                Ok(None)
            })
            .await
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
                // #10530: the store assigns the key at first register and keeps it.
                project_key: None,
                decommissioned_at: None,
                archived_at: None,
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
        // Collect existing names and origins to avoid overwriting or duplicating
        // registered entries.
        let existing = match self.list().await {
            Ok(projects) => ExistingProjects::from_projects(&projects),
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
    /// registry). Call sites: `spawn_managed_inproject` and
    /// `spawn_managed_on_main` (`daemon::managed_routes`), each
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
        let existing = match self.list().await {
            Ok(projects) => ExistingProjects::from_projects(&projects),
            Err(e) => {
                warn!("register_from_session: failed to list existing projects: {e}");
                return;
            }
        };
        self.register_implicit_from_record(record, &existing, "register_from_session")
            .await;
    }

    /// Mark `name` decommissioned at `at` (#10988).
    ///
    /// Why: a decommissioned project keeps its row so implicit registration
    /// cannot re-list it; the row carries the state.
    /// What: one [`Self::update_with`] that stamps `decommissioned_at`.
    /// `NotFound` propagates without writing.
    /// Test: `decommission_project_stops_live_sessions_and_keeps_the_workspace`.
    pub async fn mark_decommissioned(
        &self,
        name: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Project, ProjectStoreError> {
        self.update_with(name, move |mut p| {
            p.decommissioned_at = Some(at);
            p
        })
        .await
    }

    /// Mark `name` archived at `at` (#10991).
    ///
    /// Why: owner ruling D8b — the TUI dims a project whose directory moved to
    /// the archive. What: one [`Self::update_with`] that stamps `archived_at`;
    /// `NotFound` propagates without writing.
    /// Test: `archive_session_moves_the_directory_and_marks_the_project`.
    pub async fn mark_archived(
        &self,
        name: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Project, ProjectStoreError> {
        self.update_with(name, move |mut p| {
            p.archived_at = Some(at);
            p
        })
        .await
    }

    /// Hold the registry's write lock until the guard drops (tests only): any
    /// writer, such as [`Self::mark_archived`], blocks meanwhile (#10991).
    #[cfg(test)]
    pub(crate) async fn hold_write_lock(&self) -> tokio::sync::OwnedRwLockWriteGuard<ProjectStore> {
        Arc::clone(&self.store).write_owned().await
    }

    /// Clear `decommissioned_at` and `archived_at` (#10991) on every project
    /// `record`'s `repo_url` binds to (#10988).
    ///
    /// Why: Bob's ruling — a project shows decommissioned until a new session
    /// runs. The boot pass (`auto_register_from_sessions`) never calls this: it
    /// replays old records, and no new session has run.
    /// What: matches with the fleet grouping's own rule
    /// ([`super::resolve_session_project`]) and returns the names it cleared.
    /// A list or update failure is returned, never logged and dropped: the
    /// caller (`spawn_managed`, the one place every spawn path returns
    /// through) reports it to its client.
    /// Test: `reactivate_for_session_clears_a_decommissioned_project`,
    /// `reactivate_for_session_returns_a_list_failure`,
    /// `auto_register_keeps_a_decommissioned_project_decommissioned`.
    pub async fn reactivate_for_session(
        &self,
        record: &SessionRecord,
    ) -> Result<Vec<String>, ProjectStoreError> {
        let projects = self.list().await?;
        let mut cleared = Vec::new();
        // #10991: a new session also lifts the archived state.
        for p in projects
            .iter()
            .filter(|p| p.is_decommissioned() || p.is_archived())
        {
            if super::resolve_session_project(record, std::slice::from_ref(p)).is_none() {
                continue;
            }
            self.update_with(&p.name, |mut p| {
                p.decommissioned_at = None;
                p.archived_at = None;
                p
            })
            .await?;
            info!(name = %p.name, "project reactivated by a new session (#10988)");
            cleared.push(p.name.clone());
        }
        Ok(cleared)
    }

    /// Shared tail of [`Self::auto_register_from_sessions`] (looped) and
    /// [`Self::register_from_session`] (single-record): derive an implicit
    /// `Project` from `record`'s `repo_url` and upsert it unless `existing`
    /// already holds that name or that origin (#3822 — extracted so the two
    /// entry points can never drift on what "implicit registration" means;
    /// #10696 added the origin match).
    ///
    /// `log_prefix` names the caller in the `warn!`/`info!`/`debug!` lines so
    /// a daemon log line still identifies which pass produced it. No line
    /// carries a URL's credentials (#9124).
    /// Test: `implicit_registration_never_logs_a_credentialed_url`,
    /// `implicit_registration_skips_a_keyed_record_with_the_same_origin`,
    /// `implicit_registration_skips_a_legacy_ssh_record_for_the_same_repo`,
    /// `implicit_registration_registers_a_different_origin`.
    async fn register_implicit_from_record(
        &self,
        record: &SessionRecord,
        existing: &ExistingProjects,
        log_prefix: &str,
    ) {
        let Some(repo_url) = &record.repo_url else {
            return;
        };
        // #9124: a remote URL may embed `user:token@`; every arm logs this.
        let shown = crate::core::remote_url_redact::redact_stored_url(repo_url);
        let Some(name) = derive_name_from_url(repo_url) else {
            warn!(
                repo_url = %shown,
                "{log_prefix}: could not derive project name from URL; skipping"
            );
            return;
        };
        if existing.names.contains(&name) {
            debug!(name = %name, "{log_prefix}: project already registered; skipping");
            return;
        }
        // #10696: a record under another name (e.g. a directory-named
        // `tm project init` record) may already own this origin.
        if existing.holds_origin(repo_url) {
            debug!(name = %name, repo_url = %shown, "{log_prefix}: origin already registered; skipping");
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
            project_key: None,
            decommissioned_at: None,
            archived_at: None,
        };
        if let Err(e) = self.register(project).await {
            warn!(name = %name, "{log_prefix}: failed to register: {e}");
        } else {
            info!(name = %name, repo_url = %shown, "{log_prefix}: registered project from session history");
        }
    }
}

/// Names and origins of the registered projects, for the implicit-registration skip.
///
/// Why: #10696 — matching on name alone let a session add a second record for
/// an origin a differently-named record already owns. A legacy record has no
/// stored `project_key`, so its origin must be derived from its `repo_url`.
/// What: `names` holds every record name. `origin_urls` holds every trimmed
/// `repo_url` verbatim. `origin_keys` holds each stored `project_key`, or for a
/// legacy record the key [`origin_key`] derives — the same `owner/repo` the
/// store mints, so an ssh and an https URL for one repo match.
/// Test: `implicit_registration_skips_a_keyed_record_with_the_same_origin`,
/// `implicit_registration_skips_a_legacy_ssh_record_for_the_same_repo`,
/// `implicit_registration_registers_a_different_origin`.
struct ExistingProjects {
    names: std::collections::HashSet<String>,
    origin_urls: std::collections::HashSet<String>,
    origin_keys: std::collections::HashSet<String>,
}

impl ExistingProjects {
    /// Index `projects` by name, verbatim origin URL and origin key.
    fn from_projects(projects: &[Project]) -> Self {
        let mut existing = Self {
            names: std::collections::HashSet::new(),
            origin_urls: std::collections::HashSet::new(),
            origin_keys: std::collections::HashSet::new(),
        };
        for p in projects {
            existing.names.insert(p.name.clone());
            existing.origin_urls.insert(p.repo_url.trim().to_string());
            // #10696: a legacy record carries no key; derive one from its URL.
            if let Some(key) = p.project_key.clone().or_else(|| origin_key(&p.repo_url)) {
                existing.origin_keys.insert(key);
            }
        }
        existing
    }

    /// Whether a registered record already owns `repo_url`'s origin.
    fn holds_origin(&self, repo_url: &str) -> bool {
        self.origin_urls.contains(repo_url.trim())
            || origin_key(repo_url).is_some_and(|key| self.origin_keys.contains(&key))
    }
}

/// The `owner/repo` key of a remote URL, or `None` for a checkout path.
///
/// Why: #10696 — this pass runs on every session spawn; a path-form `repo_url`
/// would make [`super::project_key_from_repo_url`] spawn `git` per record.
/// A path still matches verbatim through `origin_urls`.
/// What: `None` for an absolute path, else [`super::project_key_from_repo_url`].
/// Test: `implicit_registration_registers_a_different_origin`.
fn origin_key(repo_url: &str) -> Option<String> {
    if repo_url.trim().starts_with('/') {
        return None;
    }
    super::project_key_from_repo_url(repo_url)
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
