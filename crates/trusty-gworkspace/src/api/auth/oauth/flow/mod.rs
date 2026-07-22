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
use super::errors::{redact_token_response, sanitize_oauth_error};
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
/// simpler to hand-write. `pub(super)` so `oauth::client_store` (issue #3518)
/// can validate/parse a per-profile client file with the identical accepted
/// shapes instead of re-implementing this.
/// What: Accepts `{"client_id","client_secret"}` or the nested `installed`
/// (or `web`) object.
/// Test: `parses_flat_creds`, `parses_installed_creds`.
pub(super) fn parse_client_creds_json(data: &str) -> Result<ClientCreds> {
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

/// Compose the user-facing consent prompt lines (printed to stderr).
///
/// Why: `setup` runs both interactively (auto-open the browser) and in
/// headless / in-session contexts (`--print-url` / `--no-browser`, where no
/// browser can be launched). Both modes must display the SAME consent URL — the
/// only thing that changes is whether we also spawn the browser and how we word
/// the instruction. Isolating the wording here (a) keeps the URL provably
/// identical across modes and (b) makes that contract unit-testable without a
/// live network round-trip.
/// What: Returns the ordered prompt lines. When `open_browser` is true the
/// first line announces the launch; otherwise it instructs the user to open the
/// URL manually. `auth_url` is embedded verbatim in every mode.
/// Test: `consent_prompt_url_is_identical_regardless_of_browser_mode`,
/// `consent_prompt_wording_differs_by_mode`.
fn consent_prompt_lines(auth_url: &str, open_browser: bool, timeout_secs: u64) -> Vec<String> {
    let mut lines = Vec::new();
    if open_browser {
        lines.push("Opening your browser to authorize trusty-gworkspace...".to_string());
        lines.push(format!(
            "If it does not open, open this URL manually:\n\n{auth_url}\n"
        ));
    } else {
        lines.push(
            "Open this URL in a browser to authorize trusty-gworkspace \
             (no browser will be launched):"
                .to_string(),
        );
        lines.push(format!("\n{auth_url}\n"));
    }
    lines.push(format!(
        "Waiting up to {timeout_secs}s for you to finish in the browser..."
    ));
    lines
}

/// Run the full interactive consent flow and persist the minted token.
///
/// Why: The single entry point the `setup` CLI calls — orchestrates the
/// browser consent, code capture, token exchange, email resolution, and
/// persistence so the caller only decides profile name / default mode.
/// What: Generates PKCE + state, binds a loopback callback, prints the consent
/// URL and — when `open_browser` is true — opens the system browser (passing
/// `false`, via `setup --print-url` / `--no-browser`, prints the URL only, for
/// headless / in-session re-auth). Waits (bounded by [`CONSENT_TIMEOUT`]) for
/// the redirect, exchanges the code, resolves the email, and writes a
/// wire-compatible `StoredToken`. The default-profile decision is resolved up
/// front (from on-disk state, before any network call) so a would-be-displaced
/// default can be warned about immediately.
/// Test: Deferred (needs a live Google client + browser). Constituent pure
/// helpers are unit-tested (`consent_prompt_url_is_identical_regardless_of_browser_mode`).
pub async fn run_consent(
    storage: &TokenStorage,
    profile: &str,
    default_mode: DefaultMode,
    open_browser: bool,
) -> Result<ConsentOutcome> {
    run_consent_with(
        storage,
        profile,
        default_mode,
        open_browser,
        CONSENT_TIMEOUT,
        |auth_url| {
            for line in consent_prompt_lines(auth_url, open_browser, CONSENT_TIMEOUT.as_secs()) {
                eprintln!("{line}");
            }
        },
    )
    .await
}

/// Run the consent flow with an explicit timeout and a caller-supplied hook
/// for the consent URL, instead of the CLI's hardcoded stderr print.
///
/// Why: `setup` prints the URL to stderr and blocks up to
/// [`CONSENT_TIMEOUT`] (5 minutes) — fine for an interactive terminal, but
/// the `add_account` MCP tool (issue #3503) needs the URL back in its OWN
/// tool response (the calling agent cannot see this process's stderr) and a
/// much shorter bound (an MCP client may itself time out a single tool call
/// long before 5 minutes). Splitting the URL hand-off into a callback lets
/// both entry points share every byte of the PKCE / token-exchange /
/// persistence logic — no OAuth reimplementation.
/// What: Identical to [`run_consent`] except `timeout` replaces
/// `CONSENT_TIMEOUT` for the browser-redirect wait, and `on_auth_url` runs
/// synchronously with the built URL (before the browser is opened) instead
/// of the hardcoded print. [`run_consent`] is just this function called with
/// the default timeout and its stderr-printing closure.
/// Test: Live browser round-trip stays deferred for both entry points; the
/// pure helpers (`build_auth_url`, `consent_prompt_lines`, `should_set_default`)
/// this calls are unit-tested.
pub async fn run_consent_with(
    storage: &TokenStorage,
    profile: &str,
    default_mode: DefaultMode,
    open_browser: bool,
    timeout: std::time::Duration,
    on_auth_url: impl FnOnce(&str),
) -> Result<ConsentOutcome> {
    // Issue #3518: resolve THIS profile's own client (persisted via
    // `setup --oauth-client` / `add_account`'s `oauth_client_path`) if it has
    // one, falling back to the global client otherwise. The minted refresh
    // token is bound to whichever client authorizes it, so authorization and
    // every later refresh (`OAuthManager::refresh`) MUST agree on the same
    // client for a given profile — this is the single call site that decides
    // it for authorization.
    let creds = super::client_store::resolve_client_creds_for_profile(profile)?;

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

    on_auth_url(&auth_url);
    if open_browser && let Err(e) = open::that(&auth_url) {
        warn!(error = %e, "failed to launch browser; user must open the URL manually");
    }

    // Block for the redirect off the async runtime so we don't stall the reactor.
    let expected_state = state.clone();
    let code = tokio::task::spawn_blocking(move || {
        callback::wait_for_code_with_timeout(&listener, &expected_state, timeout)
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
    // NB: `body` here is a 2xx token response containing access/refresh tokens;
    // on the (practically unreachable) parse failure it must be redacted, never
    // embedded verbatim, before entering the error chain.
    serde_json::from_str(&body)
        .with_context(|| format!("parse token response: {}", redact_token_response(&body)))
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
/// default when asked, matching Python multi-profile semantics. Routed
/// through `TokenStorage::update` (#3502) so a concurrent write (e.g. another
/// profile refreshing at the same moment) can't lose this one.
/// What: Reloads the map under the shared lock, unsets other defaults when
/// `set_default`, inserts the new entry, and saves.
/// Test: `persist_marks_single_default` via a temp storage.
fn persist(
    storage: &TokenStorage,
    profile: &str,
    mut stored: StoredToken,
    set_default: bool,
) -> Result<()> {
    storage.update(|all| {
        if set_default {
            for entry in all.values_mut() {
                entry.metadata.is_default = false;
            }
            stored.metadata.is_default = true;
        }
        all.insert(profile.to_string(), stored);
        Ok(())
    })
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
mod tests;
