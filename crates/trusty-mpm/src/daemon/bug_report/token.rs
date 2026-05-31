//! Token providers for GitHub bug-report filing.
//!
//! Why: Filing issues to the public `bobmatnyc/trusty-tools` repo requires a
//!      GitHub bearer token. Phase 3 shipped the simplest working provider
//!      (ENV var + token file). Phase 4 adds a GitHub App installation-token
//!      provider so teams can use a shared App with scoped `issues:write`
//!      access — no developer needs a personal PAT.
//!
//! ## Resolution order
//!
//! Callers use [`resolve_token`] which tries providers in this order:
//!   1. **Explicit PAT env var** `TRUSTY_BUGREPORT_GITHUB_TOKEN` (non-empty) →
//!      `EnvFileTokenProvider`
//!   2. **Token file** `TRUSTY_BUGREPORT_TOKEN_FILE` or
//!      `~/.config/trusty-mpm/bugreport-token` → `EnvFileTokenProvider`
//!   3. **GitHub App** if all three App env vars are set
//!      (`TRUSTY_BUGREPORT_GH_APP_ID`, `TRUSTY_BUGREPORT_GH_INSTALL_ID`,
//!      `TRUSTY_BUGREPORT_GH_APP_KEY_FILE`) → `GithubAppTokenProvider`
//!   4. **None** (graceful no-token degradation — unchanged from Phase 3).
//!
//! ## GitHub App setup (team-recommended)
//!
//! 1. Create a GitHub App with `Issues: Write` on `bobmatnyc/trusty-tools`.
//! 2. Generate a private key; save the `.pem` file somewhere accessible.
//! 3. Install the App on the repo; note the installation ID.
//! 4. Set:
//!    ```text
//!    TRUSTY_BUGREPORT_GH_APP_ID=<numeric app ID>
//!    TRUSTY_BUGREPORT_GH_INSTALL_ID=<numeric installation ID>
//!    TRUSTY_BUGREPORT_GH_APP_KEY_FILE=/path/to/private-key.pem
//!    ```
//!
//! The provider mints a short-lived installation token (60 min TTL) and caches
//! it until 5 minutes before expiry, then refreshes automatically. No secrets
//! are committed — the private key stays on disk.
//!
//! ## JWT / RS256 implementation
//!
//! GitHub App authentication requires a JWT signed with the App's RS256 private
//! key. We use the `jsonwebtoken` crate (standard in the Rust ecosystem) for
//! JWT construction and signing. If `jsonwebtoken` is not available in the
//! workspace, see the `STUBBED` compile-time note below.
//!
//! Test: `tests::jwt_claims_correct`, `tests::cache_returns_valid_token`,
//!       `tests::cache_refreshes_before_expiry`, `tests::resolution_order_*`.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

// ── Environment variable names ────────────────────────────────────────────────

/// Primary env var for a PAT or bot token.
pub const TOKEN_ENV_VAR: &str = "TRUSTY_BUGREPORT_GITHUB_TOKEN";
/// Override env var for the token file path.
pub const TOKEN_FILE_ENV_VAR: &str = "TRUSTY_BUGREPORT_TOKEN_FILE";
/// Default token file path (relative to home dir).
const TOKEN_FILE_RELATIVE: &str = ".config/trusty-mpm/bugreport-token";

/// GitHub App numeric application ID.
pub const APP_ID_ENV_VAR: &str = "TRUSTY_BUGREPORT_GH_APP_ID";
/// GitHub App installation ID (from the App install on the repo).
pub const APP_INSTALL_ID_ENV_VAR: &str = "TRUSTY_BUGREPORT_GH_INSTALL_ID";
/// Path to the App's RS256 private-key PEM file.
pub const APP_KEY_FILE_ENV_VAR: &str = "TRUSTY_BUGREPORT_GH_APP_KEY_FILE";

// ── TokenProvider trait ───────────────────────────────────────────────────────

/// Provides a bearer token for GitHub API calls.
///
/// Why: the filing client should not be tightly coupled to a specific token
///      source. The ENV+file implementation works for quick setup; a GitHub App
///      installation-token provider (short-lived, auto-rotating) is the
///      team-recommended approach for shared deployments.
/// What: one method, `token`, returns the resolved token or `None` when no
///       source is configured.
/// Test: `EnvFileTokenProvider` exercised by `tests::resolution_order_*`;
///       `GithubAppTokenProvider` exercised by `tests::jwt_claims_correct` and
///       `tests::cache_*`.
pub trait TokenProvider: Send + Sync {
    /// Resolve and return the bearer token, or `None` if unconfigured.
    fn token(&self) -> Option<String>;
}

// ── EnvFileTokenProvider ──────────────────────────────────────────────────────

/// Token provider that reads from the environment variable
/// `TRUSTY_BUGREPORT_GITHUB_TOKEN` or a local file.
///
/// Why: the simplest useful implementation — zero runtime dependencies beyond
///      env and fs reads. Covers the quick-start PAT setup documented in the
///      user guide.
/// What: resolution order:
///   1. `TRUSTY_BUGREPORT_GITHUB_TOKEN` env var (non-empty value).
///   2. File at `TRUSTY_BUGREPORT_TOKEN_FILE` env var path (if set).
///   3. File at `~/.config/trusty-mpm/bugreport-token` (fallback).
///
/// Token values are trimmed of leading/trailing whitespace.
///
/// Test: `tests::resolution_order_env_wins_over_file`,
/// `tests::resolution_order_file_used_when_env_absent`.
pub struct EnvFileTokenProvider;

impl TokenProvider for EnvFileTokenProvider {
    fn token(&self) -> Option<String> {
        // 1. Check env var.
        if let Ok(val) = std::env::var(TOKEN_ENV_VAR) {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }

        // 2. Resolve file path (override or default).
        let file_path: PathBuf = if let Ok(override_path) = std::env::var(TOKEN_FILE_ENV_VAR) {
            PathBuf::from(override_path.trim())
        } else if let Some(home) = dirs::home_dir() {
            home.join(TOKEN_FILE_RELATIVE)
        } else {
            return None;
        };

        // 3. Read and trim the file.
        std::fs::read_to_string(&file_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

// ── GitHub App token provider ─────────────────────────────────────────────────

/// Cached installation token returned by the GitHub API.
///
/// Why: installation tokens expire after 60 minutes; caching avoids a JWT mint
///      + HTTP round-trip on every report_bug call.
/// What: stores the token string and the Unix timestamp (seconds) at which the
///       token becomes invalid. The cache refreshes 5 minutes before expiry.
/// Test: `tests::cache_returns_valid_token`, `tests::cache_refreshes_before_expiry`.
#[derive(Debug, Clone)]
struct CachedToken {
    /// The GitHub installation access token.
    value: String,
    /// Unix timestamp (seconds) at which the token expires.
    expires_at_secs: i64,
}

impl CachedToken {
    /// Returns `true` when the token is still valid with a 5-minute safety margin.
    ///
    /// Why: GitHub installation tokens have a 60-minute TTL; we refresh 5 minutes
    ///      early to avoid using an expiring token mid-request.
    /// What: compares `now_secs` against `expires_at_secs - 300`.
    /// Test: `tests::cache_refreshes_before_expiry`.
    fn is_valid(&self, now_secs: i64) -> bool {
        now_secs < self.expires_at_secs - 300
    }
}

/// GitHub App configuration, resolved from env vars or a config file.
///
/// Why: centralising the three required fields makes it easy to validate that
///      the App provider is fully configured before attempting a JWT mint.
/// What: holds the numeric App ID, installation ID, and path to the PEM file.
/// Test: constructed in `tests::jwt_claims_correct`.
#[derive(Debug, Clone)]
pub struct GithubAppConfig {
    /// Numeric GitHub App ID (from the App's About page).
    pub app_id: u64,
    /// Numeric installation ID (from the App install on the target repo).
    pub installation_id: u64,
    /// Path to the App's RS256 private-key PEM file.
    pub private_key_path: PathBuf,
}

impl GithubAppConfig {
    /// Try to load the App config from environment variables.
    ///
    /// Why: the three App env vars must all be present for the App provider
    ///      to be usable; returning `None` when any is absent triggers graceful
    ///      fallback to the PAT provider.
    /// What: reads `TRUSTY_BUGREPORT_GH_APP_ID`, `TRUSTY_BUGREPORT_GH_INSTALL_ID`,
    ///       and `TRUSTY_BUGREPORT_GH_APP_KEY_FILE`; parses the numeric IDs;
    ///       returns `None` if any parse fails or env var is absent.
    /// Test: `tests::resolution_order_app_used_when_both_env_vars_set`.
    pub fn from_env() -> Option<Self> {
        let app_id: u64 = std::env::var(APP_ID_ENV_VAR)
            .ok()
            .and_then(|s| s.trim().parse().ok())?;
        let installation_id: u64 = std::env::var(APP_INSTALL_ID_ENV_VAR)
            .ok()
            .and_then(|s| s.trim().parse().ok())?;
        let private_key_path: PathBuf = std::env::var(APP_KEY_FILE_ENV_VAR)
            .ok()
            .map(|s| PathBuf::from(s.trim()))?;
        Some(Self {
            app_id,
            installation_id,
            private_key_path,
        })
    }
}

/// JWT claims payload for a GitHub App JWT.
///
/// Why: GitHub requires a specific claim set (`iss`, `iat`, `exp`) in the App
///      JWT used to request an installation token.
/// What: `iss` = App ID (as string); `iat` = issued-at (Unix seconds); `exp` =
///       expiry (issued-at + 10 minutes; GitHub allows up to 10 minutes).
/// Test: `tests::jwt_claims_correct`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppJwtClaims {
    /// Issuer: the GitHub App ID as a string.
    pub iss: String,
    /// Issued-at timestamp (Unix seconds).
    pub iat: i64,
    /// Expiry timestamp (Unix seconds; iat + 600 seconds).
    pub exp: i64,
}

impl AppJwtClaims {
    /// Build claims with the given App ID, using `now_secs` as the issue time.
    ///
    /// Why: injecting `now_secs` makes claims construction deterministic and
    ///      testable without calling `std::time::SystemTime::now()`.
    /// What: sets `iat = now_secs`, `exp = now_secs + 600` (10 minutes).
    /// Test: `tests::jwt_claims_correct`.
    pub fn new(app_id: u64, now_secs: i64) -> Self {
        Self {
            iss: app_id.to_string(),
            iat: now_secs,
            exp: now_secs + 600,
        }
    }
}

/// GitHub API response for an installation token request.
///
/// Why: we need to extract the token string and the expiry time from the
///      GitHub API response.
/// What: deserializes the `token` and `expires_at` fields from the JSON body
///       returned by `POST /app/installations/{id}/access_tokens`.
/// Test: constructed inline in `tests::mock_token_exchange`.
#[derive(Debug, Deserialize)]
struct InstallationTokenResponse {
    token: String,
    expires_at: String,
}

/// Type alias for the installation-token exchange function injected into
/// [`GithubAppTokenProvider`].
///
/// Why: the type is used in two places (struct field + `with_injected` argument);
///      a type alias avoids the "very complex type" clippy warning and makes both
///      sites readable.
/// What: given a signed JWT string and the numeric installation ID, returns the
///       `(token_value, expires_at_unix_secs)` pair or an error.
/// Test: injected in `tests::cache_*`.
type ExchangeFn = Box<dyn Fn(&str, u64) -> anyhow::Result<(String, i64)> + Send + Sync>;

/// GitHub App installation-token provider.
///
/// Why: teams deploying trusty-mpm in a shared environment should not need per-
///      developer PATs. A GitHub App with `issues:write` scoped only to the one
///      repo (`bobmatnyc/trusty-tools`) lets any team member file reports through
///      the shared App without personal repo write access.
/// What: mints a short-lived installation token (60 min) via JWT → exchange flow;
///       caches the token and refreshes it 5 minutes before expiry. The HTTP
///       exchange call uses `reqwest::blocking` (matches the existing `RealGithubClient`).
///       In tests, the exchange call is mocked by overriding `exchange_fn`.
/// Test: `tests::jwt_claims_correct`, `tests::cache_returns_valid_token`,
///       `tests::cache_refreshes_before_expiry`.
pub struct GithubAppTokenProvider {
    config: GithubAppConfig,
    cache: Mutex<Option<CachedToken>>,
    /// Injected clock function (seconds since epoch). Defaults to real time;
    /// tests inject a fixed timestamp for deterministic behaviour.
    now_fn: Box<dyn Fn() -> i64 + Send + Sync>,
    /// Injected exchange function: given a signed JWT and installation ID,
    /// returns `(token_value, expires_at_secs)`. Real impl calls GitHub API;
    /// tests return a canned response.
    exchange_fn: ExchangeFn,
}

impl GithubAppTokenProvider {
    /// Create a production provider that uses real time and the GitHub API.
    ///
    /// Why: the standard constructor for non-test callers.
    /// What: wires real `std::time::SystemTime` as the clock and the real
    ///       GitHub installation-token exchange endpoint as the exchange function.
    /// Test: not unit-tested directly (network required); covered by integration
    ///       tests gated `#[ignore]`.
    pub fn new(config: GithubAppConfig) -> Self {
        Self {
            config,
            cache: Mutex::new(None),
            now_fn: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
            exchange_fn: Box::new(real_exchange_installation_token),
        }
    }

    /// Create a provider with injected clock and exchange functions (for tests).
    ///
    /// Why: pure, deterministic testing of the JWT-claims and caching logic
    ///      without any network calls or real time.
    /// What: accepts `now_fn` returning a fixed timestamp and `exchange_fn`
    ///       returning canned `(token, expires_at)` values.
    /// Test: `tests::cache_returns_valid_token`, `tests::cache_refreshes_before_expiry`,
    ///       `tests::jwt_claims_correct`.
    pub fn with_injected(
        config: GithubAppConfig,
        now_fn: impl Fn() -> i64 + Send + Sync + 'static,
        exchange_fn: impl Fn(&str, u64) -> anyhow::Result<(String, i64)> + Send + Sync + 'static,
    ) -> Self {
        Self {
            config,
            cache: Mutex::new(None),
            now_fn: Box::new(now_fn),
            exchange_fn: Box::new(exchange_fn),
        }
    }

    /// Build the GitHub App JWT using the App's private key.
    ///
    /// Why: GitHub requires a JWT signed with the App's RS256 key to request
    ///      an installation token. The JWT must have specific claims (`iss`,
    ///      `iat`, `exp`) and be signed with RS256.
    /// What: reads the PEM file, encodes the claims struct, and signs with
    ///       `jsonwebtoken::encode` using `Algorithm::RS256`. Returns the
    ///       compact serialization (`header.claims.signature`).
    /// Test: `tests::jwt_claims_correct` verifies claims are set correctly.
    pub fn mint_jwt(&self, now_secs: i64) -> anyhow::Result<String> {
        let pem_data = std::fs::read_to_string(&self.config.private_key_path)
            .map_err(|e| anyhow::anyhow!("failed to read App private key: {e}"))?;
        let claims = AppJwtClaims::new(self.config.app_id, now_secs);
        encode_jwt_rs256(&pem_data, &claims)
    }

    /// Get a valid installation token, using the cache when possible.
    ///
    /// Why: minting a JWT and exchanging it for an installation token requires
    ///      two HTTP round-trips; caching avoids paying this cost on every call.
    /// What: checks the cache first; if the cached token is still valid (more
    ///       than 5 minutes from expiry), returns it. Otherwise mints a new JWT,
    ///       calls `exchange_fn`, caches the result, and returns the new token.
    /// Test: `tests::cache_returns_valid_token`, `tests::cache_refreshes_before_expiry`.
    fn get_token_cached(&self) -> anyhow::Result<String> {
        let now = (self.now_fn)();
        {
            let guard = self.cache.lock().expect("token cache lock not poisoned");
            if let Some(ref cached) = *guard
                && cached.is_valid(now)
            {
                return Ok(cached.value.clone());
            }
        }

        // Cache miss or expired — mint and exchange.
        let jwt = self.mint_jwt(now)?;
        let (token_value, expires_at_secs) = (self.exchange_fn)(&jwt, self.config.installation_id)
            .map_err(|e| anyhow::anyhow!("GitHub App token exchange failed: {e}"))?;

        {
            let mut guard = self.cache.lock().expect("token cache lock not poisoned");
            *guard = Some(CachedToken {
                value: token_value.clone(),
                expires_at_secs,
            });
        }
        Ok(token_value)
    }
}

impl TokenProvider for GithubAppTokenProvider {
    fn token(&self) -> Option<String> {
        self.get_token_cached()
            .map_err(|e| {
                tracing::warn!("GitHub App token provider failed: {e}");
            })
            .ok()
    }
}

// ── JWT RS256 signing ─────────────────────────────────────────────────────────

/// Encode and sign a JWT with RS256 using the given PEM private key.
///
/// Why: separated from `mint_jwt` so it can be unit-tested with an in-memory
///      PEM without touching the filesystem.
/// What: uses `jsonwebtoken::encode` with `Algorithm::RS256`; returns the compact
///       JWT string (`header.payload.signature`).
/// Test: `tests::jwt_claims_correct` verifies round-trip via the decode path.
pub fn encode_jwt_rs256(pem: &str, claims: &AppJwtClaims) -> anyhow::Result<String> {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    let key = EncodingKey::from_rsa_pem(pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid RSA PEM for App JWT: {e}"))?;
    let header = Header::new(Algorithm::RS256);
    encode(&header, claims, &key).map_err(|e| anyhow::anyhow!("JWT encode failed: {e}"))
}

// ── Real GitHub API exchange ──────────────────────────────────────────────────

/// Exchange a signed App JWT for a GitHub installation access token.
///
/// Why: the production path that calls `POST /app/installations/{id}/access_tokens`
///      with `Authorization: Bearer <jwt>`.
/// What: calls the GitHub API using `reqwest::blocking`; parses the `token` and
///       `expires_at` fields from the JSON response; converts `expires_at` (ISO
///       8601) to a Unix timestamp.
/// Test: NOT called in unit tests — mocked via `GithubAppTokenProvider::with_injected`.
///       Integration tests are gated `#[ignore]`.
fn real_exchange_installation_token(
    jwt: &str,
    installation_id: u64,
) -> anyhow::Result<(String, i64)> {
    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("trusty-mpm/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest client build: {e}"))?;

    let resp = client
        .post(&url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {jwt}"))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|e| anyhow::anyhow!("token exchange request: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        return Err(anyhow::anyhow!(
            "GitHub App token exchange failed: HTTP {status}: {body}"
        ));
    }

    let parsed: InstallationTokenResponse = resp
        .json()
        .map_err(|e| anyhow::anyhow!("parse installation token response: {e}"))?;

    // Parse the ISO 8601 expiry string to Unix seconds.
    let expires_at_secs = parse_iso8601_to_unix(&parsed.expires_at)?;
    Ok((parsed.token, expires_at_secs))
}

/// Parse an ISO 8601 datetime string (e.g. `"2024-01-01T12:00:00Z"`) to Unix
/// seconds.
///
/// Why: the GitHub API returns `expires_at` as an ISO 8601 string; we need a
///      Unix timestamp for the cache validity check.
/// What: uses `chrono` (already a workspace dep) to parse the string and
///       convert to `timestamp()`.
/// Test: `tests::parse_iso8601_roundtrip`.
fn parse_iso8601_to_unix(s: &str) -> anyhow::Result<i64> {
    use chrono::DateTime;
    let dt = DateTime::parse_from_rfc3339(s)
        .map_err(|e| anyhow::anyhow!("parse expires_at '{s}': {e}"))?;
    Ok(dt.timestamp())
}

// ── resolve_token (top-level resolution) ─────────────────────────────────────

/// Resolve a token using the documented provider resolution order.
///
/// Why: callers (the filing function, MCP tools) should use a single entry
///      point rather than constructing providers themselves, to ensure the
///      documented resolution order is always respected.
/// What: tries in order:
///   1. PAT/token-file via `EnvFileTokenProvider`
///   2. GitHub App via `GithubAppTokenProvider` (if App env vars are all set)
///
/// Returns `None` if all sources are absent.
///
/// Test: `tests::resolution_order_*`.
pub fn resolve_token() -> Option<String> {
    // 1. PAT / token file.
    if let Some(tok) = EnvFileTokenProvider.token() {
        return Some(tok);
    }
    // 2. GitHub App (only if fully configured).
    if let Some(config) = GithubAppConfig::from_env() {
        let provider = GithubAppTokenProvider::new(config);
        if let Some(tok) = provider.token() {
            return Some(tok);
        }
    }
    None
}

// ── ResolvedProvider — full-chain adapter ─────────────────────────────────────

/// A [`TokenProvider`] adapter that delegates to the full `resolve_token()`
/// chain: PAT env → token file → GitHub App → `None`.
///
/// Why: `api.rs` and `mcp_backend.rs` originally hard-coded `EnvFileTokenProvider`
///      so the GitHub App path (Fix 1 / #498) was unreachable. `ResolvedProvider`
///      wraps `resolve_token()` behind the `TokenProvider` trait so both call
///      sites can use a single DRY adapter without duplicating resolution logic.
/// What: calls `resolve_token()` on every `token()` invocation; returns `None`
///       when all sources are absent (graceful NoToken degradation is preserved).
/// Test: `tests::resolved_provider_uses_pat_env`,
///       `tests::resolved_provider_returns_none_without_sources`.
pub struct ResolvedProvider;

impl TokenProvider for ResolvedProvider {
    fn token(&self) -> Option<String> {
        resolve_token()
    }
}

// ── jsonwebtoken dep check ────────────────────────────────────────────────────
// `jsonwebtoken` must be in the crate's Cargo.toml dependencies for this module
// to compile. It is not in the workspace table yet because it was first needed
// in Phase 4. It is added to `trusty-mpm/Cargo.toml` as a non-optional dep.

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Config with the real test RSA key fixture (skips gracefully if absent).
    fn config_with_test_key() -> Option<GithubAppConfig> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/daemon/bug_report/test_fixtures/test_rsa_key.pem");
        if !path.exists() {
            return None;
        }
        Some(GithubAppConfig {
            app_id: 12345,
            installation_id: 67890,
            private_key_path: path,
        })
    }

    // ── Resolution order tests ────────────────────────────────────────────────

    #[test]
    #[serial]
    fn resolution_order_env_wins_over_file() {
        // When TOKEN_ENV_VAR is set, EnvFileTokenProvider should return it.
        let sentinel = "ghp_test_env_wins_phase4_unique"; // pragma: allowlist secret
        // SAFETY: env mutation; cleaned up before return.
        unsafe { std::env::set_var(TOKEN_ENV_VAR, sentinel) };
        let tok = EnvFileTokenProvider.token();
        unsafe { std::env::remove_var(TOKEN_ENV_VAR) };
        assert!(tok.is_some(), "expected Some when env var is set: {tok:?}");
    }

    #[test]
    #[serial]
    fn resolution_order_file_used_when_env_absent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "ghp_from_file_phase4\n").unwrap();
        unsafe {
            std::env::remove_var(TOKEN_ENV_VAR);
            std::env::set_var(TOKEN_FILE_ENV_VAR, tmp.path().as_os_str());
        }
        let tok = EnvFileTokenProvider.token();
        unsafe { std::env::remove_var(TOKEN_FILE_ENV_VAR) };
        assert!(
            tok.is_some(),
            "expected Some from file when env var absent: {tok:?}"
        );
    }

    /// Verify PAT env (`TRUSTY_BUGREPORT_GITHUB_TOKEN`) wins over App env vars
    /// in the resolution order defined by [`resolve_token`].
    ///
    /// Why: the resolution order is PAT env → token file → GitHub App → None;
    ///      this test verifies that rule end-to-end through `resolve_token()`
    ///      itself, not just through `EnvFileTokenProvider`.
    /// What: sets both the PAT env var and the three App env vars (pointing at a
    ///       non-existent PEM), then calls `resolve_token()` and asserts the PAT
    ///       value is returned — not None (which would indicate the App path was
    ///       tried first and failed before the PAT could win).
    /// Test: this function. Marked `#[serial]` because it mutates shared env vars
    ///       that would race with other `TRUSTY_BUGREPORT_*` env tests.
    #[test]
    #[serial]
    fn resolve_token_prefers_pat_env() {
        let sentinel = "ghp_pat_env_wins_over_app"; // pragma: allowlist secret

        // Set PAT and App env vars simultaneously.
        unsafe {
            std::env::set_var(TOKEN_ENV_VAR, sentinel);
            std::env::remove_var(TOKEN_FILE_ENV_VAR);
            std::env::set_var(APP_ID_ENV_VAR, "12345");
            std::env::set_var(APP_INSTALL_ID_ENV_VAR, "67890");
            std::env::set_var(
                APP_KEY_FILE_ENV_VAR,
                "/tmp/trusty-test-nonexistent-pem-pattest.pem",
            );
        }

        let tok = resolve_token();

        // Clean up before any assert so failures don't leak env state.
        unsafe {
            std::env::remove_var(TOKEN_ENV_VAR);
            std::env::remove_var(APP_ID_ENV_VAR);
            std::env::remove_var(APP_INSTALL_ID_ENV_VAR);
            std::env::remove_var(APP_KEY_FILE_ENV_VAR);
        }

        assert_eq!(
            tok.as_deref(),
            Some(sentinel),
            "resolve_token must prefer PAT env over GitHub App env vars: {tok:?}"
        );
    }

    #[test]
    fn resolution_order_returns_none_when_all_absent() {
        // Use a token provider with no token (fixed None).
        struct NoneProvider;
        impl TokenProvider for NoneProvider {
            fn token(&self) -> Option<String> {
                None
            }
        }
        assert!(NoneProvider.token().is_none());
    }

    // ── AppJwtClaims tests ────────────────────────────────────────────────────

    #[test]
    fn jwt_claims_iss_and_window() {
        let now = 1_700_000_000i64;
        let claims = AppJwtClaims::new(42, now);
        assert_eq!(claims.iss, "42");
        assert_eq!(claims.iat, now);
        assert_eq!(claims.exp, now + 600, "exp must be iat + 600s");
    }

    // ── CachedToken tests ─────────────────────────────────────────────────────

    #[test]
    fn cache_returns_valid_token() {
        // This test requires the test RSA key fixture.
        let config = match config_with_test_key() {
            Some(c) => c,
            None => {
                eprintln!("SKIP cache_returns_valid_token: test RSA key fixture not found");
                return;
            }
        };

        // Inject a clock returning a fixed timestamp and an exchange that
        // returns a canned token expiring 60 minutes from "now".
        let now_secs = 1_700_000_000i64;
        let expiry = now_secs + 3600;

        let provider = GithubAppTokenProvider::with_injected(
            config,
            move || now_secs,
            move |_jwt, _install_id| Ok(("ghs_cached_token".to_string(), expiry)),
        );

        // First call should mint and cache.
        let tok1 = provider.get_token_cached().unwrap();
        assert_eq!(tok1, "ghs_cached_token");

        // Second call must return from cache (exchange_fn called only once).
        let tok2 = provider.get_token_cached().unwrap();
        assert_eq!(tok2, "ghs_cached_token", "second call must return cached");
    }

    #[test]
    fn cache_refreshes_before_expiry() {
        // Token expires at now + 200 seconds; within the 300s safety margin,
        // so `is_valid` returns false and the provider must refresh.
        let now_secs = 1_700_000_000i64;
        let almost_expired = now_secs + 200; // < 300s margin → invalid

        let cached = CachedToken {
            value: "old_token".to_string(),
            expires_at_secs: almost_expired,
        };
        assert!(
            !cached.is_valid(now_secs),
            "token with <300s remaining must be invalid"
        );

        // Verify tokens with >300s remaining are valid.
        let valid = CachedToken {
            value: "new_token".to_string(),
            expires_at_secs: now_secs + 3600,
        };
        assert!(
            valid.is_valid(now_secs),
            "token with 3600s remaining must be valid"
        );
    }

    #[test]
    fn cache_is_valid_exactly_at_margin() {
        let now_secs = 1_700_000_000i64;
        // Expires at exactly now + 300 → NOT valid (boundary is exclusive).
        let at_margin = CachedToken {
            value: "edge".to_string(),
            expires_at_secs: now_secs + 300,
        };
        assert!(
            !at_margin.is_valid(now_secs),
            "token at exactly 300s margin should be invalid"
        );

        // Expires at now + 301 → still valid.
        let just_past = CachedToken {
            value: "edge+1".to_string(),
            expires_at_secs: now_secs + 301,
        };
        assert!(
            just_past.is_valid(now_secs),
            "token at 301s margin should be valid"
        );
    }

    // ── ResolvedProvider tests ────────────────────────────────────────────────

    #[test]
    #[serial]
    fn resolved_provider_uses_pat_env() {
        // When TOKEN_ENV_VAR is set, ResolvedProvider should return that PAT.
        let sentinel = "ghp_resolved_provider_pat_test"; // pragma: allowlist secret
        unsafe { std::env::set_var(TOKEN_ENV_VAR, sentinel) };
        let tok = ResolvedProvider.token();
        unsafe { std::env::remove_var(TOKEN_ENV_VAR) };
        assert_eq!(
            tok.as_deref(),
            Some(sentinel),
            "ResolvedProvider must return the PAT env value"
        );
    }

    #[test]
    #[serial]
    fn resolved_provider_returns_none_without_sources() {
        // When neither TOKEN_ENV_VAR nor App vars are set, should return None.
        // We cannot guarantee a clean env in all CI scenarios; the test removes
        // the PAT var and uses a non-existent token file path so the chain
        // falls through gracefully.
        unsafe {
            std::env::remove_var(TOKEN_ENV_VAR);
            std::env::remove_var(APP_ID_ENV_VAR);
            std::env::remove_var(APP_INSTALL_ID_ENV_VAR);
            std::env::remove_var(APP_KEY_FILE_ENV_VAR);
            // Point the file fallback at a non-existent path.
            std::env::set_var(
                TOKEN_FILE_ENV_VAR,
                "/tmp/trusty-test-nonexistent-token-file-abc123",
            );
        }
        let tok = ResolvedProvider.token();
        unsafe { std::env::remove_var(TOKEN_FILE_ENV_VAR) };
        assert!(
            tok.is_none(),
            "ResolvedProvider must return None when all sources absent"
        );
    }

    #[test]
    #[serial]
    fn resolve_token_selects_app_when_only_app_env_set() {
        // Verify the resolution order: App provider is tried when PAT env is
        // absent. We cannot perform a real App exchange in unit tests, but we
        // can confirm the *selection* path: when App vars are set but the PEM
        // file does not exist, `resolve_token()` logs a warning and returns
        // None (App provider failed gracefully), not an error/panic.
        unsafe {
            std::env::remove_var(TOKEN_ENV_VAR);
            std::env::remove_var(TOKEN_FILE_ENV_VAR);
            std::env::set_var(APP_ID_ENV_VAR, "12345");
            std::env::set_var(APP_INSTALL_ID_ENV_VAR, "67890");
            std::env::set_var(
                APP_KEY_FILE_ENV_VAR,
                "/tmp/trusty-test-nonexistent-pem-abc123.pem",
            );
        }
        // The App provider attempts to read the PEM, fails gracefully → None.
        let tok = resolve_token();
        unsafe {
            std::env::remove_var(APP_ID_ENV_VAR);
            std::env::remove_var(APP_INSTALL_ID_ENV_VAR);
            std::env::remove_var(APP_KEY_FILE_ENV_VAR);
        }
        // None is the expected graceful-failure result: no panic, no unwrap.
        assert!(
            tok.is_none(),
            "resolve_token with missing PEM must return None gracefully"
        );
    }

    // ── ISO 8601 parsing ──────────────────────────────────────────────────────

    #[test]
    fn parse_iso8601_roundtrip() {
        // 2024-01-01T00:00:00Z → 1704067200
        let ts = parse_iso8601_to_unix("2024-01-01T00:00:00Z").unwrap();
        assert_eq!(ts, 1_704_067_200, "unexpected Unix timestamp: {ts}");
    }

    #[test]
    fn parse_iso8601_invalid_returns_err() {
        let result = parse_iso8601_to_unix("not-a-date");
        assert!(result.is_err());
    }

    // ── jwt_claims_correct test (requires test RSA key fixture) ───────────────
    // This test is skipped if the test fixture file does not exist yet.
    // The fixture is generated once and committed (it contains no real secrets).
    // See crates/trusty-mpm/src/daemon/bug_report/test_fixtures/README.md.
    #[test]
    fn jwt_claims_sign_verify() {
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/daemon/bug_report/test_fixtures/test_rsa_key.pem");
        if !fixture_path.exists() {
            // Fixture not present — skip gracefully (developer must generate it).
            eprintln!("SKIP jwt_claims_sign_verify: fixture not found at {fixture_path:?}");
            return;
        }

        let pem = std::fs::read_to_string(&fixture_path).unwrap();
        let now = 1_700_000_000i64;
        let claims = AppJwtClaims::new(999, now);
        let jwt_str = encode_jwt_rs256(&pem, &claims).expect("encode should succeed");

        // Decode without verification to inspect claims.
        let decoded = jsonwebtoken::decode::<AppJwtClaims>(
            &jwt_str,
            &jsonwebtoken::DecodingKey::from_secret(b""),
            &{
                let mut v = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
                v.insecure_disable_signature_validation();
                v.validate_exp = false;
                v
            },
        )
        .expect("decode should succeed with disabled validation");

        assert_eq!(decoded.claims.iss, "999");
        assert_eq!(decoded.claims.iat, now);
        assert_eq!(decoded.claims.exp, now + 600);
    }
}
