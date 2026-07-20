//! On-disk OAuth token storage compatible with the Python CLI.
//!
//! Why: We want to share `~/.gworkspace-mcp/tokens.json` between the Python
//! CLI (which performs the interactive OAuth flow) and this Rust MCP server.
//! What: Reads/writes a `HashMap<profile_name, StoredToken>` JSON object.
//! Two-tier lookup: project-level `./.gworkspace-mcp/tokens.json` first,
//! then `~/.gworkspace-mcp/tokens.json`. `load()` warns loudly (without
//! changing the override contract) when a project-level entry shadowing a
//! profile is expired while the user-level entry it hides is still valid
//! (issue #2946 — a stale worktree-local override otherwise silently wins
//! and re-poisons `save()` on every re-auth). [`TokenStorage::update`] guards
//! every read-modify-write call site (refresh, consent persist, CLI
//! `accounts default`/`accounts remove`) against concurrent writers losing
//! each other's changes (issue #3502).
//! Test: see integration test `tests/auth_models.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use tracing::warn;

use super::models::StoredToken;
use crate::api::constants::DEFAULT_PROFILE;

/// Token file storage with two-tier lookup (project then user).
///
/// Why: Matches Python `TokenStorage` semantics — project-level overrides
/// user-level, while user-level is the durable fallback.
/// What: Holds the user-level path (always `~/.gworkspace-mcp/tokens.json`)
/// and an optional project-level path (`./.gworkspace-mcp/tokens.json`), plus
/// a `warned_stale_shadows` set throttling the stale-shadow warning in
/// [`TokenStorage::load`] to once per profile per process (see its doc).
/// Test: integration test reads a temp file.
#[derive(Debug, Clone)]
pub struct TokenStorage {
    user_path: PathBuf,
    project_path: Option<PathBuf>,
    warned_stale_shadows: Arc<Mutex<HashSet<String>>>,
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
            warned_stale_shadows: Arc::new(Mutex::new(HashSet::new())),
            write_guard: Arc::new(Mutex::new(())),
        }
    }

    /// Construct with an explicit path (test helper).
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            user_path: path,
            project_path: None,
            warned_stale_shadows: Arc::new(Mutex::new(HashSet::new())),
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

    /// Load merged tokens: user-level base, project-level overrides.
    ///
    /// Why: Preserves the documented override contract (project always wins
    /// per profile key) so existing project-scoped setups keep working
    /// unchanged. But a project override that is itself stale — e.g. minted
    /// once inside a worktree and never refreshed since — silently wins over
    /// a healthy user-level token and recreates the exact 401 loop from
    /// issue #2946 with no signal to the operator. This surfaces that case
    /// loudly instead of staying silent, without touching precedence. `load`
    /// sits on the per-request hot path (`BaseClient::get_access_token` calls
    /// it via `get_profile`/`get_default` on every MCP tool invocation, plus
    /// again on the 401-retry path), so the warning is throttled to once per
    /// profile per process by [`TokenStorage::warn_stale_shadow_once`]
    /// (PR #2949 review) — otherwise a persistent stale shadow would repeat
    /// the multi-line warning on every single call.
    /// What: Merges project-level entries over user-level per profile key;
    /// before merging, warns (at most once per profile) for every profile
    /// where the project-level entry is expired while the user-level entry
    /// it shadows is not.
    /// Test: `load_warns_when_project_shadow_is_stale` (warning fires),
    /// `load_does_not_rewarn_on_second_load` (throttled to once), plus
    /// `load_still_prefers_project_override_after_warning` for the
    /// unchanged-precedence contract.
    pub fn load(&self) -> Result<HashMap<String, StoredToken>> {
        let mut merged = Self::load_from(&self.user_path).unwrap_or_default();
        if let Some(project_path) = &self.project_path {
            let project_tokens = Self::load_from(project_path).unwrap_or_default();
            for (profile, project_token) in &project_tokens {
                if let Some(user_token) = merged.get(profile) {
                    self.warn_stale_shadow_once(profile, project_token, user_token, project_path);
                }
            }
            merged.extend(project_tokens);
        }
        Ok(merged)
    }

    /// Emit the stale-project-shadow warning for `profile`, at most once per
    /// profile for the lifetime of this `TokenStorage` (and every clone of
    /// it, since `warned_stale_shadows` is `Arc`-shared).
    ///
    /// Why: Isolated out of `load()` so the throttling mechanism (an
    /// insert-returns-false-if-present check on a shared `HashSet`) is a
    /// single, obviously-correct spot, and so the structured `tracing::warn!`
    /// call site (house style: named fields, not a preformatted string) is
    /// separate from the pure stale/fresh decision in
    /// [`stale_shadow_warning`].
    /// What: Computes [`stale_shadow_warning`]; on `Some`, inserts `profile`
    /// into `warned_stale_shadows` and only logs when the insert reports the
    /// profile was newly added (i.e. this is the first time this process has
    /// seen this exact stale shadow for this profile). A poisoned mutex
    /// (only reachable if a prior holder panicked mid-lock) recovers via
    /// `into_inner` rather than propagating — a missed dedup entry is
    /// harmless (one extra warning), unlike propagating a panic through the
    /// hot read path.
    /// Test: `load_warns_when_project_shadow_is_stale`,
    /// `load_does_not_rewarn_on_second_load`.
    fn warn_stale_shadow_once(
        &self,
        profile: &str,
        project: &StoredToken,
        user: &StoredToken,
        project_path: &Path,
    ) {
        let Some(info) = stale_shadow_warning(project, user, project_path, &self.user_path) else {
            return;
        };
        let mut warned = self
            .warned_stale_shadows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !warned.insert(profile.to_string()) {
            return; // already warned for this profile this process
        }
        drop(warned);
        warn!(
            profile = %profile,
            project_path = %info.project_path.display(),
            project_expires_at = %info.project_expires_at,
            user_path = %info.user_path.display(),
            user_expires_at = %info.user_expires_at,
            "project-level tokens.json override is EXPIRED but shadows a valid user-level \
             token for this profile — serving the STALE override per the documented \
             project-overrides-user contract. Run setup from this directory, or remove the \
             project override, to use the fresher token. (This warning fires once per \
             profile per process.)"
        );
    }

    /// Save tokens to the primary write path (project if known, else user).
    ///
    /// Why: `tokens.json` holds live OAuth refresh tokens — on Unix we
    /// restrict it to owner-only (0600) so other local users/processes can't
    /// read it off disk. This crate is now a primary minting path (not just
    /// a reader of Python-written files), so it must not regress that.
    /// What: Writes the pretty-printed JSON, then (Unix only) chmods the
    /// file to 0600. Byte content is unchanged — this only affects file mode.
    /// Test: `save_restricts_permissions_on_unix` (cfg(unix)).
    pub fn save(&self, tokens: &HashMap<String, StoredToken>) -> Result<()> {
        let target = self
            .project_path
            .clone()
            .unwrap_or_else(|| self.user_path.clone());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(tokens)?;
        std::fs::write(&target, data)
            .with_context(|| format!("write tokens to {}", target.display()))?;
        Self::restrict_permissions(&target)?;
        Ok(())
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

    /// The path `save()` actually writes to (project override if known, else
    /// the user-level file) — also the file this process's writes must be
    /// serialised against.
    fn primary_path(&self) -> PathBuf {
        self.project_path
            .clone()
            .unwrap_or_else(|| self.user_path.clone())
    }

    /// Sidecar lock file path for [`TokenStorage::update`]'s cross-process
    /// lock — never contains token data itself.
    fn lock_path(&self) -> PathBuf {
        let target = self.primary_path();
        let file_name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tokens.json".to_string());
        target.with_file_name(format!("{file_name}.lock"))
    }

    /// Perform an atomic read-modify-write on the stored token map.
    ///
    /// Why: `OAuthManager::refresh` and the consent flow's `persist` (and the
    /// CLI's `accounts default`/`accounts remove`) each used to do
    /// `load()` -> mutate -> `save()` with no synchronization. Two concurrent
    /// writers — different profiles refreshing at once, in the same or
    /// different processes — could each `load()` the map before either
    /// `save()`d, silently losing whichever write lost the race (issue
    /// #3502). Centralising the whole cycle here, guarded by a lock, means
    /// every call site gets the fix for free instead of re-deriving it.
    /// What: Acquires an in-process mutex (serialises clones of this
    /// `TokenStorage` within the current process) and, inside that, an
    /// advisory exclusive lock on a sidecar `<path>.lock` file (serialises
    /// across processes; blocks until acquired — contention here is brief:
    /// one JSON parse + one JSON write). Reloads the map fresh under both
    /// locks (never trusts a caller's possibly-stale copy), applies `f`, and
    /// saves before releasing. Both locks release automatically on return
    /// (including on error, via `?` and RAII guards) so a failure never
    /// leaves the file locked.
    /// Test: `concurrent_updates_do_not_lose_writes`.
    pub fn update<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut HashMap<String, StoredToken>) -> Result<T>,
    {
        let _in_process_guard = self
            .write_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let lock_path = self.lock_path();
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open lock file {}", lock_path.display()))?;
        let mut rw_lock = fd_lock::RwLock::new(lock_file);
        let _file_guard = rw_lock
            .write()
            .with_context(|| format!("acquire lock on {}", lock_path.display()))?;

        let mut tokens = self.load()?;
        let result = f(&mut tokens)?;
        self.save(&tokens)?;
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
/// mirroring the `stale_shadow_warning` / `should_set_default` pattern
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

/// Structured detail of a stale project-level shadow, for the `tracing::warn!`
/// fields in [`TokenStorage::warn_stale_shadow_once`].
///
/// Why: Split out of the message-formatting version of this check so `load`
/// can log with named fields (house style) instead of a single preformatted
/// string, while [`stale_shadow_warning`] stays a pure, directly-testable
/// decision function.
/// What: Owned copies of both paths and both `expires_at` timestamps.
/// Test: covered transitively via [`stale_shadow_warning`]'s own tests and
/// `load_warns_when_project_shadow_is_stale`.
struct StaleShadowInfo {
    project_path: PathBuf,
    project_expires_at: DateTime<Utc>,
    user_path: PathBuf,
    user_expires_at: DateTime<Utc>,
}

/// Decide whether a project-level token entry shadows a user-level one for
/// the same profile while itself being stale.
///
/// Why: Isolated as a pure predicate (no I/O, no logging) so the exact
/// condition is directly unit-testable without a temp filesystem or a
/// tracing-capturing test harness — mirrors the `refresh_failure_message`
/// pattern in `oauth::errors`.
/// What: Returns `Some(StaleShadowInfo)` naming both paths and both
/// `expires_at` timestamps when `project` is expired and `user` is not;
/// `None` in every other case (both fresh, both stale, or the project entry
/// is the fresher one) — those cases are not the silent-stale-shadow failure
/// mode this guards against.
/// Test: `warns_when_project_stale_and_user_fresh`,
/// `no_warning_when_both_fresh`, `no_warning_when_both_stale`,
/// `no_warning_when_project_is_fresher`.
fn stale_shadow_warning(
    project: &StoredToken,
    user: &StoredToken,
    project_path: &Path,
    user_path: &Path,
) -> Option<StaleShadowInfo> {
    if project.token.is_expired() && !user.token.is_expired() {
        Some(StaleShadowInfo {
            project_path: project_path.to_path_buf(),
            project_expires_at: project.token.expires_at,
            user_path: user_path.to_path_buf(),
            user_expires_at: user.token.expires_at,
        })
    } else {
        None
    }
}

impl Default for TokenStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
