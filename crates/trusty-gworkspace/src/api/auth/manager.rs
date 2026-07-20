//! OAuth refresh manager — exchanges a refresh token for a new access token.
//!
//! Why: Access tokens expire ~1 hour, so we refresh before requests go stale.
//! When the *refresh* token itself is expired or revoked Google replies with
//! `invalid_grant`; the only fix is interactive re-consent, so this path must
//! surface the exact `gworkspace-mcp setup --profile <name>` command rather
//! than an opaque HTTP body.
//! What: POSTs `grant_type=refresh_token` to Google's OAuth token endpoint,
//! updates the on-disk record on success, and routes failures through the
//! shared [`refresh_failure_message`] helper (actionable re-auth hint on
//! `invalid_grant`, sanitized error otherwise).
//! Test: Manual — requires real Google credentials. The failure-message mapping
//! is unit-tested in `oauth::errors`
//! (`refresh_failure_message_names_profile_and_setup_command`).

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, Utc};
use serde::Deserialize;
use tracing::warn;

use super::models::{OAuthToken, StoredToken};
use super::oauth::errors::{redact_token_response, refresh_failure_message};
use super::oauth::resolve_client_creds;
use super::storage::TokenStorage;
use crate::api::constants::OAUTH_TOKEN_URL;

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// OAuth refresh manager.
///
/// Why: Encapsulates Google's OAuth client credentials so `BaseClient` only
/// needs to call one method to get a fresh token.
/// What: Resolves `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET` env
/// vars first, falling back to `~/.gworkspace-mcp/oauth_client.json` (see
/// `from_env`), on construction. `refresh` performs the HTTP exchange.
/// Test: Construction is covered by `from_env_falls_back_to_oauth_client_json`,
/// `from_env_returns_none_when_both_absent`, and
/// `from_env_prefers_env_vars_over_file`.
pub struct OAuthManager {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
}

impl OAuthManager {
    /// Construct from env vars, falling back to `oauth_client.json` on disk.
    ///
    /// Why: Issue #2946 — no tm-managed session sets
    /// `GOOGLE_OAUTH_CLIENT_ID`/`SECRET`, so `from_env` previously always
    /// returned `None` there, silently disabling self-refresh: every
    /// tm-managed MCP server ran read-only-token mode and 401'd on Google
    /// once the access token expired (~1h). `setup`/`doctor` already resolve
    /// client credentials with an env-first, file-fallback strategy via
    /// `resolve_client_creds`; reusing it here (rather than duplicating the
    /// parse logic) gives the refresh path the same fallback for free.
    /// What: Delegates to [`resolve_client_creds`] (env vars win when
    /// present, else reads `~/.gworkspace-mcp/oauth_client.json`). Returns
    /// `Ok(None)` — with a warning logged — only when neither source yields
    /// credentials (read-only token mode: refresh disabled).
    /// Test: `from_env_falls_back_to_oauth_client_json`,
    /// `from_env_returns_none_when_both_absent`,
    /// `from_env_prefers_env_vars_over_file`.
    pub fn from_env() -> Result<Option<Self>> {
        match resolve_client_creds() {
            Ok(creds) => Ok(Some(Self {
                http: reqwest::Client::new(),
                client_id: creds.client_id,
                client_secret: creds.client_secret,
            })),
            Err(e) => {
                warn!(
                    error = %e,
                    "no OAuth client credentials found (env vars or oauth_client.json); \
                     token refresh disabled — expired tokens will not self-refresh"
                );
                Ok(None)
            }
        }
    }

    /// Refresh the access token for the given profile and persist the result.
    ///
    /// Why: The stored token is near or past expiry; we need a fresh one
    /// before the next API call.
    /// What: POSTs to Google's OAuth endpoint with `grant_type=refresh_token`,
    /// parses the response, updates `expires_at` to `now + expires_in`, and
    /// writes the updated `StoredToken` back to disk. On an HTTP failure it
    /// returns [`refresh_failure_message`]'s actionable error (naming the exact
    /// re-auth command on `invalid_grant`).
    /// Test: live path needs real Google creds; the failure-message mapping is
    /// covered by `refresh_failure_message_names_profile_and_setup_command`.
    pub async fn refresh(&self, storage: &TokenStorage, profile: &str) -> Result<OAuthToken> {
        let mut stored: StoredToken = storage
            .get_profile(profile)?
            .ok_or_else(|| anyhow!("no stored token for profile '{profile}'"))?;
        let refresh_token = stored
            .token
            .refresh_token
            .clone()
            .ok_or_else(|| anyhow!("no refresh_token available for profile '{profile}'"))?;

        let params = [
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ];
        let resp = self
            .http
            .post(OAUTH_TOKEN_URL)
            .form(&params)
            .send()
            .await
            .context("POST oauth2 token endpoint")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "{}",
                refresh_failure_message(status, &body, profile)
            ));
        }
        // NB: `body` here is a 2xx token response containing access/refresh
        // tokens; on the (practically unreachable) parse failure it must be
        // redacted, never embedded verbatim, before entering the error chain.
        let parsed: GoogleTokenResponse = serde_json::from_str(&body)
            .with_context(|| format!("parse token response: {}", redact_token_response(&body)))?;

        let expires_in = parsed.expires_in.unwrap_or(3600);
        let new_token = OAuthToken {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token.or(Some(refresh_token)),
            expires_at: Utc::now() + Duration::seconds(expires_in),
            scopes: parsed
                .scope
                .map(|s| s.split_whitespace().map(String::from).collect())
                .unwrap_or(stored.token.scopes.clone()),
            token_type: parsed.token_type.unwrap_or_else(|| "Bearer".into()),
        };

        stored.token = new_token.clone();
        stored.metadata.last_refreshed = Some(Utc::now());

        // Guard against a concurrent refresh of a different profile (or the
        // same one) losing this write — see `TokenStorage::update` (#3502).
        storage.update(|all| {
            all.insert(profile.to_string(), stored);
            Ok(())
        })?;

        Ok(new_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::PathBuf;

    /// Env vars mutated by the fallback tests below; captured/restored as a
    /// group so a panic mid-test never leaks a fake `HOME` or client id into
    /// later tests.
    const MUTATED_ENV_VARS: &[&str] = &[
        "HOME",
        "GOOGLE_OAUTH_CLIENT_ID",
        "GOOGLE_OAUTH_CLIENT_SECRET",
    ];

    /// RAII guard that snapshots and restores a fixed set of env vars, even
    /// on panic.
    ///
    /// Why: `from_env`'s fallback path reads `HOME` (via `dirs::home_dir`)
    /// indirectly through `resolve_client_creds`; testing it in-process means
    /// mutating real process env state, which must never leak across tests
    /// (issue #2946 fallback tests run `#[serial]` for the same reason).
    /// What: Captures each var's current value on construction; `Drop`
    /// restores it (`set_var` if it was present, `remove_var` if absent).
    /// Test: exercised by every test in this module — a leaked var would
    /// make a later, unrelated test flaky.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn capture(vars: &[&'static str]) -> Self {
            let saved = vars.iter().map(|&v| (v, std::env::var(v).ok())).collect();
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                // SAFETY: every caller of `EnvGuard` runs under `#[serial]`,
                // so no other thread reads/writes these vars concurrently.
                match v {
                    Some(val) => unsafe { std::env::set_var(k, val) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }

    /// Build a fresh temp dir with a `.gworkspace-mcp/` subdir, never
    /// touching the real `~/.gworkspace-mcp`.
    fn fresh_temp_home(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gw-manager-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".gworkspace-mcp")).expect("mkdir temp home");
        dir
    }

    #[test]
    #[serial]
    fn from_env_falls_back_to_oauth_client_json() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        // SAFETY: serialised via #[serial]; EnvGuard restores on drop.
        unsafe {
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_ID");
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_SECRET");
        }
        let home = fresh_temp_home("fallback");
        std::fs::write(
            home.join(".gworkspace-mcp").join("oauth_client.json"),
            r#"{"client_id":"file-id","client_secret":"file-secret"}"#,
        )
        .expect("write oauth_client.json");
        // SAFETY: see above.
        unsafe { std::env::set_var("HOME", &home) };

        let mgr = OAuthManager::from_env()
            .expect("from_env should not error")
            .expect("oauth_client.json fallback should enable refresh");
        assert_eq!(mgr.client_id, "file-id");
        assert_eq!(mgr.client_secret, "file-secret");
    }

    #[test]
    #[serial]
    fn from_env_returns_none_when_both_absent() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        // SAFETY: see EnvGuard doc.
        unsafe {
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_ID");
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_SECRET");
        }
        let home = fresh_temp_home("absent");
        // SAFETY: see EnvGuard doc.
        unsafe { std::env::set_var("HOME", &home) };

        let mgr = OAuthManager::from_env().expect("from_env should not error (warn, not error)");
        assert!(
            mgr.is_none(),
            "refresh must stay disabled with neither env vars nor oauth_client.json present"
        );
    }

    #[test]
    #[serial]
    fn from_env_prefers_env_vars_over_file() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        let home = fresh_temp_home("precedence");
        std::fs::write(
            home.join(".gworkspace-mcp").join("oauth_client.json"),
            r#"{"client_id":"file-id","client_secret":"file-secret"}"#,
        )
        .expect("write oauth_client.json");
        // SAFETY: see EnvGuard doc.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("GOOGLE_OAUTH_CLIENT_ID", "env-id");
            std::env::set_var("GOOGLE_OAUTH_CLIENT_SECRET", "env-secret");
        }

        let mgr = OAuthManager::from_env()
            .expect("from_env should not error")
            .expect("env vars should enable refresh");
        assert_eq!(mgr.client_id, "env-id", "env vars must win over the file");
        assert_eq!(mgr.client_secret, "env-secret");
    }
}
