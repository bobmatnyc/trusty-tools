//! Endpoint, scope and timing constants for the Google Chat API layer.
//!
//! Why: one place for every Google URL and tunable, so tests override the
//! hosts through `client::Endpoints` and nothing else hardcodes them.
//! What: production hosts, the JWT bearer grant, the two OAuth scopes, the
//! assertion lifetime, the token refresh margin and the key-file mode rule.
//! Test: exercised through `tests/gchat_http.rs`.

use std::time::Duration;

/// Google's OAuth 2.0 token endpoint, where the signed assertion is exchanged.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// The `aud` claim of every assertion. Google requires this exact value, so it
/// stays fixed even when tests point the transport at a mock server.
pub const TOKEN_AUDIENCE: &str = "https://oauth2.googleapis.com/token";

/// Google Chat REST host; paths are appended as `/v1/spaces/{space}/messages`.
pub const CHAT_API_BASE: &str = "https://chat.googleapis.com";

/// Cloud Pub/Sub REST host; paths are appended as `/v1/{subscription}:pull`.
pub const PUBSUB_API_BASE: &str = "https://pubsub.googleapis.com";

/// `grant_type` value of the service-account JWT bearer flow (RFC 7523).
pub const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// Scope for Chat app authentication (`spaces.messages.create` as the app).
pub const SCOPE_CHAT_BOT: &str = "https://www.googleapis.com/auth/chat.bot";

/// Scope for Pub/Sub `subscriptions.pull` and `subscriptions.acknowledge`.
pub const SCOPE_PUBSUB: &str = "https://www.googleapis.com/auth/pubsub";

/// Every scope one access token carries, joined with spaces in the assertion.
pub const SCOPES: [&str; 2] = [SCOPE_CHAT_BOT, SCOPE_PUBSUB];

/// Assertion lifetime (`exp - iat`). Google caps it at one hour.
pub const ASSERTION_LIFETIME_SECS: u64 = 3600;

/// A cached access token is replaced once fewer than this many seconds remain.
pub const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;

/// The only accepted permission bits for a service-account key file.
pub const REQUIRED_KEY_FILE_MODE: u32 = 0o600;

/// Longest error message kept from a Google error body, after redaction.
pub const ERROR_MESSAGE_MAX_CHARS: usize = 300;

/// Whole-request timeout. A Pub/Sub pull without `returnImmediately` may hold
/// the request open for a bounded time while it waits for messages.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
