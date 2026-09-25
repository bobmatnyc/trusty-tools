//! On-disk OAuth token storage compatible with the Python CLI.
//!
//! Why: We want to share `~/.gworkspace-mcp/tokens.json` between the Python
//! CLI (which performs the interactive OAuth flow) and this Rust MCP server.
//! What: Reads/writes a `HashMap<profile_name, StoredToken>` JSON object.
//! Two-tier lookup: project-level `./.gworkspace-mcp/tokens.json` and
//! `~/.gworkspace-mcp/tokens.json`. When both hold a profile, the newer or
//! wider-scoped entry wins (see [`precedence::resolve`], #8539) and `load()`
//! warns once naming the winner and why. [`TokenStorage::update`] guards
//! every read-modify-write call site (refresh, consent persist, CLI
//! `accounts default`/`accounts remove`) against concurrent writers losing
//! each other's changes (issue #3502), and writes each changed entry back to
//! the store it was read from (#8539).
//! Test: `tests.rs` beside this file; `precedence.rs` for the rule itself.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use tracing::warn;

use super::models::StoredToken;
use crate::api::constants::DEFAULT_PROFILE;
use precedence::{Resolution, Store, issued_at, resolve};

mod precedence;

/// Token file storage with two-tier lookup (project and user).
///
/// Why: Matches Python `TokenStorage` semantics — a project-level store can
/// override user-level per directory, while user-level is the durable
/// fallback.
/// What: Holds the user-level path (always `~/.gworkspace-mcp/tokens.json`)
/// and an optional project-level path (`./.gworkspace-mcp/tokens.json`), plus
/// a `warned_shadows` set throttling the shadow warning in
/// [`TokenStorage::load`] to once per profile and winner per process.
/// Test: `tests.rs` beside this file.
#[derive(Debug, Clone)]
pub struct TokenStorage {
    user_path: PathBuf,
    project_path: Option<PathBuf>,
    warned_shadows: Arc<Mutex<HashSet<String>>>,
    /// In-process mutex serialising [`TokenStorage::update`] calls across
    /// every clone of this `TokenStorage` within the current process.
    ///
    /// Why: The cross-process file lock alone still lets two threads in the
    /// *same* process interleave a load-mutate-save cycle between the point
    /// one thread releases the lock and the next acquires it, if they are
    /// not also serialised in-process; a plain `Mutex` closes that window
    /// cheaply without relying on the file lock's fairness semantics.
    /// What: Held for the full duration of `update`'s critical section.
    /// Test: `concurrent_updates_do_not_lose_writes`.
    write_guard: Arc<Mutex<()>>,
}

impl TokenStorage {
    /// Construct with default paths.
    ///
    /// Why: Default location matches the Python CLI so a user who ran
    /// `gworkspace-mcp setup` once works across both implementations.
    /// What: User path resolves via `dirs::home_dir`, project path is
    /// `./.gworkspace-mcp/tokens.json` if the directory exists.
    /// Test: covered by integration tests.
    pub fn new() -> Self {
        let user_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".gworkspace-mcp")
            .join("tokens.json");
        let project_candidate = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".gworkspace-mcp")
            .join("tokens.json");
        let project_path = if project_candidate.exists() {
            Some(project_candidate)
        } else {
            None
        };
        Self {
            user_path,
            project_path,
            warned_shadows: Arc::new(Mutex::new(HashSet::new())),
            write_guard: Arc::new(Mutex::new(())),
        }
    }

    /// Construct with an explicit path (test helper).
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            user_path: path,
            project_path: None,
            warned_shadows: Arc::new(Mutex::new(HashSet::new())),
            write_guard: Arc::new(Mutex::new(())),
        }
    }

    fn load_from(path: &PathBuf) -> Result<HashMap<String, StoredToken>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("read tokens file {}", path.display()))?;
        let map: HashMap<String, StoredToken> = serde_json::from_str(&data)
            .with_context(|| format!("parse tokens JSON {}", path.display()))?;
        Ok(map)
    }

    /// Load merged tokens: one entry per profile, across both stores.
    ///
    /// Why: A project entry used to win unconditionally, so one minted before
    /// a re-consent silently shadowed the fresh, wider user-level token and
    /// Gmail filter writes got 403 (#8539). `load` sits on the per-request hot
    /// path (`BaseClient::get_access_token` reaches it via
    /// `get_profile`/`get_default` on every MCP tool call), so the shadow
    /// warning is throttled (PR #2949 review).
    /// What: Returns the merged view built by [`TokenStorage::load_tiers`]:
    /// a profile in one store resolves to that entry; a profile in both
    /// resolves per [`precedence::resolve`] (different account → project;
    /// strict scope superset; later issue/refresh time; tie → project).
    /// Test: `project_entry_lacking_scope_no_longer_shadows_fresh_user_entry`,
    /// `load_warns_once_naming_winner_without_token_values`.
    pub fn load(&self) -> Result<HashMap<String, StoredToken>> {
        Ok(self.load_tiers().merged)
    }

    /// Read both stores and resolve each profile to one entry.
    ///
    /// Why: [`TokenStorage::update`] must know which store each merged entry
    /// came from to write it back there (#8539); `load` needs only the merge.
    /// What: Reads each store (an unreadable or unparsable file counts as
    /// empty, as before). For a profile in both stores whose `token` fields
    /// differ, picks the winner with [`precedence::resolve`] and warns once;
    /// identical tokens resolve to the project entry silently.
    /// Test: `project_entry_lacking_scope_no_longer_shadows_fresh_user_entry`.
    fn load_tiers(&self) -> Tiers {
        let user = Self::load_from(&self.user_path).unwrap_or_default();
        let project = match &self.project_path {
            Some(path) => Self::load_from(path).unwrap_or_default(),
            None => HashMap::new(),
        };
        let mut merged = HashMap::with_capacity(user.len() + project.len());
        let mut origin = HashMap::with_capacity(user.len() + project.len());
        for (profile, entry) in &project {
            origin.insert(profile.clone(), Store::Project);
            merged.insert(profile.clone(), entry.clone());
        }
        for (profile, u) in &user {
            let winner = match project.get(profile) {
                None => Store::User,
                // #8539: identical credentials are not a shadow; keep project.
                Some(p) if p.token == u.token => Store::Project,
                // #8539: newer or wider wins, not project-always-wins.
                Some(p) => {
                    let resolution = resolve(p, u);
                    self.warn_shadow_once(profile, &resolution, p, u);
                    resolution.winner
                }
            };
            if winner == Store::User {
                origin.insert(profile.clone(), Store::User);
                merged.insert(profile.clone(), u.clone());
            }
        }
        Tiers {
            user,
            project,
            merged,
            origin,
        }
    }

    /// Warn that `profile` has differing entries in both stores, once per
    /// profile and winning store for the lifetime of this `TokenStorage` and
    /// its clones (`warned_shadows` is `Arc`-shared).
    ///
    /// Why: A shadow is silent otherwise (#8539); the old warning fired only
    /// when the project entry had expired. Throttled because `load` runs on
    /// every MCP tool call (PR #2949 review).
    /// What: Logs one structured `warn!` naming the profile, the winning
    /// store, the reason, both paths, and both issue and expiry times. It
    /// never logs a token value. A poisoned mutex recovers via `into_inner`:
    /// a missed dedup entry costs one extra warning.
    /// Test: `load_warns_once_naming_winner_without_token_values`.
    fn warn_shadow_once(
        &self,
        profile: &str,
        resolution: &Resolution,
        project: &StoredToken,
        user: &StoredToken,
    ) {
        let key = format!("{profile}\u{0}{}", resolution.winner);
        let mut warned = self
            .warned_shadows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !warned.insert(key) {
            return; // already warned for this profile and winner
        }
        drop(warned);
        let project_path = self
            .project_path
            .as_deref()
            .map(Path::display)
            .map(|d| d.to_string())
            .unwrap_or_default();
        warn!(
            profile = %profile,
            winner = %resolution.winner,
            reason = %resolution.reason,
            project_path = %project_path,
            project_issued_at = %issued_at(project),
            project_expires_at = %project.token.expires_at,
            user_path = %self.user_path.display(),
            user_issued_at = %issued_at(user),
            user_expires_at = %user.token.expires_at,
            "project-level and user-level token stores hold different tokens for this \
             profile; serving the winner's entry. Remove the losing entry to silence this. \
             (Fires once per profile and winner per process.)"
        );
    }

    /// Save tokens to the primary write path (project if known, else user).
    ///
    /// Why: `tokens.json` holds live OAuth refresh tokens — on Unix we
    /// restrict it to owner-only (0600) so other local users/processes can't
    /// read it off disk. This crate is now a primary minting path (not just
    /// a reader of Python-written files), so it must not regress that.
    /// What: Writes `tokens` whole to one file via [`Self::write_store`]. Every
    /// production mutation goes through [`Self::update`], which routes each
    /// entry to its own store instead (#8539).
    /// Test: `save_restricts_permissions_on_unix` (cfg(unix)).
    pub fn save(&self, tokens: &HashMap<String, StoredToken>) -> Result<()> {
        let target = self
            .project_path
            .clone()
            .unwrap_or_else(|| self.user_path.clone());
        Self::write_store(&target, tokens)
    }

    /// Write one store file: pretty JSON, then (Unix) mode 0600.
    fn write_store(target: &Path, tokens: &HashMap<String, StoredToken>) -> Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(tokens)?;
        std::fs::write(target, data)
            .with_context(|| format!("write tokens to {}", target.display()))?;
        Self::restrict_permissions(target)
    }

    /// Restrict the token file to owner read/write only (Unix: mode 0600).
    ///
    /// Why: Isolated so the mode-setting logic is a single, obviously-correct
    /// spot rather than inlined in `save`, and so non-Unix targets get a
    /// trivial no-op instead of a compile error.
    /// What: `chmod 0600` on Unix; no-op elsewhere (Windows ACLs already
    /// default to the owning user for files under the user profile).
    /// Test: `save_restricts_permissions_on_unix`.
    #[cfg(unix)]
    fn restrict_permissions(path: &std::path::Path) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", path.display()))
    }

    #[cfg(not(unix))]
    fn restrict_permissions(_path: &std::path::Path) -> Result<()> {
        Ok(())
    }

    /// Open the sidecar `<store>.lock` file guarding `store` — it never
    /// contains token data itself.
    fn open_lock(store: &Path) -> Result<fd_lock::RwLock<std::fs::File>> {
        let file_name = store
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tokens.json".to_string());
        let lock_path = store.with_file_name(format!("{file_name}.lock"));
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open lock file {}", lock_path.display()))?;
        Ok(fd_lock::RwLock::new(file))
    }

    /// Perform an atomic read-modify-write on the stored token map.
    ///
    /// Why: `OAuthManager::refresh` and the consent flow's `persist` (and the
    /// CLI's `accounts default`/`accounts remove`) each used to do
    /// `load()` -> mutate -> `save()` with no synchronization, losing
    /// whichever concurrent write lost the race (issue #3502). Saving the
    /// whole merged view to the project file also copied user-level entries
    /// into it, and a refresh of a user-level winner landed in the project
    /// store, leaving the user store stale (#8539).
    /// What: Takes an in-process mutex, then an advisory exclusive lock on
    /// each store's sidecar `<path>.lock` — user first, then project, a fixed
    /// order that cannot deadlock. Reloads both stores under the locks,
    /// applies `f` to the merged view, and routes the result with
    /// [`route_writes`]: an unchanged entry is never copied across stores, a
    /// changed entry goes back to the store it was read from, a new profile
    /// goes to the project store if one exists (else user), and a removed
    /// profile leaves both stores. Only a store whose content changed is
    /// rewritten. Locks release on return, including on error.
    /// Test: `concurrent_updates_do_not_lose_writes`,
    /// `refresh_write_back_targets_the_winning_store`,
    /// `remove_profile_clears_both_stores`.
    pub fn update<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut HashMap<String, StoredToken>) -> Result<T>,
    {
        let _in_process_guard = self
            .write_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // #8539: writes can reach both stores, so both are locked.
        let mut user_lock = Self::open_lock(&self.user_path)?;
        let _user_guard = user_lock
            .write()
            .with_context(|| format!("lock {}", self.user_path.display()))?;
        let mut project_lock = self
            .project_path
            .as_deref()
            .map(Self::open_lock)
            .transpose()?;
        let _project_guard = match project_lock.as_mut() {
            Some(lock) => Some(lock.write().context("lock project-level tokens")?),
            None => None,
        };

        let tiers = self.load_tiers();
        let mut merged = tiers.merged.clone();
        let result = f(&mut merged)?;
        let new_profiles = if self.project_path.is_some() {
            Store::Project
        } else {
            Store::User
        };
        let (user, project) = route_writes(&tiers, &merged, new_profiles);
        if user != tiers.user {
            Self::write_store(&self.user_path, &user)?;
        }
        if let Some(path) = &self.project_path
            && project != tiers.project
        {
            Self::write_store(path, &project)?;
        }
        Ok(result)
    }

    /// Return the default profile token (is_default=true), or the first one,
    /// or the entry matching `DEFAULT_PROFILE`, else None.
    pub fn get_default(&self) -> Result<Option<StoredToken>> {
        let tokens = self.load()?;
        if tokens.is_empty() {
            return Ok(None);
        }
        if let Some((_k, v)) = tokens.iter().find(|(_, v)| v.metadata.is_default) {
            return Ok(Some(v.clone()));
        }
        if let Some(v) = tokens.get(DEFAULT_PROFILE) {
            return Ok(Some(v.clone()));
        }
        if tokens.len() == 1 {
            return Ok(tokens.into_values().next().map(Some).unwrap_or(None));
        }
        Ok(None)
    }

    /// Return the named profile, if it exists.
    pub fn get_profile(&self, name: &str) -> Result<Option<StoredToken>> {
        Ok(self.load()?.get(name).cloned())
    }

    /// List all profiles as `(name, email, is_default)` tuples.
    pub fn list_accounts(&self) -> Result<Vec<(String, Option<String>, bool)>> {
        let tokens = self.load()?;
        let mut out: Vec<(String, Option<String>, bool)> = tokens
            .into_iter()
            .map(|(name, t)| (name, t.metadata.email, t.metadata.is_default))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Mark `name` as the default profile, clearing any other default.
    ///
    /// Why: Shared by the CLI (`accounts default`) and the `set_default_account`
    /// MCP tool so both surfaces get the identical lock-guarded mutation
    /// instead of each re-implementing load-mutate-save.
    /// What: Errors if `name` is absent; otherwise unsets `is_default` on
    /// every entry, sets it on `name`, and saves — all under [`Self::update`].
    /// Test: `set_default_moves_flag`, `set_default_rejects_unknown` (via the
    /// `cli::accounts` wrapper, which this backs).
    pub fn set_default_profile(&self, name: &str) -> Result<()> {
        self.update(|all| {
            if !all.contains_key(name) {
                return Err(anyhow!("no profile named '{name}'"));
            }
            for entry in all.values_mut() {
                entry.metadata.is_default = false;
            }
            if let Some(entry) = all.get_mut(name) {
                entry.metadata.is_default = true;
            }
            Ok(())
        })
    }

    /// Remove `name`, reassigning the default if it was the one removed.
    ///
    /// Why: Shared by the CLI (`accounts remove`) and the `remove_account` MCP
    /// tool. Removing the current default used to leave zero default entries,
    /// silently breaking `BaseClient::resolve_stored`'s default-profile
    /// fallback for every subsequent call with no explicit `account` (issue
    /// #3502).
    /// What: Errors if `name` is absent; otherwise removes it and, only when
    /// it had `is_default = true` and other profiles remain, marks the
    /// alphabetically-first remaining profile name as the new default. Saves
    /// under [`Self::update`]. Returns [`RemoveOutcome`] naming the removed
    /// profile and the reassigned default, if any.
    /// Test: `remove_deletes_profile` (via `cli::accounts`),
    /// `remove_default_reassigns_to_next_profile`,
    /// `remove_default_leaves_none_when_last_profile`,
    /// `remove_non_default_does_not_reassign`.
    pub fn remove_profile(&self, name: &str) -> Result<RemoveOutcome> {
        self.update(|all| remove_and_reassign_default(all, name))
    }
}

/// Outcome of [`TokenStorage::remove_profile`]: which profile was removed,
/// and — only when it had been the default — which remaining profile
/// inherited that role.
///
/// Why: Both the CLI and the `remove_account` MCP tool need to report the
/// reassignment (if any) to the user/caller rather than silently swapping
/// which account subsequent default-scoped calls act against.
/// What: `reassigned_default` is `None` when the removed profile was not the
/// default, or when it was but no profiles remain.
/// Test: see [`TokenStorage::remove_profile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveOutcome {
    pub removed: String,
    pub reassigned_default: Option<String>,
}

/// Pure mutation: remove `name` from `all`, reassigning the default if it was
/// the one removed.
///
/// Why: Isolated as a pure function (no I/O, no locking) so the exact
/// reassignment rule is directly unit-testable against a plain `HashMap`,
/// mirroring the `precedence::resolve` / `should_set_default` pattern
/// elsewhere in this crate.
/// What: Errors if `name` is absent. Otherwise removes it; if it was
/// `is_default` and `all` is non-empty afterward, marks the
/// alphabetically-first remaining key `is_default = true` and returns it as
/// `reassigned_default`.
/// Test: `remove_default_reassigns_to_next_profile`,
/// `remove_default_leaves_none_when_last_profile`,
/// `remove_non_default_does_not_reassign`.
fn remove_and_reassign_default(
    all: &mut HashMap<String, StoredToken>,
    name: &str,
) -> Result<RemoveOutcome> {
    let removed = all
        .remove(name)
        .ok_or_else(|| anyhow!("no profile named '{name}'"))?;

    let mut reassigned_default = None;
    if removed.metadata.is_default {
        let mut remaining: Vec<&String> = all.keys().collect();
        remaining.sort();
        if let Some(next) = remaining.first().map(|s| s.to_string()) {
            if let Some(entry) = all.get_mut(&next) {
                entry.metadata.is_default = true;
            }
            reassigned_default = Some(next);
        }
    }

    Ok(RemoveOutcome {
        removed: name.to_string(),
        reassigned_default,
    })
}

/// Both stores as read from disk, plus the merged view `load` serves and the
/// store each merged entry came from.
struct Tiers {
    user: HashMap<String, StoredToken>,
    project: HashMap<String, StoredToken>,
    merged: HashMap<String, StoredToken>,
    origin: HashMap<String, Store>,
}

/// Split an updated merged view back into the two stores.
///
/// Why: Writing the merged view to one file copied the other store's entries
/// into it and sent a user-level winner's refresh to the project store
/// (#8539). A pure function keeps the routing rule testable in isolation.
/// What: Starts from each store's on-disk content. A profile dropped from
/// the merged view is removed from both stores, so its losing entry cannot
/// resurface. An entry equal to its pre-update value is left where it is. A
/// changed entry goes to the store it was read from; a profile new to both
/// stores goes to `new_profiles`. Returns `(user, project)`.
/// Test: `refresh_write_back_targets_the_winning_store`,
/// `remove_profile_clears_both_stores`.
fn route_writes(
    tiers: &Tiers,
    merged: &HashMap<String, StoredToken>,
    new_profiles: Store,
) -> (HashMap<String, StoredToken>, HashMap<String, StoredToken>) {
    let mut user = tiers.user.clone();
    let mut project = tiers.project.clone();
    for profile in tiers.merged.keys() {
        if !merged.contains_key(profile) {
            user.remove(profile);
            project.remove(profile);
        }
    }
    for (profile, entry) in merged {
        if tiers.merged.get(profile) == Some(entry) {
            continue;
        }
        let target = match tiers.origin.get(profile).copied().unwrap_or(new_profiles) {
            Store::User => &mut user,
            Store::Project => &mut project,
        };
        target.insert(profile.clone(), entry.clone());
    }
    (user, project)
}

impl Default for TokenStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
