//! On-disk OAuth token storage compatible with the Python CLI.
//!
//! Why: We want to share `~/.gworkspace-mcp/tokens.json` between the Python
//! CLI (which performs the interactive OAuth flow) and this Rust MCP server.
//! What: Reads/writes a `HashMap<profile_name, StoredToken>` JSON object.
//! Two-tier lookup: project-level `./.gworkspace-mcp/tokens.json` and
//! `~/.gworkspace-mcp/tokens.json`. When both hold a profile, the entry is
//! chosen by [`precedence::resolve`] and `load()` warns once naming the
//! winner and why (#8539). [`TokenStorage::update`] guards every
//! read-modify-write call site (refresh, consent persist, CLI
//! `accounts default`/`accounts remove`) against concurrent writers losing
//! each other's changes (issue #3502), routes each change to the right store,
//! and refuses to write when a store cannot be read (#8539).
//! Test: `tests.rs` beside this file; `precedence.rs` for the rule itself.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use tracing::warn;

use super::models::StoredToken;
use crate::api::constants::DEFAULT_PROFILE;
use precedence::{Resolution, Store, resolve, same_account};

mod files;
mod precedence;

/// Token file storage with two-tier lookup (project and user).
///
/// Why: Matches Python `TokenStorage` semantics — a project-level store can
/// override user-level per directory, while user-level is the durable
/// fallback.
/// What: Holds the user-level path (always `~/.gworkspace-mcp/tokens.json`)
/// and an optional project-level path (`./.gworkspace-mcp/tokens.json`), plus
/// a `warned_once` set throttling [`TokenStorage::load`]'s shadow and
/// unreadable-store warnings to once per key per process.
/// Test: `tests.rs` beside this file.
#[derive(Debug, Clone)]
pub struct TokenStorage {
    user_path: PathBuf,
    project_path: Option<PathBuf>,
    warned_once: Arc<Mutex<HashSet<String>>>,
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
    /// `./.gworkspace-mcp/tokens.json` if it exists and is not the user file
    /// itself (cwd = `$HOME`, or a symlinked directory).
    /// Test: `same_store_sees_through_a_symlinked_dir`.
    pub fn new() -> Self {
        let user_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".gworkspace-mcp")
            .join("tokens.json");
        let project_candidate = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".gworkspace-mcp")
            .join("tokens.json");
        // #8539: one file seen through two paths is one store, not two.
        let project_path =
            Some(project_candidate).filter(|p| p.exists() && !files::same_store(p, &user_path));
        Self::with_paths(user_path, project_path)
    }

    /// Construct with an explicit path (test helper).
    pub fn with_path(path: PathBuf) -> Self {
        Self::with_paths(path, None)
    }

    /// Construct with explicit user and optional project paths.
    pub(crate) fn with_paths(user_path: PathBuf, project_path: Option<PathBuf>) -> Self {
        Self {
            user_path,
            project_path,
            warned_once: Arc::new(Mutex::new(HashSet::new())),
            write_guard: Arc::new(Mutex::new(())),
        }
    }

    /// The project store, unless it is the user file under another path.
    fn project_store(&self) -> Option<&Path> {
        // #8539: locking one file twice self-deadlocks; treat it as one store.
        self.project_path
            .as_deref()
            .filter(|p| !files::same_store(p, &self.user_path))
    }

    /// Load merged tokens: one entry per profile, across both stores.
    ///
    /// Why: A project entry used to win unconditionally, so one minted before
    /// a re-consent silently shadowed the fresh, wider user-level token and
    /// Gmail filter writes got 403 (#8539). `load` sits on the per-request hot
    /// path (`BaseClient::get_access_token` reaches it via
    /// `get_profile`/`get_default` on every MCP tool call), so its warnings
    /// are throttled (PR #2949 review).
    /// What: Returns the merged view built by [`TokenStorage::load_tiers`]:
    /// a profile in one store resolves to that entry; a profile in both
    /// resolves per [`precedence::resolve`]. A store that cannot be read is
    /// served as empty, with one warning per path.
    /// Test: `project_entry_lacking_scope_no_longer_shadows_fresh_user_entry`,
    /// `load_warns_once_naming_winner_without_token_values`,
    /// `load_warns_on_unparsable_store_without_echoing_it`.
    pub fn load(&self) -> Result<HashMap<String, StoredToken>> {
        Ok(self.load_tiers(false)?.merged)
    }

    /// Read both stores and resolve each profile to one entry.
    ///
    /// Why: [`TokenStorage::update`] must know which store each merged entry
    /// came from to write it back there, and must not write over a store it
    /// could not read (#8539); `load` needs only the merge.
    /// What: Reads each store. With `strict`, a read or parse failure is
    /// returned as an error; otherwise it warns once for that path and counts
    /// the store as empty. For a profile in both stores whose `token` fields
    /// differ, picks the winner with [`precedence::resolve`] and warns once;
    /// identical tokens resolve to the project entry silently.
    /// Test: `project_entry_lacking_scope_no_longer_shadows_fresh_user_entry`,
    /// `update_refuses_to_overwrite_an_unparsable_store`.
    fn load_tiers(&self, strict: bool) -> Result<Tiers> {
        let user = self.read_tier(&self.user_path, strict)?;
        let project = match self.project_store() {
            Some(path) => self.read_tier(path, strict)?,
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
        Ok(Tiers {
            user,
            project,
            merged,
            origin,
        })
    }

    /// Read one store; see [`TokenStorage::load_tiers`] for `strict`.
    fn read_tier(&self, path: &Path, strict: bool) -> Result<HashMap<String, StoredToken>> {
        match files::read_store(path) {
            Ok(map) => Ok(map),
            // #8539: a write must never overwrite a store it could not read.
            Err(e) if strict => Err(e.into()),
            Err(e) => {
                if self.first_time(format!("unreadable\u{0}{}", e.path().display())) {
                    // `e` carries the path and serde's position only, never file bytes.
                    warn!(error = %e, "token store unreadable; serving it as empty");
                }
                Ok(HashMap::new())
            }
        }
    }

    /// True the first time `key` is seen by this `TokenStorage` or a clone.
    /// A poisoned mutex recovers via `into_inner`: a missed dedup entry
    /// costs one extra warning.
    fn first_time(&self, key: String) -> bool {
        self.warned_once
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key)
    }

    /// Warn that `profile` has differing entries in both stores, once per
    /// profile and winning store.
    ///
    /// Why: A shadow is silent otherwise (#8539); the old warning fired only
    /// when the project entry had expired. Throttled because `load` runs on
    /// every MCP tool call (PR #2949 review).
    /// What: Logs one structured `warn!` naming the profile, the winning
    /// store, the reason, both paths, and both consent, refresh and expiry
    /// times. It never logs a token value.
    /// Test: `load_warns_once_naming_winner_without_token_values`.
    fn warn_shadow_once(
        &self,
        profile: &str,
        resolution: &Resolution,
        project: &StoredToken,
        user: &StoredToken,
    ) {
        if !self.first_time(format!("{profile}\u{0}{}", resolution.winner)) {
            return;
        }
        let project_path = self
            .project_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        warn!(
            profile = %profile,
            winner = %resolution.winner,
            reason = %resolution.reason,
            project_path = %project_path,
            project_created_at = %project.metadata.created_at,
            project_last_refreshed = ?project.metadata.last_refreshed,
            project_expires_at = %project.token.expires_at,
            user_path = %self.user_path.display(),
            user_created_at = %user.metadata.created_at,
            user_last_refreshed = ?user.metadata.last_refreshed,
            user_expires_at = %user.token.expires_at,
            "project-level and user-level token stores hold different tokens for this \
             profile; serving the winner's entry. Remove the losing entry to silence this. \
             (Fires once per profile and winner per process.)"
        );
    }

    /// Save tokens to the primary write path (project if known, else user).
    ///
    /// Why: `tokens.json` holds live OAuth refresh tokens, so the file is
    /// owner-only (0600) on Unix.
    /// What: Writes `tokens` whole to one file. Every production mutation
    /// goes through [`Self::update`], which routes each entry to its own
    /// store instead (#8539).
    /// Test: `save_restricts_permissions_on_unix` (cfg(unix)).
    pub fn save(&self, tokens: &HashMap<String, StoredToken>) -> Result<()> {
        let target = self.project_store().unwrap_or(&self.user_path);
        files::write_store(target, tokens)
    }

    /// Perform an atomic read-modify-write on the stored token map.
    ///
    /// Why: `OAuthManager::refresh` and the CLI's `accounts default`/`accounts
    /// remove` each used to do `load()` -> mutate -> `save()` with no
    /// synchronization, losing whichever concurrent write lost the race
    /// (issue #3502). Saving the whole merged view to the project file also
    /// copied user-level entries into it and sent a user-level winner's
    /// refresh to the project store (#8539).
    /// What: Takes an in-process mutex, then an advisory exclusive lock on
    /// each store's sidecar `<path>.lock` — user first, then project, a fixed
    /// order that cannot deadlock; a project path naming the user file is
    /// locked once. Reloads both stores under the locks and fails, writing
    /// nothing, if either cannot be read. Applies `f` to the merged view and
    /// writes back per [`route_writes`]. Only a store whose content changed
    /// is rewritten. Locks release on return, including on error.
    /// Test: `concurrent_updates_do_not_lose_writes`,
    /// `refresh_write_back_targets_the_winning_store`,
    /// `update_does_not_deadlock_when_project_and_user_are_the_same_file`,
    /// `update_refuses_to_overwrite_an_unparsable_store`.
    pub fn update<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut HashMap<String, StoredToken>) -> Result<T>,
    {
        self.update_routed(None, f)
    }

    /// [`Self::update`] for a newly consented credential for `profile`.
    ///
    /// Why: A consent may be for a different account than the stored entry.
    /// Routing it like a refresh would overwrite the user-level credential
    /// from inside a project directory (#8539).
    /// What: As [`Self::update`], except `profile`'s entry is written to the
    /// project store when one exists, else to the user store.
    /// Test: `persist_in_project_dir_does_not_overwrite_user_credential`.
    pub fn update_consent<F, T>(&self, profile: &str, f: F) -> Result<T>
    where
        F: FnOnce(&mut HashMap<String, StoredToken>) -> Result<T>,
    {
        self.update_routed(Some(profile), f)
    }

    fn update_routed<F, T>(&self, consent: Option<&str>, f: F) -> Result<T>
    where
        F: FnOnce(&mut HashMap<String, StoredToken>) -> Result<T>,
    {
        let _in_process_guard = self
            .write_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // #8539: writes can reach both stores, so both are locked.
        let mut user_lock = files::open_lock(&self.user_path)?;
        let _user_guard = user_lock
            .write()
            .with_context(|| format!("lock {}", self.user_path.display()))?;
        let project_path = self.project_store();
        let mut project_lock = project_path.map(files::open_lock).transpose()?;
        let _project_guard = match project_lock.as_mut() {
            Some(lock) => Some(lock.write().context("lock project-level tokens")?),
            None => None,
        };

        let tiers = self.load_tiers(true)?;
        let mut merged = tiers.merged.clone();
        let result = f(&mut merged)?;
        let new_profiles = if project_path.is_some() {
            Store::Project
        } else {
            Store::User
        };
        let (user, project) = route_writes(&tiers, &merged, new_profiles, consent);
        if user != tiers.user {
            files::write_store(&self.user_path, &user)?;
        }
        if let Some(path) = project_path
            && project != tiers.project
        {
            files::write_store(path, &project)?;
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
    /// under [`Self::update`], which deletes a losing entry in the other
    /// store only when it names the same account. Returns [`RemoveOutcome`]
    /// naming the removed profile, the reassigned default, and whether a
    /// user-level entry for `name` remains (#8539).
    /// Test: `remove_deletes_profile` (via `cli::accounts`),
    /// `remove_default_reassigns_to_next_profile`,
    /// `remove_default_leaves_none_when_last_profile`,
    /// `remove_non_default_does_not_reassign`,
    /// `remove_profile_keeps_a_different_account_user_entry`.
    pub fn remove_profile(&self, name: &str) -> Result<RemoveOutcome> {
        let mut outcome = self.update(|all| remove_and_reassign_default(all, name))?;
        // #8539: a different- or unknown-account user entry is kept; say so.
        outcome.user_entry_remains = self.project_store().is_some()
            && files::read_store(&self.user_path).is_ok_and(|m| m.contains_key(name));
        Ok(outcome)
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
/// default, or when it was but no profiles remain. `user_entry_remains` is
/// true when a project-level entry was removed but a user-level entry for a
/// different or unrecorded account was kept, and now serves the profile.
/// Test: see [`TokenStorage::remove_profile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveOutcome {
    pub removed: String,
    pub reassigned_default: Option<String>,
    pub user_entry_remains: bool,
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
        user_entry_remains: false,
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
/// (#8539). A pure function keeps the routing rule in one place.
/// What: Starts from each store's on-disk content, then:
///
/// - A profile dropped from the merged view is removed from the store that
///   served it. The other store's entry is removed too only when it names the
///   same account ([`same_account`]); a different or unrecorded account is
///   kept, so a removal never destroys another account's credential.
/// - An entry equal to its pre-update value is left where it is, never copied
///   into the other store.
/// - The `consent` profile's entry goes to `new_profiles` (project if one
///   exists, else user): a new consent may be a different account.
/// - Any other changed entry goes back to the store it was read from; a
///   profile new to both stores goes to `new_profiles`.
///
/// Refresh races are settled by the caller under the lock, not here: see
/// `OAuthManager::refresh`. Returns `(user, project)`.
/// Test: `refresh_write_back_targets_the_winning_store`,
/// `remove_profile_clears_both_stores`,
/// `remove_profile_keeps_a_different_account_user_entry`,
/// `persist_in_project_dir_does_not_overwrite_user_credential`.
fn route_writes(
    tiers: &Tiers,
    merged: &HashMap<String, StoredToken>,
    new_profiles: Store,
    consent: Option<&str>,
) -> (HashMap<String, StoredToken>, HashMap<String, StoredToken>) {
    let mut user = tiers.user.clone();
    let mut project = tiers.project.clone();
    for (profile, old) in &tiers.merged {
        if merged.contains_key(profile) {
            continue;
        }
        let (served, other) = match tiers.origin.get(profile) {
            Some(Store::User) => (&mut user, &mut project),
            _ => (&mut project, &mut user),
        };
        served.remove(profile);
        // #8539: never delete another account's credential with this one.
        if other.get(profile).is_some_and(|o| same_account(o, old)) {
            other.remove(profile);
        }
    }
    for (profile, entry) in merged {
        if tiers.merged.get(profile) == Some(entry) {
            continue;
        }
        // #8539: a consent is routed as new, a refresh back to its origin.
        let target = if consent == Some(profile.as_str()) {
            new_profiles
        } else {
            tiers.origin.get(profile).copied().unwrap_or(new_profiles)
        };
        match target {
            Store::User => user.insert(profile.clone(), entry.clone()),
            Store::Project => project.insert(profile.clone(), entry.clone()),
        };
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
