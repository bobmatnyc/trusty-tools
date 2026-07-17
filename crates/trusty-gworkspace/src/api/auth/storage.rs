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
//! and re-poisons `save()` on every re-auth).
//! Test: see integration test `tests/auth_models.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
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
        }
    }

    /// Construct with an explicit path (test helper).
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            user_path: path,
            project_path: None,
            warned_stale_shadows: Arc::new(Mutex::new(HashSet::new())),
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
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn save_restricts_permissions_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("gw-storage-perms-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokens.json");
        let storage = TokenStorage::with_path(path.clone());

        storage.save(&HashMap::new()).expect("save");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "tokens.json must be owner-read/write only, got {:o}",
            mode & 0o777
        );
    }

    #[test]
    fn save_still_round_trips_content() {
        // Guards against the permissions change accidentally altering the
        // byte-serde-compatible wire format.
        let dir = std::env::temp_dir().join(format!("gw-storage-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = TokenStorage::with_path(dir.join("tokens.json"));

        let mut tokens = HashMap::new();
        tokens.insert(
            "primary".to_string(),
            StoredToken {
                version: 1,
                metadata: crate::api::auth::models::TokenMetadata {
                    service_name: "primary".into(),
                    provider: "google".into(),
                    created_at: chrono::Utc::now(),
                    last_refreshed: None,
                    email: Some("user@example.com".into()),
                    is_default: true,
                },
                token: crate::api::auth::models::OAuthToken {
                    access_token: "a".into(),
                    refresh_token: Some("r".into()),
                    expires_at: chrono::Utc::now() + chrono::Duration::seconds(3600),
                    scopes: vec!["openid".into()],
                    token_type: "Bearer".into(),
                },
            },
        );

        storage.save(&tokens).expect("save");
        let loaded = storage.load().expect("load");
        assert_eq!(
            loaded["primary"].metadata.email.as_deref(),
            Some("user@example.com")
        );
        assert!(loaded["primary"].metadata.is_default);
    }

    /// Build a `StoredToken` expiring `expires_in_secs` from now (negative =
    /// already expired) for the stale-shadow-warning tests below.
    fn make_stored(expires_in_secs: i64) -> StoredToken {
        StoredToken {
            version: 1,
            metadata: crate::api::auth::models::TokenMetadata {
                service_name: "test".into(),
                provider: "google".into(),
                created_at: chrono::Utc::now(),
                last_refreshed: None,
                email: Some("user@example.com".into()),
                is_default: false,
            },
            token: crate::api::auth::models::OAuthToken {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at: chrono::Utc::now() + chrono::Duration::seconds(expires_in_secs),
                scopes: vec!["openid".into()],
                token_type: "Bearer".into(),
            },
        }
    }

    #[test]
    fn warns_when_project_stale_and_user_fresh() {
        let project = make_stored(-3600);
        let user = make_stored(3600);
        let info = stale_shadow_warning(
            &project,
            &user,
            Path::new("/proj/.gworkspace-mcp/tokens.json"),
            Path::new("/home/user/.gworkspace-mcp/tokens.json"),
        )
        .expect("must warn: project stale, user fresh");
        assert_eq!(
            info.project_path,
            Path::new("/proj/.gworkspace-mcp/tokens.json")
        );
        assert_eq!(
            info.user_path,
            Path::new("/home/user/.gworkspace-mcp/tokens.json")
        );
        assert_eq!(info.project_expires_at, project.token.expires_at);
        assert_eq!(info.user_expires_at, user.token.expires_at);
    }

    #[test]
    fn no_warning_when_both_fresh() {
        let project = make_stored(3600);
        let user = make_stored(7200);
        assert!(stale_shadow_warning(&project, &user, Path::new("p"), Path::new("u")).is_none());
    }

    #[test]
    fn no_warning_when_both_stale() {
        // Both stale is not the silent-shadow failure mode — the caller gets
        // an expired token either way, so no extra signal is needed here.
        let project = make_stored(-3600);
        let user = make_stored(-7200);
        assert!(stale_shadow_warning(&project, &user, Path::new("p"), Path::new("u")).is_none());
    }

    #[test]
    fn no_warning_when_project_is_fresher() {
        let project = make_stored(3600);
        let user = make_stored(-3600);
        assert!(stale_shadow_warning(&project, &user, Path::new("p"), Path::new("u")).is_none());
    }

    /// Build a two-tier temp `TokenStorage` (separate user/project dirs) with
    /// a `work` profile present on both sides, and write the given
    /// user/project token maps to disk. Shared by the precedence and
    /// warn-capture tests below.
    fn temp_shadowed_storage(
        label: &str,
        user_expires_in_secs: i64,
        project_expires_in_secs: i64,
    ) -> TokenStorage {
        let dir = std::env::temp_dir().join(format!("gw-storage-{label}-{}", uuid::Uuid::new_v4()));
        let user_dir = dir.join("user");
        let project_dir = dir.join("project");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        let user_path = user_dir.join("tokens.json");
        let project_path = project_dir.join("tokens.json");

        let mut user_tokens = HashMap::new();
        user_tokens.insert("work".to_string(), make_stored(user_expires_in_secs));
        std::fs::write(&user_path, serde_json::to_string(&user_tokens).unwrap()).unwrap();

        let mut project_tokens = HashMap::new();
        let mut project_stored = make_stored(project_expires_in_secs);
        project_stored.token.access_token = "stale-access-token".into();
        project_tokens.insert("work".to_string(), project_stored);
        std::fs::write(
            &project_path,
            serde_json::to_string(&project_tokens).unwrap(),
        )
        .unwrap();

        TokenStorage {
            user_path,
            project_path: Some(project_path),
            warned_stale_shadows: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    #[test]
    fn load_still_prefers_project_override_after_warning() {
        // Regression guard for the precedence contract: this fix only adds a
        // warning, it does not change which entry wins (see PR body) — a
        // stale project override must still be served, just noisily.
        let storage = temp_shadowed_storage("precedence", 3600, -3600);

        let loaded = storage.load().unwrap();
        assert_eq!(
            loaded["work"].token.access_token, "stale-access-token",
            "project override must still win per the documented contract \
             (warn, don't change precedence)"
        );
    }

    #[test]
    #[tracing_test::traced_test]
    fn load_warns_when_project_shadow_is_stale() {
        let storage = temp_shadowed_storage("warns", 3600, -3600);

        storage.load().unwrap();

        assert!(logs_contain("work"));
        assert!(logs_contain("STALE"));
    }

    #[test]
    #[tracing_test::traced_test]
    fn load_does_not_rewarn_on_second_load() {
        // PR #2949 review (HIGH): load() sits on the per-request hot path —
        // an unthrottled warn would repeat on every single MCP tool call.
        // Two loads on the same TokenStorage must produce exactly one
        // stale-shadow warning, not two.
        let storage = temp_shadowed_storage("norewarn", 3600, -3600);

        storage.load().unwrap();
        storage.load().unwrap();

        logs_assert(|lines: &[&str]| {
            let count = lines
                .iter()
                .filter(|l| l.contains("STALE") && l.contains("work"))
                .count();
            if count == 1 {
                Ok(())
            } else {
                Err(format!(
                    "expected exactly 1 stale-shadow warning after two load() calls, found {count}"
                ))
            }
        });
    }
}
