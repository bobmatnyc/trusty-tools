//! Service-account access tokens: key-file loading, the RS256 JWT bearer
//! grant, and a token cache.
//!
//! Why: Chat app authentication and Pub/Sub pull both take an OAuth access
//! token minted from a service-account key. The Mac running the bot has no
//! user to consent, so the JWT bearer grant is the only flow that fits
//! (#9448). The key is the most sensitive thing this crate holds, so loading
//! refuses a key file anyone but its owner can read, and no type here prints
//! key material or a token.
//! What: [`ServiceAccountKey`] loads a Google JSON key file after an `fstat`
//! mode check; [`TokenSource`] signs an assertion, exchanges it at the token
//! endpoint and caches the result until [`TOKEN_REFRESH_MARGIN_SECS`] before
//! expiry. Time comes from an injectable [`Clock`].
//! Test: `tests/gchat_http.rs` — `key_file_mode_0644_is_refused_before_any_request`,
//! `token_is_cached_within_expiry_and_refreshed_past_margin`,
//! `jwt_assertion_verifies_with_the_public_key_and_carries_the_claims`,
//! `debug_output_never_contains_key_material_or_token`.

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::gchat::api::constants::{
    ASSERTION_LIFETIME_SECS, JWT_BEARER_GRANT_TYPE, REQUIRED_KEY_FILE_MODE, SCOPES, TOKEN_AUDIENCE,
    TOKEN_REFRESH_MARGIN_SECS,
};
use crate::gchat::api::error::{error_message, transport_error, GchatError};

/// Source of "now" as Unix seconds. Tests inject a settable clock so cache
/// expiry is exercised without sleeping.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The wall clock as Unix seconds (0 if the system clock predates 1970).
pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

/// The fields this layer reads from a Google service-account JSON key file.
#[derive(Deserialize)]
struct KeyFileJson {
    #[serde(rename = "type", default)]
    key_type: Option<String>,
    client_email: String,
    private_key: String,
    #[serde(default)]
    private_key_id: Option<String>,
}

/// JWT claims of the bearer-grant assertion.
#[derive(Serialize)]
struct AssertionClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

/// A loaded service-account key, ready to sign assertions.
///
/// Why: holds the parsed RSA key so the file is read once, at startup.
/// What: the service-account email (`iss`), the optional key id (`kid`
/// header) and the signing key. `Debug` prints the email and key id only.
/// Test: `key_file_mode_0644_is_refused_before_any_request`,
/// `debug_output_never_contains_key_material_or_token`.
pub struct ServiceAccountKey {
    client_email: String,
    private_key_id: Option<String>,
    signing_key: EncodingKey,
}

impl ServiceAccountKey {
    /// Load a Google service-account JSON key file from `path`.
    ///
    /// Why: the caller names a path, and a key readable by group or others
    /// must never be used (#9448, Architect ruling D4).
    /// What: opens the file, checks the permission bits of the open handle
    /// (no check-then-open race) are exactly `0600`, then parses
    /// `client_email`, `private_key` (PKCS#8 or PKCS#1 PEM) and
    /// `private_key_id`. Every error names the path and a fixed reason, never
    /// file content. Non-Unix platforms are refused, since the mode cannot be
    /// checked there.
    /// Test: `key_file_mode_0644_is_refused_before_any_request`,
    /// `key_file_that_is_not_a_service_account_key_is_refused`.
    pub fn from_file(path: &Path) -> Result<Self, GchatError> {
        let read_err = |e: std::io::Error| GchatError::KeyFileRead {
            path: path.to_path_buf(),
            reason: e.kind().to_string(),
        };
        let invalid = |reason: &'static str| GchatError::KeyFileInvalid {
            path: path.to_path_buf(),
            reason,
        };
        let mut file = std::fs::File::open(path).map_err(read_err)?;
        check_mode(path, &file)?;
        let mut raw = String::new();
        file.read_to_string(&mut raw).map_err(read_err)?;
        let parsed: KeyFileJson = serde_json::from_str(&raw)
            .map_err(|_| invalid("not a JSON key with client_email and private_key"))?;
        if parsed
            .key_type
            .as_deref()
            .is_some_and(|t| t != "service_account")
        {
            return Err(invalid("`type` is not service_account"));
        }
        let signing_key = EncodingKey::from_rsa_pem(parsed.private_key.as_bytes())
            .map_err(|_| invalid("private_key is not an RSA PEM key"))?;
        Ok(Self {
            client_email: parsed.client_email,
            private_key_id: parsed.private_key_id,
            signing_key,
        })
    }

    /// The service-account email, used as the assertion's `iss`.
    pub fn client_email(&self) -> &str {
        &self.client_email
    }

    /// Sign a bearer-grant assertion for `scope`, issued at `now`.
    fn sign_assertion(&self, scope: &str, now: u64) -> Result<String, GchatError> {
        let claims = AssertionClaims {
            iss: &self.client_email,
            scope,
            aud: TOKEN_AUDIENCE,
            iat: now,
            exp: now.saturating_add(ASSERTION_LIFETIME_SECS),
        };
        let mut header = Header::new(Algorithm::RS256);
        header.kid = self.private_key_id.clone();
        jsonwebtoken::encode(&header, &claims, &self.signing_key)
            .map_err(|e| GchatError::Signing(e.to_string()))
    }
}

impl fmt::Debug for ServiceAccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccountKey")
            .field("client_email", &self.client_email)
            .field("private_key_id", &self.private_key_id)
            .field("signing_key", &"[redacted]")
            .finish()
    }
}

/// Refuse a key file whose permission bits are not exactly `0600`.
#[cfg(unix)]
fn check_mode(path: &Path, file: &std::fs::File) -> Result<(), GchatError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = file.metadata().map_err(|e| GchatError::KeyFileRead {
        path: path.to_path_buf(),
        reason: e.kind().to_string(),
    })?;
    let mode = meta.permissions().mode() & 0o777;
    if mode != REQUIRED_KEY_FILE_MODE {
        return Err(GchatError::KeyFilePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

/// Non-Unix: the mode cannot be checked, so the key is refused.
#[cfg(not(unix))]
fn check_mode(path: &Path, _file: &std::fs::File) -> Result<(), GchatError> {
    let _ = REQUIRED_KEY_FILE_MODE;
    Err(GchatError::KeyFileInvalid {
        path: path.to_path_buf(),
        reason: "key-file permission check is only supported on Unix",
    })
}

/// An OAuth access token and its expiry.
///
/// Why: callers need the token value for one request; nothing should print it.
/// What: `Debug` shows only the expiry.
/// Test: `debug_output_never_contains_key_material_or_token`.
#[derive(Clone)]
pub struct AccessToken {
    value: String,
    expires_at: u64,
}

impl AccessToken {
    /// The bearer token value. Send it in an `Authorization` header only.
    pub fn secret(&self) -> &str {
        &self.value
    }

    /// Expiry as Unix seconds, per the source's clock.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessToken")
            .field("value", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The token endpoint's success body.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// Mints and caches access tokens for one service account.
///
/// Why: a token lives an hour; minting one per request would add a round trip
/// to every call and invite rate limits.
/// What: one cached token behind an async mutex, so concurrent callers
/// trigger at most one refresh. The token carries both [`SCOPES`].
/// Test: `token_is_cached_within_expiry_and_refreshed_past_margin`,
/// `jwt_assertion_verifies_with_the_public_key_and_carries_the_claims`.
pub struct TokenSource {
    key: ServiceAccountKey,
    http: reqwest::Client,
    token_url: String,
    scope: String,
    clock: Clock,
    cache: tokio::sync::Mutex<Option<AccessToken>>,
}

impl TokenSource {
    /// A source for `key` that exchanges assertions at `token_url`, using the
    /// system clock.
    pub fn new(
        key: ServiceAccountKey,
        http: reqwest::Client,
        token_url: impl Into<String>,
    ) -> Self {
        Self {
            key,
            http,
            token_url: token_url.into(),
            scope: SCOPES.join(" "),
            clock: system_clock(),
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Replace the clock (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The service-account email the tokens are minted for.
    pub fn client_email(&self) -> &str {
        self.key.client_email()
    }

    /// A valid access token, from cache when more than the refresh margin
    /// remains, else freshly minted.
    ///
    /// Why: the single entry point every API call takes its bearer from.
    /// What: on a miss, signs an assertion (`iss`, `scope`, `aud`, `iat`,
    /// `exp = iat + 3600`), POSTs it as a form with the JWT bearer grant type,
    /// and caches the result with `expires_at = now + expires_in`.
    /// Test: `token_is_cached_within_expiry_and_refreshed_past_margin`,
    /// `token_endpoint_refusal_is_typed_and_redacted`.
    pub async fn access_token(&self) -> Result<AccessToken, GchatError> {
        let mut cache = self.cache.lock().await;
        let now = (self.clock)();
        if let Some(token) = cache.as_ref() {
            if now.saturating_add(TOKEN_REFRESH_MARGIN_SECS) < token.expires_at {
                return Ok(token.clone());
            }
        }
        let fresh = self.fetch(now).await?;
        *cache = Some(fresh.clone());
        Ok(fresh)
    }

    /// Drop the cached token so the next call mints a new one. Called after a
    /// 401, when Google has revoked a token the cache still holds.
    pub async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }

    async fn fetch(&self, now: u64) -> Result<AccessToken, GchatError> {
        let assertion = self.key.sign_assertion(&self.scope, now)?;
        let form = [
            ("grant_type", JWT_BEARER_GRANT_TYPE),
            ("assertion", assertion.as_str()),
        ];
        let response = self
            .http
            .post(&self.token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| transport_error("oauth2", &e))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| transport_error("oauth2", &e))?;
        if !status.is_success() {
            return Err(GchatError::TokenEndpoint {
                status: status.as_u16(),
                message: error_message(&body, &[&assertion]),
            });
        }
        // A serde error can quote the body, which holds the token: fixed text.
        let parsed: TokenResponse =
            serde_json::from_str(&body).map_err(|_| GchatError::Decode {
                api: "oauth2",
                reason: "token response lacks access_token or expires_in".to_string(),
            })?;
        if parsed.access_token.is_empty() {
            return Err(GchatError::Decode {
                api: "oauth2",
                reason: "token response has an empty access_token".to_string(),
            });
        }
        Ok(AccessToken {
            value: parsed.access_token,
            expires_at: now.saturating_add(parsed.expires_in),
        })
    }
}

impl fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSource")
            .field("key", &self.key)
            .field("token_url", &self.token_url)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}
