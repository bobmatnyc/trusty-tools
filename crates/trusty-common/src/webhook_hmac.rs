//! GitHub webhook HMAC-SHA256 verification, over the exact received bytes.
//!
//! Why: the same check exists twice already — `trusty-analyze`'s
//! `core/github.rs:210` and `trusty-review`'s
//! `integrations/github/webhook.rs:27` — and they disagree on what an *unset*
//! secret means: review returns 401, analyze logs a warning and processes the
//! payload anyway (ADR-0034 Context, "how they disagree"). ADR-0034 §3 puts
//! verification in exactly one place, at console, and unifies the unset-secret
//! policy to fail-closed. This module is that place. #5089 step 3 routes
//! console through it; step 4 retires the other two copies.
//!
//! What: [`verify_github_signature`] returns a three-state
//! [`SignatureVerdict`] rather than a `bool`, because "no secret is configured"
//! and "the signature is wrong" are different operator problems that both have
//! to fail closed. The digest is computed over the raw body bytes — the HMAC
//! covers the literal request body, so a caller must pass what arrived on the
//! wire, never a re-serialised value.
//!
//! Test: `tests` below — a valid signature, a wrong secret, an empty secret, a
//! missing `sha256=` prefix, non-hex, a truncated digest, and a body mutated by
//! one byte.
//!
//! [`verify_github_signature`]: crate::webhook_hmac::verify_github_signature
//! [`SignatureVerdict`]: crate::webhook_hmac::SignatureVerdict

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Algorithm name recorded in a relayed frame's provenance record.
///
/// ADR-0034 §3 requires the frame to state which algorithm was checked, so a
/// target can tell a verified delivery from an unverified one without
/// re-deriving it from the header name.
pub const HMAC_ALGORITHM: &str = "hmac-sha256";

/// The header GitHub carries the digest in.
pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";

/// Outcome of verifying one delivery.
///
/// Why: a `bool` cannot distinguish "the operator never configured a secret"
/// from "someone sent a forged payload", and collapsing them is how
/// `trusty-analyze` ended up treating the first as permission to proceed.
/// Both are refusals here; only the diagnostic differs.
/// What: three variants, of which exactly one means the payload may be used.
/// Test: every `verify_github_signature_*` test asserts on a specific variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureVerdict {
    /// The digest matched. The body is authentic.
    Valid,
    /// No secret is configured, so nothing can be verified. Fail closed.
    SecretMissing,
    /// A secret is configured and the digest did not match, was malformed, or
    /// was absent.
    Invalid,
}

impl SignatureVerdict {
    /// True only for [`SignatureVerdict::Valid`].
    ///
    /// Exists so a call site cannot accidentally write `!= Invalid` and let
    /// `SecretMissing` through.
    pub fn is_valid(self) -> bool {
        matches!(self, SignatureVerdict::Valid)
    }
}

/// Verify a GitHub `X-Hub-Signature-256` digest against the raw body.
///
/// Why: the single entry point for this check (ADR-0034 §3, "Verification
/// happens exactly once, at console, over the exact bytes GitHub sent"). Any
/// re-framing of the body destroys the ability to verify it, so this must run
/// before the payload is parsed, copied, or re-encoded.
///
/// What: rejects an empty or whitespace-only `secret` with
/// [`SignatureVerdict::SecretMissing`]; otherwise parses the `sha256=<hex>`
/// header, decodes the hex, and constant-time compares it against
/// `HMAC-SHA256(secret, body)` via `Mac::verify_slice`. Any parse or decode
/// failure is [`SignatureVerdict::Invalid`], never an error return, so the
/// caller emits one uniform 401 for every refusal and leaks nothing about
/// which step failed.
///
/// Test: `verify_github_signature_accepts_a_valid_digest`,
/// `verify_github_signature_rejects_a_wrong_secret`,
/// `verify_github_signature_reports_a_missing_secret`,
/// `verify_github_signature_rejects_a_missing_prefix`,
/// `verify_github_signature_rejects_non_hex`,
/// `verify_github_signature_rejects_a_truncated_digest`,
/// `verify_github_signature_rejects_a_one_byte_body_change`.
pub fn verify_github_signature(
    secret: &str,
    body: &[u8],
    signature_header: &str,
) -> SignatureVerdict {
    if secret.trim().is_empty() {
        return SignatureVerdict::SecretMissing;
    }
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return SignatureVerdict::Invalid;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return SignatureVerdict::Invalid;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return SignatureVerdict::Invalid;
    };
    mac.update(body);
    if mac.verify_slice(&expected).is_ok() {
        SignatureVerdict::Valid
    } else {
        SignatureVerdict::Invalid
    }
}

/// Produce the `sha256=<hex>` header value for `body` under `secret`.
///
/// Why: every test in this workspace that exercises a webhook path needs to
/// mint a valid signature, and three copies of that helper already exist in
/// test modules. Exposing it from the crate that owns verification keeps the
/// signing and checking halves from drifting.
/// What: `format!("sha256={}", hex(HMAC-SHA256(secret, body)))`.
/// Test: used as the input to `verify_github_signature_accepts_a_valid_digest`.
pub fn sign_github_body(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts a key of any length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-hmac-key"; // pragma: allowlist secret
    const BODY: &[u8] = br#"{"action":"review_requested","number":7}"#;

    #[test]
    fn verify_github_signature_accepts_a_valid_digest() {
        let header = sign_github_body(SECRET, BODY);
        assert_eq!(
            verify_github_signature(SECRET, BODY, &header),
            SignatureVerdict::Valid
        );
    }

    #[test]
    fn verify_github_signature_rejects_a_wrong_secret() {
        let header = sign_github_body("some-other-secret", BODY); // pragma: allowlist secret
        assert_eq!(
            verify_github_signature(SECRET, BODY, &header),
            SignatureVerdict::Invalid
        );
    }

    #[test]
    fn verify_github_signature_reports_a_missing_secret() {
        let header = sign_github_body(SECRET, BODY);
        // An unset secret and a whitespace-only one are the same operator
        // mistake and must not be distinguishable from a wrong signature by
        // the caller's response.
        for empty in ["", "   ", "\t\n"] {
            assert_eq!(
                verify_github_signature(empty, BODY, &header),
                SignatureVerdict::SecretMissing,
                "secret {empty:?}"
            );
            assert!(!verify_github_signature(empty, BODY, &header).is_valid());
        }
    }

    #[test]
    fn verify_github_signature_rejects_a_missing_prefix() {
        let signed = sign_github_body(SECRET, BODY);
        let bare = signed.trim_start_matches("sha256=");
        assert_eq!(
            verify_github_signature(SECRET, BODY, bare),
            SignatureVerdict::Invalid
        );
        assert_eq!(
            verify_github_signature(SECRET, BODY, ""),
            SignatureVerdict::Invalid
        );
    }

    #[test]
    fn verify_github_signature_rejects_non_hex() {
        assert_eq!(
            verify_github_signature(SECRET, BODY, "sha256=zzzz"),
            SignatureVerdict::Invalid
        );
    }

    #[test]
    fn verify_github_signature_rejects_a_truncated_digest() {
        let signed = sign_github_body(SECRET, BODY);
        let truncated = &signed[..signed.len() - 4];
        assert_eq!(
            verify_github_signature(SECRET, BODY, truncated),
            SignatureVerdict::Invalid
        );
    }

    #[test]
    fn verify_github_signature_rejects_a_one_byte_body_change() {
        let header = sign_github_body(SECRET, BODY);
        let mut mutated = BODY.to_vec();
        let last = mutated.len() - 1;
        mutated[last] ^= 0x01;
        assert_eq!(
            verify_github_signature(SECRET, &mutated, &header),
            SignatureVerdict::Invalid
        );
    }
}
