//! Compiled regex constants for the scrubber.
//!
//! Why: each regex is compiled exactly once at first use via `LazyLock`,
//! keeping the hot scrubbing path allocation-free.
//! What: one `static` per secret pattern (PEM, Bearer, JWT, sk-*, GitHub, AWS,
//! Google, Slack, connection-string). Path scrubbing uses char-scanners instead
//! of regexes because the `regex` crate lacks lookbehind support.
//! Test: each regex is exercised by the corresponding test in `super::tests`.

use regex::Regex;

pub(super) static RE_PEM: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----")
        .expect("valid PEM regex")
});

pub(super) static RE_BEARER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // Matches `Bearer <token>` or `Authorization: <anything>` (case-insensitive).
    Regex::new(r"(?i)(Authorization\s*:\s*[^\n\r]+|Bearer\s+\S+)").expect("valid bearer regex")
});

pub(super) static RE_JWT: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // Three base64url segments separated by dots, starting with `eyJ`.
    Regex::new(r"eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]*").expect("valid JWT regex")
});

pub(super) static RE_SK_PREFIX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // sk-ant- / sk-or- / sk-proj- / bare sk- prefixes (OpenAI, Anthropic, OpenRouter, etc.)
    Regex::new(r"sk-(?:ant-|or-|proj-)?[A-Za-z0-9_\-]{16,}").expect("valid sk- regex")
});

pub(super) static RE_GITHUB_TOKEN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // GitHub PAT prefixes: ghp_ gho_ ghu_ ghs_ ghr_
    Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}").expect("valid GitHub token regex")
});

pub(super) static RE_AWS_KEY: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"AKIA[0-9A-Z]{16}").expect("valid AWS key regex"));

pub(super) static RE_GOOGLE_KEY: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"AIza[0-9A-Za-z_\-]{35}").expect("valid Google key regex")
});

pub(super) static RE_SLACK_TOKEN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"xox[baprs]-[A-Za-z0-9\-]+").expect("valid Slack token regex") // pragma: allowlist secret
});

pub(super) static RE_CONN_STRING: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // Pattern: proto://user:password@host — matches connection strings with credentials. // pragma: allowlist secret
    Regex::new(r#"[a-zA-Z][a-zA-Z0-9+\-.]*://[^:@\s]+:[^@\s]+@[^\s"']+"#)
        .expect("valid conn-string regex")
});
