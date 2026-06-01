//! GitHub App authentication: JWT minting and installation-token exchange.
//!
//! Why: GitHub App authentication uses short-lived JWTs (max 10 minutes)
//! signed with the App's RSA private key, then exchanged for even shorter-lived
//! installation access tokens (max 1 hour).  Building this in Rust avoids the
//! Python subprocess dependency and gives us proper error types.
//! (spec REV-401, source-analysis §4.1)
//!
//! What: `mint_app_jwt` signs a JWT with RS256 (iss=App ID, iat, exp=iat+600s);
//! `exchange_installation_token` POSTs to GitHub's installation-token endpoint
//! and returns the short-lived token string; `resolve_token` selects the
//! correct installation by org-name (case-insensitive) or falls back to a PAT.
//!
//! Test: `jwt_claims_correctness` verifies iss/iat/exp without a network call;
//! `resolve_token_pat_fallback` verifies PAT fallback when no App config is set.

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::integrations::github::{GithubClient, GithubError};

// ─── JWT claims shape ─────────────────────────────────────────────────────────

/// JWT claims for a GitHub App authentication token.
///
/// Why: GitHub requires exactly these three fields in the App JWT payload
/// (https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-jwt-for-a-github-app).
/// What: `iss` is the App ID as a string, `iat` is the issued-at Unix timestamp,
/// `exp` is the expiry Unix timestamp (max 10 minutes after `iat`).
/// Test: `jwt_claims_correctness` constructs and decodes these claims.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppJwtClaims {
    /// GitHub App ID (issuer).
    pub iss: String,
    /// Issued-at: Unix epoch seconds (60s in the past to allow clock skew).
    pub iat: u64,
    /// Expiry: Unix epoch seconds (iat + 600s max per GitHub docs).
    pub exp: u64,
}

// ─── Installation token response shape ────────────────────────────────────────

/// Response from `POST /app/installations/{installation_id}/access_tokens`.
///
/// Why: we only need the `token` field from the response for subsequent API
/// calls; the rest of the fields are ignored in the MVP.
/// What: a minimal deserialisation target for the GitHub API response.
/// Test: `installation_token_deserialises` covers happy-path JSON.
#[derive(Debug, Deserialize)]
pub struct InstallationTokenResponse {
    /// The short-lived installation access token.
    pub token: String,
}

// ─── JWT minting ──────────────────────────────────────────────────────────────

/// Mint a GitHub App JWT valid for 10 minutes.
///
/// Why: all GitHub App API calls require a signed JWT; `jsonwebtoken` (already
/// a workspace dep) handles RS256 signing natively.
/// What: reads the current Unix timestamp via `SystemTime`, sets `iat` 60 seconds
/// in the past (to tolerate clock skew between the caller and GitHub), and
/// sets `exp = iat + 660s` (10 minutes + the 60s skew buffer, keeping the
/// effective window at 10 minutes).  The PEM may be either a bare PKCS#8 block
/// or a PKCS#1 block (RSAPrivateKey); `EncodingKey::from_rsa_pem` handles both.
/// Test: `jwt_claims_correctness` decodes the minted JWT and asserts iss/iat/exp.
pub fn mint_app_jwt(app_id: &str, private_key_pem: &str) -> Result<String, GithubError> {
    let encoding_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|e| GithubError::Auth(format!("invalid App private key PEM: {e}")))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| GithubError::Auth(format!("system clock before Unix epoch: {e}")))?
        .as_secs();

    // GitHub recommends setting iat 60 seconds in the past to allow for clock
    // drift between the requester and GitHub's servers.
    let iat = now.saturating_sub(60);
    let exp = iat + 660; // 60s skew + 600s (10 min) window.

    let claims = AppJwtClaims {
        iss: app_id.to_string(),
        iat,
        exp,
    };

    let header = Header::new(Algorithm::RS256);
    jsonwebtoken::encode(&header, &claims, &encoding_key)
        .map_err(|e| GithubError::Auth(format!("JWT signing failed: {e}")))
}

// ─── Installation token exchange ──────────────────────────────────────────────

/// Exchange an App JWT for a short-lived installation access token.
///
/// Why: installation tokens are required for all GitHub API calls on behalf of
/// an installation (e.g. reading PRs, posting review comments in an org).
/// What: `POST /app/installations/{installation_id}/access_tokens` with the
/// App JWT as a Bearer token.  Returns the installation token string.
/// Test: requires a live GitHub App — covered by integration tests only;
/// `install_token_exchange_returns_transport_on_unreachable` tests error path.
pub async fn exchange_installation_token(
    client: &GithubClient,
    app_jwt: &str,
    installation_id: u64,
) -> Result<String, GithubError> {
    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");
    let resp = client
        .http
        .post(&url)
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", format!("Bearer {app_jwt}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", &client.user_agent)
        .send()
        .await
        .map_err(|e| GithubError::Transport(format!("POST {url}: {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| GithubError::Transport(format!("read body of {url}: {e}")))?;

    if !status.is_success() {
        return Err(GithubError::Api {
            status: status.as_u16(),
            body,
        });
    }

    let token_resp: InstallationTokenResponse = serde_json::from_str(&body).map_err(|e| {
        GithubError::Transport(format!("failed to parse installation token response: {e}"))
    })?;

    Ok(token_resp.token)
}

// ─── Multi-org token resolution ───────────────────────────────────────────────

/// Resolve the correct access token for a GitHub org owner.
///
/// Why: the bot may be installed in multiple orgs (e.g. `duettoresearch`,
/// `hotstats`); the correct installation token is selected by org name.
/// (spec REV-402)
/// What: if App auth is configured (app_id + private_key + at least one
/// installation), mints an App JWT and exchanges it for the installation token
/// matching `owner` (case-insensitive).  Falls back to the PAT if no
/// installation matches or if App auth is not configured.  Returns
/// `Err(GithubError::MissingToken)` only if neither App auth nor PAT is
/// available.
/// Test: `resolve_token_pat_fallback` verifies the PAT path.
pub async fn resolve_token(
    client: &GithubClient,
    config: &crate::config::ReviewConfig,
    owner: &str,
) -> Result<String, GithubError> {
    // Try App auth if configured.
    if let (Some(app_id), Some(private_key)) = (
        config.github_app_id.as_deref(),
        config.github_app_private_key.as_deref(),
    ) {
        // Find a matching installation by case-insensitive owner name.
        let matching_id = config
            .github_installations
            .iter()
            .find_map(|(inst_owner, inst_id)| {
                if inst_owner.to_lowercase() == owner.to_lowercase() {
                    Some(*inst_id)
                } else {
                    None
                }
            });

        if let Some(installation_id) = matching_id {
            let jwt = mint_app_jwt(app_id, private_key)?;
            return exchange_installation_token(client, &jwt, installation_id).await;
        }

        // No installation matched — fall through to PAT.
        tracing::debug!(
            owner,
            "no GitHub App installation found for owner; falling back to PAT"
        );
    }

    // PAT fallback.
    if config.github_token.is_empty() {
        return Err(GithubError::MissingToken);
    }
    Ok(config.github_token.clone())
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// An RSA-2048 PKCS#8 private key generated offline for testing only.
    /// This key is test-only and carries no secrets; it is not used in production.
    /// Generated with: openssl genrsa 2048 | openssl pkcs8 -topk8 -nocrypt
    // pragma: allowlist secret
    const TEST_RSA_PEM: &str = concat!(
        "-----BEGIN PRIVATE KEY-----\n", // pragma: allowlist secret
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCwqJLJt1WufjvL\n", // pragma: allowlist secret
        "kCguz23z3rY3tshu9hf95pwe5C2g2VSzMFHRggVTQLUE8ENA6km7vIRxtmwEBTVd\n", // pragma: allowlist secret
        "5Yz89dgwO9T2w7yKS1n1HuzSdyLSTNOw0TU+0AKmY45nslLxCnvkYyQbD2BCzlbx\n", // pragma: allowlist secret
        "LkDMsBMSlAMrJs2FUfLq1xXn3u+i35vuc2qLAo2p56xVmcs94qLo7UB5y5UC3N7G\n", // pragma: allowlist secret
        "yD2GWG99vThkFPj9VYjhwjwfTIfIr/8MTg6X5jNJGzr4ebntsWMfGKgseGOviYze\n", // pragma: allowlist secret
        "cS9vmhBLcuV0JAm1h6eIVbAOQHWPdF6lo7XaXc//xuPr8OqtSMAtgDJ06S8REYO2\n", // pragma: allowlist secret
        "YkHB+GOhAgMBAAECggEAF4NofkbTtbUBmnemkYx0cxg6orHGfdZtnRLbxtTSKe2j\n", // pragma: allowlist secret
        "c3JEAaHPuaQMNAsSuIo2pDFUY5pHSEW1M7lBCc5jJxBfqTSmXLXo1FJ4bQ8EaH9n\n", // pragma: allowlist secret
        "UcqWzrR7FdB8fNrkZUbi9KQpgxyJ0HqMYe+pGlV5RGjE/zJb+pnMvmtAdCtdNA1c\n", // pragma: allowlist secret
        "o0oaS6jLuC+gRRBKtmL2yin939ZrKTj3LySJTzenm+oq2wIuBS85uIYVQ9O4aMIl\n", // pragma: allowlist secret
        "lDjCsb3YawI4j+/69OptBq9c99QXBfxStOTpUi5IDsdt5i7iXaIGZiH8MiK2TFPx\n", // pragma: allowlist secret
        "fk5YvXDet2o9Cdt+iujuF7Fu8VgWu1t0jnzDT4TLEQKBgQDXSviIl63sHu+nNEes\n", // pragma: allowlist secret
        "zW8rGYmGWnmWSHChgyBdX4oTIigrO9mBlI5Bgilcw6+qxCyzw6PSmKakAg6FqP/5\n", // pragma: allowlist secret
        "sANqinY0j2xdL2sgoWnXOr5TSN3QJ5nNJKYpjEBh4TIqTWNNYTvn1K2JIG5+ATS4\n", // pragma: allowlist secret
        "Hng1QmaRYlk7DepX6LAYmz6g5QKBgQDSD4u9iXiDHBzHglPqakwqkC5XqnL7XR9s\n", // pragma: allowlist secret
        "qFseOqzwV2viINXsLFCg+rScvcB8Ce0GIT21gttcqDN9OOuujB1gaNYdHsMZx5mE\n", // pragma: allowlist secret
        "Hvzj9SB2sPO9LeDEUC/g/8ySdu08WSf+RZ0KR39hA0wtGNMiukPC+8iU3tJG2QiX\n", // pragma: allowlist secret
        "5IxlbFXYDQKBgQCsBn2cNwaDmxyHD+ENlID1gUxADF8G1A8bHvlnYoWjUDGkigf7\n", // pragma: allowlist secret
        "4EXi1ixSsRHWczX81aA7EDpm5jXQWv9d9WRlZwmYadl+g/sncZJupcOaLKkAQARG\n", // pragma: allowlist secret
        "xLf4jtaK3zQEVR25oK4LSgb3gPCIwlHrpH0MoWfvVxRReYb8gzLiFnnueQKBgECD\n", // pragma: allowlist secret
        "xcdQkVKzL6OWw28bdokb/x+tmeLZlu0oR9Pg8XxfXSL2Mr12Xs0SMqZxIMz3v3RC\n", // pragma: allowlist secret
        "gVFd/0FV53puIPRa1CroB9qpuAIS63NIkSLyBiZt8m4HySCCADJ6XboeDH6cY0wU\n", // pragma: allowlist secret
        "1UZy7ww8lwjCtxXTXzxjWBdg1/QqdBkyeGwt+a+BAoGATVFBJ+eW2sUuEjaopIiq\n", // pragma: allowlist secret
        "9YXh6GtKarglvVny+wd1gz/3/8Oy1Ik7s3mBn7QAiK9BL9B1YpmX7bYNSSTomXqg\n", // pragma: allowlist secret
        "oTRnhZb8BGsvbOSrPeHd8O1FzobrPZ8PYl1xVReOByjKw2vR4zVLIq6YvurQNB00\n", // pragma: allowlist secret
        "ii7j4jc5884tuleJyyumF4s=\n", // pragma: allowlist secret
        "-----END PRIVATE KEY-----",  // pragma: allowlist secret
    );

    #[test]
    fn jwt_claims_correctness() {
        // Verify that the minted JWT contains the correct iss/iat/exp claims.
        // We decode without validation (test key, not verified) to inspect claims.
        let app_id = "99999";
        let token = mint_app_jwt(app_id, TEST_RSA_PEM).expect("mint_app_jwt should succeed");

        // Decode without signature verification to inspect claims.
        let mut validation = jsonwebtoken::Validation::new(Algorithm::RS256);
        validation.insecure_disable_signature_validation();
        validation.set_required_spec_claims(&[] as &[&str]);

        let decoding_key = jsonwebtoken::DecodingKey::from_secret(&[]);
        let decoded = jsonwebtoken::decode::<AppJwtClaims>(&token, &decoding_key, &validation)
            .expect("decoding JWT claims should succeed");

        // iss must match the app_id.
        assert_eq!(decoded.claims.iss, app_id, "iss must equal the App ID");

        // iat must be in the past (approximately now - 60s).
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            decoded.claims.iat <= now,
            "iat ({}) must be <= now ({})",
            decoded.claims.iat,
            now
        );
        // iat should be within the last 120 seconds (60s skew + some test slack).
        assert!(
            decoded.claims.iat >= now.saturating_sub(120),
            "iat ({}) must be recent",
            decoded.claims.iat
        );

        // exp must be iat + 660 (60s skew buffer + 600s).
        assert_eq!(
            decoded.claims.exp,
            decoded.claims.iat + 660,
            "exp must be iat + 660"
        );
    }

    #[test]
    fn jwt_mint_fails_on_bad_pem() {
        let result = mint_app_jwt("123", "not-a-valid-pem");
        assert!(result.is_err(), "bad PEM should return Err");
        match result.unwrap_err() {
            GithubError::Auth(_) => {}
            other => panic!("expected Auth error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_token_pat_fallback() {
        // When no App config is provided, resolve_token falls back to the PAT.
        use crate::config::ReviewConfig;
        let mut config = ReviewConfig::load(None);
        config.github_token = "ghp_test_token".to_string();
        config.github_app_id = None;

        let client = GithubClient::new();
        let token = resolve_token(&client, &config, "any-owner").await;
        assert!(token.is_ok(), "PAT fallback should succeed: {token:?}");
        assert_eq!(token.unwrap(), "ghp_test_token");
    }

    #[tokio::test]
    async fn resolve_token_missing_token_errors() {
        // When neither App auth nor PAT is configured, expect MissingToken.
        use crate::config::ReviewConfig;
        let mut config = ReviewConfig::load(None);
        config.github_token = String::new();
        config.github_app_id = None;

        let client = GithubClient::new();
        let result = resolve_token(&client, &config, "any-owner").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            GithubError::MissingToken => {}
            other => panic!("expected MissingToken, got {other:?}"),
        }
    }
}
