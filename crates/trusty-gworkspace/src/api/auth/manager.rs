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

use super::models::{OAuthToken, StoredToken};
use super::oauth::errors::{redact_token_response, refresh_failure_message};
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
/// What: Reads `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET` env
/// vars on construction. `refresh` performs the HTTP exchange.
/// Test: Construction is covered by `from_env_returns_none_when_missing`.
pub struct OAuthManager {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
}

impl OAuthManager {
    /// Construct from env vars, returning `Ok(None)` when both are absent
    /// (read-only token mode — refresh disabled).
    pub fn from_env() -> Result<Option<Self>> {
        let id = std::env::var("GOOGLE_OAUTH_CLIENT_ID").ok();
        let secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET").ok();
        match (id, secret) {
            (Some(client_id), Some(client_secret)) => Ok(Some(Self {
                http: reqwest::Client::new(),
                client_id,
                client_secret,
            })),
            _ => Ok(None),
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

        let mut all = storage.load()?;
        all.insert(profile.to_string(), stored);
        storage.save(&all)?;

        Ok(new_token)
    }
}
