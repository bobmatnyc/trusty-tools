//! On-disk OAuth token storage compatible with the Python CLI.
//!
//! Why: We want to share `~/.gworkspace-mcp/tokens.json` between the Python
//! CLI (which performs the interactive OAuth flow) and this Rust MCP server.
//! What: Reads/writes a `HashMap<profile_name, StoredToken>` JSON object.
//! Two-tier lookup: project-level `./.gworkspace-mcp/tokens.json` first,
//! then `~/.gworkspace-mcp/tokens.json`.
//! Test: see integration test `tests/auth_models.rs`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::models::StoredToken;
use crate::api::constants::DEFAULT_PROFILE;

/// Token file storage with two-tier lookup (project then user).
///
/// Why: Matches Python `TokenStorage` semantics — project-level overrides
/// user-level, while user-level is the durable fallback.
/// What: Holds the user-level path (always `~/.gworkspace-mcp/tokens.json`)
/// and an optional project-level path (`./.gworkspace-mcp/tokens.json`).
/// Test: integration test reads a temp file.
#[derive(Debug, Clone)]
pub struct TokenStorage {
    user_path: PathBuf,
    project_path: Option<PathBuf>,
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
        }
    }

    /// Construct with an explicit path (test helper).
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            user_path: path,
            project_path: None,
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
    pub fn load(&self) -> Result<HashMap<String, StoredToken>> {
        let mut merged = Self::load_from(&self.user_path).unwrap_or_default();
        if let Some(p) = &self.project_path {
            let project_tokens = Self::load_from(p).unwrap_or_default();
            merged.extend(project_tokens);
        }
        Ok(merged)
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
}
