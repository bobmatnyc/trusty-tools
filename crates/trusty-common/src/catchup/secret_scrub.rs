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
//!   `AKIA`, ...) and at least [`MIN_PREFIXED_LEN`] long is redacted;
//! - a value of [`MIN_KEYED_LEN`] or more hex or base64-like characters right
//!   after a credential key (`api_key=`, `token:`, `Bearer`, ...) is redacted
//!   at any length, because context, not length, makes it a secret.
//!
//! A stale PR head is not this module's concern: trusty-mpm's `resume_verify`
//! re-reads every head at resume time.
//!
//! Test: `the_token_rule_keeps_shas_and_redacts_secrets`,
//! `a_keyed_hex_value_is_redacted_and_a_bare_sha_kept`,
//! `a_snapshot_keeps_shas_and_message_ids` and
//! `a_snapshot_redacts_a_secret_shaped_hex_value` (catchup::pause).

use std::borrow::Cow;

/// What a redacted token is replaced with.
pub const SECRET_PLACEHOLDER: &str = "[REDACTED]";

/// Longest hex word kept: a full SHA-1 object id. Longer hex is a secret.
const MAX_SHA_LEN: usize = 40;

/// Shortest prefixed word treated as a credential, so `sk-1` in prose is kept.
const MIN_PREFIXED_LEN: usize = 20;

/// Shortest value after a credential key that is redacted (#10595).
const MIN_KEYED_LEN: usize = 16;

/// Word endings that name a credential, matched case-insensitively (#10595).
const CREDENTIAL_KEYS: &[&str] = &[
    "api_key", "apikey", "token", "secret", "password", "passwd", "auth", "bearer",
];

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

/// Does `word` name a credential (`api_key`, `GH_TOKEN`, `Bearer`, ...)?
fn is_credential_key(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    CREDENTIAL_KEYS.iter().any(|k| lower.ends_with(k))
}

/// Is `c` part of a hex or base64-like value?
fn is_value_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'+' | b'/' | b'=')
}

/// The byte span of a keyed value starting after a key ending at `at`.
///
/// Why: #10595 — `api_key=<32 hex>` is a SHA by length but a secret by context.
/// What: skips a quote closing a JSON key, spaces, one optional `=` or `:`,
/// spaces and one optional quote, then takes a run of value bytes, with
/// interior dots so a JWT is one value. Returns it when it is at least
/// [`MIN_KEYED_LEN`] long and either follows a separator or holds a digit, so
/// prose after a bare key is kept.
/// Test: `a_keyed_hex_value_is_redacted_and_a_bare_sha_kept`,
/// `a_json_key_keeps_its_closing_quote`, `a_jwt_is_redacted_across_its_dots`,
/// `a_letter_only_value_after_a_separator_is_redacted`.
fn keyed_value_span(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let skip_spaces = |mut j: usize| {
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        j
    };
    let is_sep = |j: usize| j < bytes.len() && matches!(bytes[j], b'=' | b':');
    let mut j = at;
    // #10595: `"token": "…"` — the key's closing quote is not the value's opening one.
    if j < bytes.len() && matches!(bytes[j], b'"' | b'\'') && is_sep(skip_spaces(j + 1)) {
        j += 1;
    }
    j = skip_spaces(j);
    let sep = is_sep(j);
    if sep {
        j = skip_spaces(j + 1);
    }
    if j < bytes.len() && matches!(bytes[j], b'"' | b'\'') {
        j += 1;
    }
    let from = j;
    // #10595: a dot followed by a value byte is inside the value (JWT `h.p.s`).
    while j < bytes.len()
        && (is_value_byte(bytes[j])
            || (bytes[j] == b'.' && j + 1 < bytes.len() && is_value_byte(bytes[j + 1])))
    {
        j += 1;
    }
    let value = &bytes[from..j];
    // #10595: after `=` or `:` a letter-only value is as secret as a mixed one.
    let keyed = sep || value.iter().any(u8::is_ascii_digit);
    (value.len() >= MIN_KEYED_LEN && keyed).then_some((from, j))
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
        let word = &text[start..i];
        // #10595: a value after a credential key is redacted at any length.
        let span = if is_credential_key(word) {
            keyed_value_span(bytes, i)
        } else if is_secret_like(word) {
            Some((start, i))
        } else {
            None
        };
        if let Some((from, to)) = span {
            let buf = out.get_or_insert_with(|| String::with_capacity(text.len()));
            buf.push_str(&text[copied..from]);
            buf.push_str(SECRET_PLACEHOLDER);
            copied = to;
            i = to;
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

    /// Why (#10595 review): a keyed value no longer than a SHA leaked.
    /// What: `api_key=<32 hex>` and `token: <40 hex>` are redacted, the bare
    /// 40-hex SHA on the same line is kept, and prose after a key is kept.
    /// Test: itself.
    #[test]
    fn a_keyed_hex_value_is_redacted_and_a_bare_sha_kept() {
        let sha40 = "0123456789abcdef0123456789abcdef01234567";
        let hex32 = "c0ffee12".repeat(4);
        let hex40 = "f1e2d3c4b5".repeat(4);
        let line = format!("head {sha40} api_key={hex32} token: {hex40}");
        let want = format!("head {sha40} api_key={SECRET_PLACEHOLDER} token: {SECRET_PLACEHOLDER}");
        assert_eq!(scrub_secret_like(&line), want);
        let b64 = format!("Authorization: Bearer {}", "aB3+dE6/gH9=".repeat(2));
        assert_eq!(
            scrub_secret_like(&b64),
            format!("Authorization: Bearer {SECRET_PLACEHOLDER}")
        );
        for kept in ["the token budget is fine", "auth flow for d3963848ab"] {
            assert!(
                matches!(scrub_secret_like(kept), Cow::Borrowed(_)),
                "{kept:?}"
            );
        }
    }
    /// #10595: the JSON key's closing quote survives redaction.
    #[test]
    fn a_json_key_keeps_its_closing_quote() {
        let hex32 = "c0ffee12".repeat(4);
        assert_eq!(
            scrub_secret_like(&format!(r#"{{"token": "{hex32}"}}"#)),
            format!(r#"{{"token": "{SECRET_PLACEHOLDER}"}}"#)
        );
    }

    /// #10595: a JWT after a key is one secret, header through signature.
    #[test]
    fn a_jwt_is_redacted_across_its_dots() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpM";
        assert_eq!(
            scrub_secret_like(&format!("Authorization: Bearer {jwt}.")),
            format!("Authorization: Bearer {SECRET_PLACEHOLDER}.")
        );
    }

    /// #10595: a letter-only value after `=` or `:` is redacted like a mixed one.
    #[test]
    fn a_letter_only_value_after_a_separator_is_redacted() {
        let letters = "correcthorsebatterystaple";
        for sep in ["=", ": "] {
            assert_eq!(
                scrub_secret_like(&format!("password{sep}{letters}")),
                format!("password{sep}{SECRET_PLACEHOLDER}")
            );
        }
    }
}
