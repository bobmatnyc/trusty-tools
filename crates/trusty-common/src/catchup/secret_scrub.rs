//! Redact secret-shaped tokens in PM free text before it reaches a pause
//! snapshot, and keep commit SHAs and message ids (#10595).
//!
//! Why: #9390 replaced every 7-40 character hex word with `<sha?>`, so a
//! resumed session lost every commit SHA and Architect message id it had
//! recorded and could not read the Architect's reply (#10595). Exact values
//! are the point of a snapshot; the only thing it must not carry is a
//! credential.
//! What: [`scrub_secret_like`] replaces each secret-like word with
//! [`SECRET_PLACEHOLDER`]. A word is a maximal run of ASCII letters, digits,
//! `_` and `-`. The rule that tells a SHA apart from a secret:
//! - a word of at most 40 hex characters is a git SHA-1 object id (abbreviated
//!   or full) or a hex message id, and is kept;
//! - a word of more than 40 hex characters is longer than any SHA-1 id, so it
//!   is a hex key, HMAC or token, and is redacted;
//! - a word opening with a known credential prefix (`ghp_`, `sk-`, `xoxb-`,
//!   `AKIA`, ...) and at least [`MIN_PREFIXED_LEN`] long is redacted.
//!
//! Test: `the_token_rule_keeps_shas_and_redacts_secrets`,
//! `a_snapshot_keeps_shas_and_message_ids` and
//! `a_snapshot_redacts_a_secret_shaped_hex_value` (catchup::pause).

use std::borrow::Cow;

/// What a redacted token is replaced with.
pub const SECRET_PLACEHOLDER: &str = "[REDACTED]";

/// Longest hex word kept: a full SHA-1 object id. Longer hex is a secret.
const MAX_SHA_LEN: usize = 40;

/// Shortest prefixed word treated as a credential, so `sk-1` in prose is kept.
const MIN_PREFIXED_LEN: usize = 20;

/// Prefixes that open a provider credential.
const CREDENTIAL_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "sk-",
    "xoxa-",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "ASIA",
];

/// Is `c` part of a word for the token rule?
fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// Is `word` (one whole word) secret-like under the module's rule?
pub fn is_secret_like(word: &str) -> bool {
    let bytes = word.as_bytes();
    let long_hex = bytes.len() > MAX_SHA_LEN && bytes.iter().all(u8::is_ascii_hexdigit);
    let prefixed =
        bytes.len() >= MIN_PREFIXED_LEN && CREDENTIAL_PREFIXES.iter().any(|p| word.starts_with(p));
    long_hex || prefixed
}

/// `text` with every secret-like word replaced by [`SECRET_PLACEHOLDER`].
///
/// Why/What: see the module doc. Borrowed when nothing matched.
/// Test: `the_token_rule_keeps_shas_and_redacts_secrets`.
pub fn scrub_secret_like(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if !is_word_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word_byte(bytes[i]) {
            i += 1;
        }
        // Word bytes are ASCII, so `start..i` is a char boundary on both ends.
        if is_secret_like(&text[start..i]) {
            let buf = out.get_or_insert_with(|| String::with_capacity(text.len()));
            buf.push_str(&text[copied..start]);
            buf.push_str(SECRET_PLACEHOLDER);
            copied = i;
        }
    }
    match out {
        Some(mut buf) => {
            buf.push_str(&text[copied..]);
            Cow::Owned(buf)
        }
        None => Cow::Borrowed(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_rule_keeps_shas_and_redacts_secrets() {
        let sha40 = "0123456789abcdef0123456789abcdef01234567";
        let hex64 = "a1b2".repeat(16);
        let ghp = format!("ghp_{}", "A1b2C3d4".repeat(4));
        let kept = [
            "PR #12 head d3963848ab.",
            "compare abc1234..def5678",
            sha40,
            "Architect msg 19a2f3c4d5e6f0a12b",
            "session 7bd5c27a-475b-41df-9e9f-a6f630801717",
            "sk-1 and ghp_x are prose",
        ];
        for input in kept {
            assert!(
                matches!(scrub_secret_like(input), Cow::Borrowed(_)),
                "{input:?}"
            );
        }
        // 41 hex characters is longer than any SHA-1 id.
        assert_eq!(scrub_secret_like(&format!("{sha40}a")), SECRET_PLACEHOLDER);
        assert_eq!(
            scrub_secret_like(&format!("key={hex64}.")),
            format!("key={SECRET_PLACEHOLDER}.")
        );
        assert_eq!(
            scrub_secret_like(&format!("token {ghp} é")),
            format!("token {SECRET_PLACEHOLDER} é")
        );
        let akia = format!("AKIA{}", "Q7".repeat(8));
        assert_eq!(scrub_secret_like(&akia), SECRET_PLACEHOLDER);
    }
}
