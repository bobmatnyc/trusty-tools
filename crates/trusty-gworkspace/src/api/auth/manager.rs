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
//! `invalid_grant`, sanitized error otherwise). Issue #3518: the client used
//! for the exchange is resolved PER-PROFILE (`oauth::client_store`) — a
//! profile authorized against its own client is always refreshed with that
//! same client, falling back to the global client only for profiles that
//! have none (backward compatible with every profile authorized before this
//! feature existed).
//! Test: The live HTTP exchange is covered against a `wiremock` server —
//! `refresh_uses_per_profile_client_when_present` and
//! `refresh_falls_back_to_global_client_when_absent` assert the exact
//! `client_id` sent on the wire. The failure-message mapping is unit-tested
//! in `oauth::errors` (`refresh_failure_message_names_profile_and_setup_command`).

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, Utc};
use serde::Deserialize;

use super::models::{OAuthToken, StoredToken};
use super::oauth::errors::{redact_token_response, refresh_failure_message};
use super::oauth::resolve_client_creds_for_profile;
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
/// Why: Centralises the token-endpoint HTTP exchange so `BaseClient` only
/// needs to call one method to get a fresh token; the OAuth CLIENT itself is
/// no longer fixed at construction time (issue #3518) — it is resolved
/// per-profile on every call via [`resolve_client_creds_for_profile`], since
/// different profiles may be bound to different clients.
/// What: Holds a `reqwest::Client` and the token endpoint URL (overridable
/// in tests via [`OAuthManager::with_token_url`] to point at a `wiremock`
/// server instead of Google's real endpoint).
/// Test: `refresh_uses_per_profile_client_when_present`,
/// `refresh_falls_back_to_global_client_when_absent`.
pub struct OAuthManager {
    http: reqwest::Client,
    token_url: String,
}

impl OAuthManager {
    /// Construct against Google's real token endpoint.
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            token_url: OAUTH_TOKEN_URL.to_string(),
        }
    }

    /// Test-only constructor pointing the refresh exchange at `token_url`
    /// (a `wiremock` mock server) instead of Google's real endpoint.
    #[cfg(test)]
    pub(crate) fn with_token_url(token_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            token_url: token_url.into(),
        }
    }

    /// Refresh the access token for the given profile and persist the result.
    ///
    /// Why: The stored token is near or past expiry; we need a fresh one
    /// before the next API call.
    /// What: Resolves `profile`'s own OAuth client (falling back to the
    /// global one — see [`resolve_client_creds_for_profile`]), POSTs to
    /// Google's OAuth endpoint with `grant_type=refresh_token`, parses the
    /// response, updates `expires_at` to `now + expires_in`, and writes the
    /// updated `StoredToken` back to disk. On an HTTP failure it returns
    /// [`refresh_failure_message`]'s actionable error (naming the exact
    /// re-auth command on `invalid_grant`).
    /// Test: `refresh_uses_per_profile_client_when_present`,
    /// `refresh_falls_back_to_global_client_when_absent`; the failure-message
    /// mapping is covered by
    /// `refresh_failure_message_names_profile_and_setup_command`.
    pub async fn refresh(&self, storage: &TokenStorage, profile: &str) -> Result<OAuthToken> {
        let creds = resolve_client_creds_for_profile(profile)
            .with_context(|| format!("resolve OAuth client for profile '{profile}'"))?;

        let mut stored: StoredToken = storage
            .get_profile(profile)?
            .ok_or_else(|| anyhow!("no stored token for profile '{profile}'"))?;
        let refresh_token = stored
            .token
            .refresh_token
            .clone()
            .ok_or_else(|| anyhow!("no refresh_token available for profile '{profile}'"))?;

        let params = [
            ("client_id", creds.client_id.as_str()),
            ("client_secret", creds.client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ];
        let resp = self
            .http
            .post(&self.token_url)
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

impl Default for OAuthManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::auth::TokenStorage;
    use crate::api::auth::models::TokenMetadata;
    use crate::api::auth::oauth::client_store::profile_client_path;
    use crate::api::auth::test_support::{EnvGuard, fresh_temp_home};
    use serial_test::serial;
    use std::path::PathBuf;
    use wiremock::matchers::{body_string_contains, method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Env vars mutated by the fallback tests below; captured/restored as a
    /// group so a panic mid-test never leaks a fake `HOME` or client id into
    /// later tests.
    const MUTATED_ENV_VARS: &[&str] = &[
        "HOME",
        "GOOGLE_OAUTH_CLIENT_ID",
        "GOOGLE_OAUTH_CLIENT_SECRET",
    ];

    fn isolated_home(label: &str) -> PathBuf {
        let home = fresh_temp_home(label);
        // SAFETY: caller runs under #[serial]; EnvGuard (held by the caller)
        // restores every var on drop.
        unsafe {
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_ID");
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_SECRET");
            std::env::set_var("HOME", &home);
        }
        home
    }

    fn write_client_json(path: &std::path::Path, client_id: &str, client_secret: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            path,
            format!(r#"{{"client_id":"{client_id}","client_secret":"{client_secret}"}}"#),
        )
        .unwrap();
    }

    fn temp_storage_with_token(profile: &str, refresh_token: &str) -> TokenStorage {
        let dir = std::env::temp_dir().join(format!("gw-manager-tokens-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = TokenStorage::with_path(dir.join("tokens.json"));
        let mut map = std::collections::HashMap::new();
        map.insert(
            profile.to_string(),
            StoredToken {
                version: 1,
                metadata: TokenMetadata {
                    service_name: profile.to_string(),
                    provider: "google".into(),
                    created_at: Utc::now(),
                    last_refreshed: None,
                    email: Some(format!("{profile}@example.com")),
                    is_default: true,
                },
                token: OAuthToken {
                    access_token: "stale".into(),
                    refresh_token: Some(refresh_token.to_string()),
                    expires_at: Utc::now() - Duration::seconds(10),
                    scopes: vec!["openid".into()],
                    token_type: "Bearer".into(),
                },
            },
        );
        storage.save(&map).unwrap();
        storage
    }

    #[tokio::test]
    #[serial]
    async fn refresh_uses_per_profile_client_when_present() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        let home = isolated_home("refresh-per-profile");
        // A DIFFERENT global client is present too, to prove it is not used.
        write_client_json(
            &home.join(".gworkspace-mcp").join("oauth_client.json"),
            "global-client",
            "global-secret",
        );
        write_client_json(
            &profile_client_path("work"),
            "profile-client",
            "profile-secret",
        );
        let storage = temp_storage_with_token("work", "r-work");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/token"))
            .and(body_string_contains("client_id=profile-client"))
            .and(body_string_contains("client_secret=profile-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-access-token",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .mount(&server)
            .await;

        let mgr = OAuthManager::with_token_url(format!("{}/token", server.uri()));
        let refreshed = mgr
            .refresh(&storage, "work")
            .await
            .expect("refresh should succeed using the profile's own client");
        assert_eq!(refreshed.access_token, "new-access-token");

        let stored = storage.get_profile("work").unwrap().unwrap();
        assert_eq!(stored.token.access_token, "new-access-token");
    }

    #[tokio::test]
    #[serial]
    async fn refresh_falls_back_to_global_client_when_absent() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        let home = isolated_home("refresh-global-fallback");
        write_client_json(
            &home.join(".gworkspace-mcp").join("oauth_client.json"),
            "global-client",
            "global-secret",
        );
        // No per-profile client file for "legacy" — must keep using global.
        let storage = temp_storage_with_token("legacy", "r-legacy");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/token"))
            .and(body_string_contains("client_id=global-client"))
            .and(body_string_contains("client_secret=global-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "legacy-new-token",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .mount(&server)
            .await;

        let mgr = OAuthManager::with_token_url(format!("{}/token", server.uri()));
        let refreshed = mgr.refresh(&storage, "legacy").await.expect(
            "a profile with no per-profile client must still refresh via the global client \
                 (backward compatibility)",
        );
        assert_eq!(refreshed.access_token, "legacy-new-token");
    }

    #[tokio::test]
    #[serial]
    async fn refresh_errors_when_no_client_resolves_for_profile() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        isolated_home("refresh-no-creds");
        // Neither a per-profile client file nor a global oauth_client.json.
        let storage = temp_storage_with_token("orphan", "r-orphan");

        let mgr = OAuthManager::new();
        let err = mgr.refresh(&storage, "orphan").await.unwrap_err();
        assert!(err.to_string().contains("resolve OAuth client"), "{err}");
    }
}
