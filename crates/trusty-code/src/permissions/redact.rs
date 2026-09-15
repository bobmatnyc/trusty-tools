//! Credential scrubbing for a published permission subject (#7948).
//!
//! Why: an `Event::PermissionRequested` carries the command an operator is
//! asked to approve, which routinely carries the credential it needs
//! (`GITHUB_TOKEN=ghp_... gh pr create`). The event reaches every attached
//! client and the session ring buffer, so the credential must not survive.
//! What: `redact_subject`.
//! Test: `permissions::tests::gate_tests::redacts_*`.

use std::sync::LazyLock;

use regex::Regex;

/// How many characters of a subject reach the event stream.
const SUBJECT_PREVIEW_CHARS: usize = 500;

/// Redact credential-shaped text out of a subject, then bound its length.
///
/// Why: see the module docs.
/// What: replaces the VALUE of a `NAME=value` assignment whose name looks like
/// a secret (token/secret/password/key/auth/credential, any case) or is an
/// all-caps environment-variable name, and the value of an `Authorization:`,
/// `Proxy-Authorization:`, or `X-Api-Key:` header; then bounds the result to
/// 500 characters. A best-effort scrub of the SHAPES a credential is passed
/// in, not a secret scanner — a positional secret (`curl -u user:pass`) is not
/// detectable from text alone.
/// Test: `redacts_a_lowercase_token_assignment`,
/// `redacts_an_uppercase_env_assignment`, `redacts_an_authorization_header`,
/// `leaves_an_ordinary_command_untouched`, `bounds_the_subject_to_500_chars`.
pub fn redact_subject(subject: &str) -> String {
    static SECRET_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\b([A-Za-z0-9_.-]*(?:token|secret|password|passwd|api[_-]?key|apikey|auth|credential)[A-Za-z0-9_.-]*)=(\S+)",
        )
        .expect("static redaction pattern compiles")
    });
    static ENV_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b([A-Z][A-Z0-9_]{2,})=(\S+)").expect("static redaction pattern compiles")
    });
    // The header value runs to the closing quote, not the first space:
    // in `Authorization: Bearer sk-live-xyz` the SECOND token is the secret.
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("(?i)\\b(authorization|x-api-key|proxy-authorization):[^'\"\n]*")
            .expect("static redaction pattern compiles")
    });

    let redacted = SECRET_ASSIGNMENT.replace_all(subject, "$1=<redacted>");
    let redacted = ENV_ASSIGNMENT.replace_all(&redacted, "$1=<redacted>");
    let redacted = HEADER.replace_all(&redacted, "$1: <redacted>");
    crate::events::preview(&redacted, SUBJECT_PREVIEW_CHARS)
}
