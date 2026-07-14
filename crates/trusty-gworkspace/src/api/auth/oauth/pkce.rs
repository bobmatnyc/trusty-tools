//! PKCE (RFC 7636) primitives, CSRF `state`, and callback-URL parsing.
//!
//! Why: The interactive consent flow must not depend on a live browser to be
//! correct — the crypto/encoding logic (code verifier/challenge, `state`
//! generation and validation, callback query parsing, and `id_token` email
//! extraction) is deterministic and fully unit-testable offline. Isolating it
//! here keeps `flow.rs` (which does I/O) thin and lets CI verify the parts
//! that matter without Google credentials.
//! What: Free functions + a `Pkce` value type. All randomness is sourced from
//! `uuid::Uuid::new_v4` (getrandom-backed) so no extra `rand` dependency is
//! pulled in. Percent-encode/decode helpers avoid a `url`-crate dependency.
//! Test: `mod tests` below covers verifier charset/length, S256 challenge
//! against an RFC 7636 vector, state uniqueness, query parsing, and
//! `id_token` decoding.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// A PKCE verifier/challenge pair for a single authorization request.
///
/// Why: RFC 7636 requires the client to prove possession of the verifier at
/// the token endpoint by having sent its S256 hash (the challenge) to the
/// authorization endpoint; both halves must be generated together and kept
/// paired for the duration of the flow.
/// What: `verifier` is a 43-char base64url (unreserved) high-entropy string;
/// `challenge` is `base64url(sha256(verifier))` with no padding.
/// Test: `verifier_is_valid_charset_and_length`, `challenge_matches_rfc_vector`.
#[derive(Debug, Clone)]
pub struct Pkce {
    /// The secret code verifier (sent to the token endpoint).
    pub verifier: String,
    /// The S256 code challenge (sent to the authorization endpoint).
    pub challenge: String,
}

impl Pkce {
    /// Generate a fresh verifier/challenge pair.
    ///
    /// Why: Every consent attempt needs its own high-entropy verifier so a
    /// captured authorization code cannot be replayed against a different
    /// verifier.
    /// What: Concatenates the 16 random bytes of two v4 UUIDs (256 bits) and
    /// base64url-encodes them into a 43-char verifier, then derives the S256
    /// challenge.
    /// Test: `verifier_is_valid_charset_and_length` and
    /// `challenge_matches_rfc_vector`.
    pub fn generate() -> Self {
        let mut entropy = Vec::with_capacity(32);
        entropy.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        entropy.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        let verifier = URL_SAFE_NO_PAD.encode(&entropy);
        let challenge = Self::challenge_for(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// Compute the S256 challenge for an arbitrary verifier.
    ///
    /// Why: Exposed separately so tests can check against the RFC 7636
    /// worked example without going through random generation.
    /// What: `base64url(sha256(ascii(verifier)))`, no padding.
    /// Test: `challenge_matches_rfc_vector`.
    pub fn challenge_for(verifier: &str) -> String {
        let digest = Sha256::digest(verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(digest)
    }
}

/// Generate an unguessable CSRF `state` token.
///
/// Why: Binds the browser round-trip to this process so a forged callback
/// (CSRF) with a mismatched `state` is rejected before any code exchange.
/// What: A hyphen-free v4 UUID (128 bits of entropy) as a hex string.
/// Test: `state_is_unique_and_nonempty`.
pub fn generate_state() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Parse an `application/x-www-form-urlencoded` query string into a map.
///
/// Why: The local callback receives `?code=...&state=...` (or `?error=...`)
/// and we must extract those values without pulling in the `url` crate.
/// What: Splits on `&`, then `=`, percent-decodes both key and value, and
/// treats `+` as a space per form-encoding rules. Later duplicate keys win.
/// Test: `parses_code_and_state`, `parses_error`, `percent_decodes_values`.
pub fn parse_callback_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

/// Percent-decode a single query component (`+` becomes space).
///
/// Why: Google URL-encodes the authorization `code` (it can contain `/`);
/// decoding is required before the token exchange.
/// What: Replaces `+` with space and each valid `%XX` with its byte; leaves
/// malformed escapes verbatim. Output is lossy-UTF8 (query components are
/// ASCII in practice).
/// Test: `percent_decodes_values`.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode a value for safe inclusion in an authorization URL query.
///
/// Why: The scope string and `redirect_uri` contain `:` `/` and spaces that
/// must be escaped so Google parses the query correctly.
/// What: Passes RFC 3986 unreserved characters (`A-Za-z0-9-._~`) through and
/// `%XX`-encodes everything else (uppercase hex).
/// Test: `encodes_reserved_characters`.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Extract the `email` claim from a Google `id_token` (JWT) without verifying
/// the signature.
///
/// Why: When the token response carries an `id_token`, we can resolve the
/// account email offline (no extra userinfo round-trip). Signature
/// verification is unnecessary here: the token came directly from Google's
/// TLS-authenticated token endpoint, not from an untrusted party.
/// What: Splits the JWT on `.`, base64url-decodes the payload segment, parses
/// it as JSON, and returns the `email` string claim if present.
/// Test: `extracts_email_from_id_token`, `rejects_malformed_id_token`.
pub fn email_from_id_token(id_token: &str) -> Option<String> {
    let payload_b64 = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_is_valid_charset_and_length() {
        let pkce = Pkce::generate();
        // 32 bytes -> 43 base64url chars (no padding).
        assert_eq!(pkce.verifier.len(), 43);
        assert!(pkce.verifier.len() >= 43 && pkce.verifier.len() <= 128);
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
            "verifier must use only RFC 7636 unreserved characters: {}",
            pkce.verifier
        );
        // Two fresh pairs must differ.
        assert_ne!(pkce.verifier, Pkce::generate().verifier);
    }

    #[test]
    fn challenge_matches_rfc_vector() {
        // RFC 7636 Appendix B worked example.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(Pkce::challenge_for(verifier), expected);
    }

    #[test]
    fn state_is_unique_and_nonempty() {
        let a = generate_state();
        let b = generate_state();
        assert!(!a.is_empty());
        assert_eq!(a.len(), 32, "hyphen-free uuid is 32 hex chars");
        assert_ne!(a, b);
    }

    #[test]
    fn parses_code_and_state() {
        let map = parse_callback_query("code=abc123&state=xyz&scope=openid");
        assert_eq!(map.get("code").map(String::as_str), Some("abc123"));
        assert_eq!(map.get("state").map(String::as_str), Some("xyz"));
        assert_eq!(map.get("scope").map(String::as_str), Some("openid"));
    }

    #[test]
    fn parses_error() {
        let map = parse_callback_query("error=access_denied");
        assert_eq!(map.get("error").map(String::as_str), Some("access_denied"));
        assert!(!map.contains_key("code"));
    }

    #[test]
    fn percent_decodes_values() {
        // Google encodes '/' in auth codes as %2F.
        let map = parse_callback_query("code=4%2F0Ab%2Dcd&state=a+b");
        assert_eq!(map.get("code").map(String::as_str), Some("4/0Ab-cd"));
        assert_eq!(map.get("state").map(String::as_str), Some("a b"));
    }

    #[test]
    fn encodes_reserved_characters() {
        assert_eq!(
            percent_encode("https://localhost:8080/callback"),
            "https%3A%2F%2Flocalhost%3A8080%2Fcallback"
        );
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("Aa0-._~"), "Aa0-._~");
    }

    #[test]
    fn extracts_email_from_id_token() {
        // header.payload.signature — only the payload must decode.
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"user@example.com","sub":"1"}"#);
        let token = format!("aGVhZGVy.{payload}.c2ln");
        assert_eq!(
            email_from_id_token(&token).as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn rejects_malformed_id_token() {
        assert!(email_from_id_token("not-a-jwt").is_none());
        assert!(email_from_id_token("only.two").is_none());
    }
}
