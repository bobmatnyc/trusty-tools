//! GitHub webhook HMAC-SHA256 signature verification.
//!
//! Why: webhook payloads from GitHub are unauthenticated unless the shared
//! secret HMAC is verified.  This guard must be applied before processing any
//! webhook event to prevent replay attacks and spoofed payloads.
//! (spec REV-404, reused semantics from `crates/trusty-analyze/src/core/github.rs`)
//!
//! What: `verify_webhook_signature` computes `HMAC-SHA256(secret, body)` and
//! performs a constant-time comparison against the `sha256=<hex>` value in the
//! `X-Hub-Signature-256` header.  Returns `true` only if the signatures match.
//!
//! Test: `webhook_signature_accepts_valid` and `webhook_signature_rejects_*`
//! cover the happy path, bad hex, missing prefix, and wrong-secret cases.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Verify a GitHub webhook `X-Hub-Signature-256` HMAC.
///
/// Why: webhooks without verified signatures must be rejected; a missing or
/// mismatched signature indicates a replay, forgery, or misconfiguration.
/// What: parses the `sha256=<hex>` prefix, decodes the hex digest, computes
/// `HMAC-SHA256(secret, body)`, and constant-time compares with the expected
/// digest.  Returns `false` (not an error) on any parse/decode failure so the
/// caller can produce a uniform 403 response.
/// Test: `webhook_signature_accepts_valid`, `webhook_signature_rejects_invalid`.
pub fn verify_webhook_signature(secret: &str, body: &[u8], signature_header: &str) -> bool {
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_signature(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn webhook_signature_accepts_valid() {
        let secret = "test-hmac-key"; // pragma: allowlist secret
        let body = br#"{"action":"review_requested"}"#;
        let header = make_signature(secret, body);
        assert!(verify_webhook_signature(secret, body, &header));
    }

    #[test]
    fn webhook_signature_rejects_wrong_secret() {
        let body = br#"{"action":"review_requested"}"#;
        let header = make_signature("correct-secret", body); // pragma: allowlist secret
        assert!(!verify_webhook_signature("wrong-secret", body, &header));
    }

    #[test]
    fn webhook_signature_rejects_missing_prefix() {
        let secret = "s"; // pragma: allowlist secret
        let body = b"payload";
        // Header without the "sha256=" prefix.
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let raw_hex = hex::encode(mac.finalize().into_bytes());
        assert!(!verify_webhook_signature(secret, body, &raw_hex));
    }

    #[test]
    fn webhook_signature_rejects_bad_hex() {
        let secret = "s"; // pragma: allowlist secret
        let body = b"payload";
        assert!(!verify_webhook_signature(
            secret,
            body,
            "sha256=not-valid-hex!!"
        ));
    }

    #[test]
    fn webhook_signature_rejects_empty_header() {
        assert!(!verify_webhook_signature("secret", b"body", ""));
    }

    #[test]
    fn webhook_signature_rejects_tampered_body() {
        let secret = "my-secret"; // pragma: allowlist secret
        let original = br#"{"action":"review_requested"}"#;
        let tampered = br#"{"action":"force_push"}"#;
        let header = make_signature(secret, original);
        assert!(!verify_webhook_signature(secret, tampered, &header));
    }
}
