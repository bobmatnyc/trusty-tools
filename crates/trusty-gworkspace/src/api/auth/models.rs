//! OAuth token data models — wire-compatible with the Python project's
//! ``tokens.json`` format.
//!
//! Why: The Python CLI writes ``~/.gworkspace-mcp/tokens.json``; this Rust
//! port reads/writes the same file so the user does not need to re-auth
//! across implementations.
//! What: Serde-derived structs that round-trip the canonical JSON shape:
//! ``{ "version": 1, "metadata": {...}, "token": {...} }``.
//! Test: Unit tests below cover ``OAuthToken::is_expired`` and JSON
//! round-trip.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn default_token_type() -> String {
    "Bearer".to_string()
}

/// A single OAuth2 token (access + optional refresh + expiry).
///
/// Why: All Google Workspace requests need a valid access token, and our
/// 401-retry pattern needs to know when it's about to expire.
/// What: Mirrors Google's token-endpoint response fields with our own
/// `expires_at` (computed at storage time, not raw `expires_in`).
/// Test: `is_expired_handles_buffer` in this module.
// #8539: PartialEq (here and below) lets storage tell a shadow from a copy.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct OAuthToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default = "default_token_type")]
    pub token_type: String,
}

impl OAuthToken {
    /// Why: Callers need to know if a token needs refresh before making a request.
    /// What: Returns true if `expires_at` is within 60 seconds of now.
    /// Test: `is_expired_handles_buffer` below.
    pub fn is_expired(&self) -> bool {
        let buffer = chrono::Duration::seconds(60);
        Utc::now() + buffer >= self.expires_at
    }
}

/// Per-profile metadata: how it was created, when it last refreshed, email, etc.
///
/// Why: Multi-profile support — users may have multiple Google accounts and
/// we need to identify them for display and for the `account` MCP param.
/// What: Mirrors Python `TokenMetadata` model.
/// Test: Covered indirectly by the storage round-trip integration test.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TokenMetadata {
    pub service_name: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_refreshed: Option<DateTime<Utc>>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

fn default_provider() -> String {
    "google".to_string()
}

/// Top-level persisted record per profile in `tokens.json`.
///
/// Why: Wraps token + metadata + version for forward-compatible migrations.
/// What: A single entry in the `HashMap<String, StoredToken>` JSON object.
/// Test: `tests/auth_models.rs` round-trips a fixture string.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct StoredToken {
    #[serde(default = "default_version")]
    pub version: u32,
    pub metadata: TokenMetadata,
    pub token: OAuthToken,
}

fn default_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_expired_handles_buffer() {
        let near_expiry = OAuthToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Utc::now() + chrono::Duration::seconds(30),
            scopes: vec![],
            token_type: "Bearer".into(),
        };
        assert!(near_expiry.is_expired(), "30s out should count as expired");

        let fresh = OAuthToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Utc::now() + chrono::Duration::seconds(600),
            scopes: vec![],
            token_type: "Bearer".into(),
        };
        assert!(!fresh.is_expired(), "10m out should not count as expired");
    }

    #[test]
    fn stored_token_round_trips_python_shape() {
        let raw = r#"{
          "version": 1,
          "metadata": {
            "service_name": "gworkspace-mcp",
            "provider": "google",
            "created_at": "2024-01-01T00:00:00Z",
            "last_refreshed": null,
            "email": "user@example.com",
            "is_default": true
          },
          "token": {
            "access_token": "ya29.example",
            "refresh_token": "1//refresh",
            "expires_at": "2099-01-01T01:00:00Z",
            "scopes": ["https://www.googleapis.com/auth/calendar"],
            "token_type": "Bearer"
          }
        }"#;
        let parsed: StoredToken = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.metadata.email.as_deref(), Some("user@example.com"));
        assert!(parsed.metadata.is_default);
        assert_eq!(parsed.token.scopes.len(), 1);
        assert!(!parsed.token.is_expired());

        let s = serde_json::to_string(&parsed).expect("serialise");
        assert!(s.contains("\"version\":1"));
        assert!(s.contains("\"service_name\":\"gworkspace-mcp\""));
    }
}
