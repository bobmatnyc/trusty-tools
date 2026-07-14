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
    /// Whether this profile was (or remained) the default after this run.
    pub default_applied: bool,
}

/// How `run_consent` should decide whether the new profile becomes default.
///
/// Why: A prior version unconditionally set `is_default=true` on every
/// `setup` call unless `--no-default` was passed, so authorizing a *second*
/// account silently stole the default from the first — swapping which
/// Google account MCP write-tools act against with no warning. This enum
/// makes the three legitimate intents explicit instead of collapsing them
/// into one bool.
/// What: `Auto` (the CLI default) only sets default when no profile is
/// currently default, or when re-authorizing the profile that already is;
/// `Force` (`--make-default`) always sets default, printing a warning if it
/// displaces a different existing default; `Never` (`--no-default`) never
/// changes the default.
/// Test: `auto_sets_default_only_when_absent`,
/// `auto_keeps_default_on_reauth_of_default_profile`,
/// `auto_does_not_steal_existing_default`, `force_overrides_existing_default`,
/// `never_keeps_existing_default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultMode {
    /// Set default only if none exists yet (or this profile already is it).
    Auto,
    /// Always set default, displacing any existing one.
    Force,
    /// Never change the default.
    Never,
}

/// Decide whether `profile` should become the default given `mode` and the
/// currently stored profiles.
///
/// Why: Isolating this as a pure function (no I/O) makes the "don't silently
/// steal the default" contract directly unit-testable without a temp
/// filesystem or network access.
/// What: See [`DefaultMode`] variant docs for the exact semantics per mode.
/// Test: see [`DefaultMode`] doc.
pub fn should_set_default(
    mode: DefaultMode,
    profile: &str,
    existing: &HashMap<String, StoredToken>,
) -> bool {
    match mode {
        DefaultMode::Never => false,
        DefaultMode::Force => true,
        DefaultMode::Auto => {
            let already_default_here = existing
                .get(profile)
                .map(|s| s.metadata.is_default)
                .unwrap_or(false);
            let any_default = existing.values().any(|s| s.metadata.is_default);
            already_default_here || !any_default
        }
    }
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

/// Wall-clock limit for the user to complete browser consent before the
/// local callback listener gives up.
///
/// Why: An unbounded `accept()` would hang the whole flow forever if the
/// browser never opens, the tab is closed, or the machine sleeps — with no
/// way to recover short of killing the process.
/// What: 5 minutes, matching typical "this link expires soon" UX elsewhere.
/// Test: Exercised via `callback::wait_for_code_with_timeout`'s own test with
/// a much shorter duration.
const CONSENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Run the full interactive consent flow and persist the minted token.
///
/// Why: The single entry point the `setup` CLI calls — orchestrates the
/// browser consent, code capture, token exchange, email resolution, and
/// persistence so the caller only decides profile name / default mode.
/// What: Generates PKCE + state, binds a loopback callback, opens the system
/// browser (falling back to printing the URL), waits (bounded by
/// [`CONSENT_TIMEOUT`]) for the redirect, exchanges the code, resolves the
/// email, and writes a wire-compatible `StoredToken`. The default-profile
/// decision is resolved up front (from on-disk state, before any network
/// call) so a would-be-displaced default can be warned about immediately.
/// Test: Deferred (needs a live Google client + browser). Constituent pure
/// helpers are unit-tested.
pub async fn run_consent(
    storage: &TokenStorage,
    profile: &str,
    default_mode: DefaultMode,
) -> Result<ConsentOutcome> {
    let creds = resolve_client_creds()?;

    let existing = storage.load()?;
    let set_default = should_set_default(default_mode, profile, &existing);
    if set_default
        && let Some((_, displaced)) = existing
            .iter()
            .find(|(name, s)| s.metadata.is_default && name.as_str() != profile)
    {
        let who = displaced
            .metadata
            .email
            .clone()
            .unwrap_or_else(|| displaced.metadata.service_name.clone());
        eprintln!("Warning: this replaces '{who}' as the default profile.");
    }

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
    eprintln!(
        "Waiting up to {}s for you to finish in the browser...",
        CONSENT_TIMEOUT.as_secs()
    );
    if let Err(e) = open::that(&auth_url) {
        warn!(error = %e, "failed to launch browser; user must open the URL manually");
    }

    // Block for the redirect off the async runtime so we don't stall the reactor.
    let expected_state = state.clone();
    let code = tokio::task::spawn_blocking(move || {
        callback::wait_for_code_with_timeout(&listener, &expected_state, CONSENT_TIMEOUT)
    })
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
        default_applied: set_default,
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
        return Err(anyhow!(
            "token exchange failed: {}",
            sanitize_oauth_error(status, &body)
        ));
    }
    serde_json::from_str(&body).with_context(|| format!("parse token response: {body}"))
}

/// Google's RFC 6749 §5.2 error-response shape (`error` + optional
/// `error_description`).
#[derive(Debug, Deserialize)]
struct OAuthErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Maximum characters of an unparseable error body to surface.
const MAX_ERROR_BODY_CHARS: usize = 200;

/// Extract a safe, bounded error message from a Google OAuth error response.
///
/// Why: The raw response body can be arbitrary third-party text (an HTML
/// error page, a verbose diagnostic dump) that ends up inside an `anyhow`
/// error chain and may be printed to the terminal or captured in crash
/// reports. Surfacing only the structured `error`/`error_description` (or a
/// short truncated fallback) keeps that surface bounded and predictable.
/// What: Parses `body` as `{"error": "...", "error_description": "..."}`;
/// falls back to a message truncated to [`MAX_ERROR_BODY_CHARS`] characters
/// when the body isn't that shape.
/// Test: `sanitizes_oauth_error_json`, `truncates_unparseable_body`.
fn sanitize_oauth_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<OAuthErrorBody>(body) {
        return match parsed.error_description {
            Some(desc) => format!("{status} {}: {desc}", parsed.error),
            None => format!("{status} {}", parsed.error),
        };
    }
    let char_count = body.chars().count();
    let truncated: String = body.chars().take(MAX_ERROR_BODY_CHARS).collect();
    if char_count > MAX_ERROR_BODY_CHARS {
        format!("{status} (unparseable error body, truncated): {truncated}...")
    } else {
        format!("{status} (unparseable error body): {truncated}")
    }
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

    fn make_stored(name: &str, is_default: bool) -> StoredToken {
        StoredToken {
            version: 1,
            metadata: TokenMetadata {
                service_name: name.to_string(),
                provider: "google".into(),
                created_at: Utc::now(),
                last_refreshed: None,
                email: Some(format!("{name}@example.com")),
                is_default,
            },
            token: OAuthToken {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at: Utc::now() + Duration::seconds(3600),
                scopes: vec!["openid".into()],
                token_type: "Bearer".into(),
            },
        }
    }

    #[test]
    fn persist_marks_single_default() {
        let dir = std::env::temp_dir().join(format!("gw-persist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = TokenStorage::with_path(dir.join("tokens.json"));

        persist(&storage, "first", make_stored("first", false), true).unwrap();
        persist(&storage, "second", make_stored("second", false), true).unwrap();

        let all = storage.load().unwrap();
        assert_eq!(all.len(), 2);
        assert!(!all["first"].metadata.is_default, "old default cleared");
        assert!(all["second"].metadata.is_default, "new entry is default");
    }

    #[test]
    fn persist_false_does_not_steal_existing_default() {
        // Regression test: a second `setup` run with set_default=false (the
        // outcome of DefaultMode::Auto when a default already exists on a
        // different profile) must leave the existing default untouched.
        let dir = std::env::temp_dir().join(format!("gw-persist2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = TokenStorage::with_path(dir.join("tokens.json"));

        persist(&storage, "first", make_stored("first", false), true).unwrap();
        persist(&storage, "second", make_stored("second", false), false).unwrap();

        let all = storage.load().unwrap();
        assert_eq!(all.len(), 2);
        assert!(
            all["first"].metadata.is_default,
            "first must remain default — it was not stolen"
        );
        assert!(!all["second"].metadata.is_default);
    }

    #[test]
    fn auto_sets_default_only_when_absent() {
        let existing: HashMap<String, StoredToken> = HashMap::new();
        assert!(should_set_default(DefaultMode::Auto, "first", &existing));
    }

    #[test]
    fn auto_does_not_steal_existing_default() {
        let mut existing = HashMap::new();
        existing.insert("first".to_string(), make_stored("first", true));
        assert!(
            !should_set_default(DefaultMode::Auto, "second", &existing),
            "second account must not silently steal the default"
        );
    }

    #[test]
    fn auto_keeps_default_on_reauth_of_default_profile() {
        let mut existing = HashMap::new();
        existing.insert("first".to_string(), make_stored("first", true));
        assert!(
            should_set_default(DefaultMode::Auto, "first", &existing),
            "re-authorizing the existing default profile should keep it default"
        );
    }

    #[test]
    fn force_overrides_existing_default() {
        let mut existing = HashMap::new();
        existing.insert("first".to_string(), make_stored("first", true));
        assert!(should_set_default(DefaultMode::Force, "second", &existing));
    }

    #[test]
    fn never_keeps_existing_default() {
        let existing: HashMap<String, StoredToken> = HashMap::new();
        assert!(!should_set_default(DefaultMode::Never, "first", &existing));
        let mut with_default = HashMap::new();
        with_default.insert("first".to_string(), make_stored("first", true));
        assert!(!should_set_default(
            DefaultMode::Never,
            "second",
            &with_default
        ));
    }

    #[test]
    fn sanitizes_oauth_error_json() {
        let msg = sanitize_oauth_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant","error_description":"Bad Request"}"#,
        );
        assert!(msg.contains("invalid_grant"));
        assert!(msg.contains("Bad Request"));
    }

    #[test]
    fn sanitizes_oauth_error_json_without_description() {
        let msg = sanitize_oauth_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant"}"#,
        );
        assert!(msg.contains("invalid_grant"));
    }

    #[test]
    fn truncates_unparseable_body() {
        let long_body = "x".repeat(500);
        let msg = sanitize_oauth_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, &long_body);
        assert!(
            msg.len() < long_body.len(),
            "message must be shorter than the raw body"
        );
        assert!(msg.contains("truncated"));
    }
}
