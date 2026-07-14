//! Interactive authorization-code + PKCE consent flow orchestration.
//!
//! Why: To mint `~/.gworkspace-mcp/tokens.json` natively (issue #2631) the
//! crate must run the full installed-app OAuth dance itself instead of
//! shelling out to the Python CLI. This module wires the offline-testable
//! primitives (`pkce`, `callback`) together with the two network calls
//! (token exchange, optional userinfo) and the existing token storage.
//! What: `resolve_client_creds` finds the OAuth client id/secret; `run_consent`
//! drives browser consent → code → token exchange → email resolution →
//! persistence, returning a `ConsentOutcome` for the CLI to print.
//! Test: `assemble_scope_string`, `build_auth_url`, and `client-creds` file
//! parsing are unit-tested; the live browser round-trip is deferred (needs a
//! real Google OAuth client + browser — see PR note).

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

use super::callback;
use super::pkce::{self, Pkce};
use crate::api::auth::models::{OAuthToken, StoredToken, TokenMetadata};
use crate::api::auth::storage::TokenStorage;
use crate::api::constants::{
    DEFAULT_PROFILE, OAUTH_AUTH_URL, OAUTH_SCOPES, OAUTH_TOKEN_URL, USERINFO_URL,
};

/// Resolved OAuth client credentials for the installed-app flow.
///
/// Why: Both the authorization request and the token exchange need the same
/// client id/secret; resolving them once avoids env re-reads and divergent
/// error handling.
/// What: Plain owned strings; sourced from env or a config file.
/// Test: `resolve_client_creds` file parsing via `parse_client_creds_json`.
#[derive(Debug, Clone)]
pub struct ClientCreds {
    /// OAuth 2.0 client id.
    pub client_id: String,
    /// OAuth 2.0 client secret (installed-app "secret").
    pub client_secret: String,
}

/// The user-visible result of a successful consent flow.
///
/// Why: The `setup` CLI prints which account was authorized and where it was
/// stored so the user gets confirmation.
/// What: The profile key written to `tokens.json` and the resolved email.
/// Test: Populated by `run_consent`; live path deferred.
#[derive(Debug, Clone)]
pub struct ConsentOutcome {
    /// Profile key under which the token was stored.
    pub profile: String,
    /// The Google account email, if it could be resolved.
    pub email: Option<String>,
}

/// Default config path holding OAuth client credentials.
///
/// Why: Users without env vars set need a documented on-disk location, and it
/// mirrors the Python CLI's `~/.gworkspace-mcp/` home.
/// What: `~/.gworkspace-mcp/oauth_client.json`.
/// Test: `resolve_client_creds` covered via `parse_client_creds_json`.
fn client_creds_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gworkspace-mcp")
        .join("oauth_client.json")
}

/// Resolve OAuth client credentials: env vars first, then config file.
///
/// Why: Keeps parity with the existing refresh path (`OAuthManager::from_env`)
/// while giving users a persistent alternative to exporting secrets in every
/// shell.
/// What: Reads `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET`; if
/// either is missing, falls back to `~/.gworkspace-mcp/oauth_client.json`.
/// Test: `parse_client_creds_json` covers both accepted JSON shapes.
pub fn resolve_client_creds() -> Result<ClientCreds> {
    let id = std::env::var("GOOGLE_OAUTH_CLIENT_ID").ok();
    let secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET").ok();
    if let (Some(client_id), Some(client_secret)) = (id, secret) {
        return Ok(ClientCreds {
            client_id,
            client_secret,
        });
    }

    let path = client_creds_path();
    if path.exists() {
        let data =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        return parse_client_creds_json(&data).with_context(|| format!("parse {}", path.display()));
    }

    Err(anyhow!(
        "no OAuth client credentials found. Set GOOGLE_OAUTH_CLIENT_ID and \
         GOOGLE_OAUTH_CLIENT_SECRET, or write {} as \
         {{\"client_id\":\"...\",\"client_secret\":\"...\"}}",
        client_creds_path().display()
    ))
}

/// Parse client credentials JSON (flat or Google installed-app shape).
///
/// Why: Google's console downloads credentials as
/// `{"installed":{"client_id":...,"client_secret":...}}`; supporting that
/// verbatim lets users drop the file in unchanged, while a flat shape is
/// simpler to hand-write.
/// What: Accepts `{"client_id","client_secret"}` or the nested `installed`
/// (or `web`) object.
/// Test: `parses_flat_creds`, `parses_installed_creds`.
fn parse_client_creds_json(data: &str) -> Result<ClientCreds> {
    let v: serde_json::Value = serde_json::from_str(data).context("invalid client creds JSON")?;
    let obj = v.get("installed").or_else(|| v.get("web")).unwrap_or(&v);
    let client_id = obj
        .get("client_id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("missing client_id"))?;
    let client_secret = obj
        .get("client_secret")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("missing client_secret"))?;
    Ok(ClientCreds {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
    })
}

/// Assemble the space-delimited scope string for the authorization request.
///
/// Why: Google expects one `scope` param with space-separated values, and the
/// set must match [`OAUTH_SCOPES`] to stay token-compatible with Python.
/// What: Joins [`OAUTH_SCOPES`] with a single space.
/// Test: `scope_string_matches_constant_set`.
pub fn assemble_scope_string() -> String {
    OAUTH_SCOPES.join(" ")
}

/// Build the full authorization-endpoint URL for the browser to open.
///
/// Why: This is the URL the user visits to grant consent; every param
/// (client_id, scope, PKCE challenge, state, redirect_uri) must be present
/// and correctly encoded or Google rejects the request.
/// What: Appends the query params to [`OAUTH_AUTH_URL`], requesting
/// `access_type=offline` and `prompt=consent` so a refresh token is always
/// issued. Uses S256 for the code challenge.
/// Test: `build_auth_url_contains_all_params`.
pub fn build_auth_url(
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    challenge: &str,
    state: &str,
) -> String {
    let params = [
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", scope),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, pkce::percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{OAUTH_AUTH_URL}?{query}")
}

/// Google token-endpoint response for the authorization-code grant.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

/// Run the full interactive consent flow and persist the minted token.
///
/// Why: The single entry point the `setup` CLI calls — orchestrates the
/// browser consent, code capture, token exchange, email resolution, and
/// persistence so the caller only decides profile name / default-ness.
/// What: Generates PKCE + state, binds a loopback callback, opens the system
/// browser (falling back to printing the URL), waits for the redirect,
/// exchanges the code, resolves the email, and writes a wire-compatible
/// `StoredToken`.
/// Test: Deferred (needs a live Google client + browser). Constituent pure
/// helpers are unit-tested.
pub async fn run_consent(
    storage: &TokenStorage,
    profile: &str,
    set_default: bool,
) -> Result<ConsentOutcome> {
    let creds = resolve_client_creds()?;
    let pkce = Pkce::generate();
    let state = pkce::generate_state();

    let listener = callback::bind_loopback()?;
    let redirect_uri = callback::redirect_uri(&listener)?;
    let scope = assemble_scope_string();
    let auth_url = build_auth_url(
        &creds.client_id,
        &redirect_uri,
        &scope,
        &pkce.challenge,
        &state,
    );

    eprintln!("Opening your browser to authorize trusty-gworkspace...");
    eprintln!("If it does not open, visit this URL manually:\n\n{auth_url}\n");
    if let Err(e) = open::that(&auth_url) {
        warn!(error = %e, "failed to launch browser; user must open the URL manually");
    }

    // Block for the redirect off the async runtime so we don't stall the reactor.
    let expected_state = state.clone();
    let code =
        tokio::task::spawn_blocking(move || callback::wait_for_code(&listener, &expected_state))
            .await
            .context("callback task panicked")??;

    info!("authorization code received; exchanging for tokens");
    let http = reqwest::Client::new();
    let token = exchange_code(&http, &creds, &code, &pkce.verifier, &redirect_uri).await?;

    let email = resolve_email(&http, &token).await;
    let stored = build_stored_token(profile, email.clone(), &token, &scope);
    persist(storage, profile, stored, set_default)?;

    Ok(ConsentOutcome {
        profile: profile.to_string(),
        email,
    })
}

/// Exchange an authorization code for tokens (authorization_code grant).
async fn exchange_code(
    http: &reqwest::Client,
    creds: &ClientCreds,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    let params = [
        ("client_id", creds.client_id.as_str()),
        ("client_secret", creds.client_secret.as_str()),
        ("code", code),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri),
    ];
    let resp = http
        .post(OAUTH_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .context("POST token endpoint (authorization_code)")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("token exchange failed ({status}): {body}"));
    }
    serde_json::from_str(&body).with_context(|| format!("parse token response: {body}"))
}

/// Resolve the account email: `id_token` claim first, then userinfo endpoint.
async fn resolve_email(http: &reqwest::Client, token: &TokenResponse) -> Option<String> {
    if let Some(id_token) = &token.id_token
        && let Some(email) = pkce::email_from_id_token(id_token)
    {
        return Some(email);
    }
    let resp = http
        .get(USERINFO_URL)
        .bearer_auth(&token.access_token)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("email").and_then(|x| x.as_str()).map(str::to_string)
}

/// Build a wire-compatible `StoredToken` from the token response.
fn build_stored_token(
    profile: &str,
    email: Option<String>,
    token: &TokenResponse,
    requested_scope: &str,
) -> StoredToken {
    let expires_in = token.expires_in.unwrap_or(3600);
    let scopes: Vec<String> = token
        .scope
        .as_deref()
        .unwrap_or(requested_scope)
        .split_whitespace()
        .map(String::from)
        .collect();
    let now = Utc::now();
    StoredToken {
        version: 1,
        metadata: TokenMetadata {
            service_name: profile.to_string(),
            provider: "google".to_string(),
            created_at: now,
            last_refreshed: None,
            email,
            is_default: false,
        },
        token: OAuthToken {
            access_token: token.access_token.clone(),
            refresh_token: token.refresh_token.clone(),
            expires_at: now + Duration::seconds(expires_in),
            scopes,
            token_type: token.token_type.clone().unwrap_or_else(|| "Bearer".into()),
        },
    }
}

/// Persist a freshly-minted token, optionally marking it the default profile.
///
/// Why: `setup` must not clobber other profiles and must keep exactly one
/// default when asked, matching Python multi-profile semantics.
/// What: Loads the existing map, unsets other defaults when `set_default`,
/// inserts the new entry, and saves.
/// Test: `persist_marks_single_default` via a temp storage.
fn persist(
    storage: &TokenStorage,
    profile: &str,
    mut stored: StoredToken,
    set_default: bool,
) -> Result<()> {
    let mut all: HashMap<String, StoredToken> = storage.load()?;
    if set_default {
        for entry in all.values_mut() {
            entry.metadata.is_default = false;
        }
        stored.metadata.is_default = true;
    }
    all.insert(profile.to_string(), stored);
    storage.save(&all)?;
    Ok(())
}

/// Effective profile name: caller override, else the shared default.
///
/// Why: `setup` defaults to the canonical `gworkspace-mcp` profile so a
/// first-time user gets a working default without thinking about names.
/// What: Returns the trimmed override if non-empty, else [`DEFAULT_PROFILE`].
/// Test: `default_profile_falls_back`.
pub fn effective_profile(override_name: Option<&str>) -> String {
    match override_name.map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => DEFAULT_PROFILE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_string_matches_constant_set() {
        let s = assemble_scope_string();
        assert!(s.starts_with("openid "));
        assert!(s.contains("https://www.googleapis.com/auth/gmail.modify"));
        assert!(s.contains("https://www.googleapis.com/auth/presentations"));
        assert_eq!(s.split(' ').count(), OAUTH_SCOPES.len());
    }

    #[test]
    fn build_auth_url_contains_all_params() {
        let url = build_auth_url(
            "cid.apps.googleusercontent.com",
            "http://127.0.0.1:5000",
            "openid https://www.googleapis.com/auth/calendar",
            "CHALLENGE",
            "STATE",
        );
        assert!(url.starts_with(OAUTH_AUTH_URL));
        assert!(url.contains("client_id=cid.apps.googleusercontent.com"));
        assert!(url.contains("code_challenge=CHALLENGE"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=STATE"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        // redirect_uri and scope must be percent-encoded.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"));
        assert!(url.contains("scope=openid%20https"));
    }

    #[test]
    fn parses_flat_creds() {
        let c = parse_client_creds_json(r#"{"client_id":"a","client_secret":"b"}"#).unwrap();
        assert_eq!(c.client_id, "a");
        assert_eq!(c.client_secret, "b");
    }

    #[test]
    fn parses_installed_creds() {
        let c = parse_client_creds_json(
            r#"{"installed":{"client_id":"x","client_secret":"y","redirect_uris":["http://localhost"]}}"#,
        )
        .unwrap();
        assert_eq!(c.client_id, "x");
        assert_eq!(c.client_secret, "y");
    }

    #[test]
    fn default_profile_falls_back() {
        assert_eq!(effective_profile(None), DEFAULT_PROFILE);
        assert_eq!(effective_profile(Some("  ")), DEFAULT_PROFILE);
        assert_eq!(effective_profile(Some("work")), "work");
    }

    #[test]
    fn persist_marks_single_default() {
        let dir = std::env::temp_dir().join(format!("gw-persist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = TokenStorage::with_path(dir.join("tokens.json"));

        let make = |name: &str| StoredToken {
            version: 1,
            metadata: TokenMetadata {
                service_name: name.to_string(),
                provider: "google".into(),
                created_at: Utc::now(),
                last_refreshed: None,
                email: Some(format!("{name}@example.com")),
                is_default: false,
            },
            token: OAuthToken {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at: Utc::now() + Duration::seconds(3600),
                scopes: vec!["openid".into()],
                token_type: "Bearer".into(),
            },
        };

        persist(&storage, "first", make("first"), true).unwrap();
        persist(&storage, "second", make("second"), true).unwrap();

        let all = storage.load().unwrap();
        assert_eq!(all.len(), 2);
        assert!(!all["first"].metadata.is_default, "old default cleared");
        assert!(all["second"].metadata.is_default, "new entry is default");
    }
}
