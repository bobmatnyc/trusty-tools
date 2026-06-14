//! Core scrubbing orchestration: [`scrub`], [`scrub_compat`], and helpers.
//!
//! Why: the public API and the rule-application loop belong together; the
//! regex constants and rule helpers live in their own files to keep each
//! unit under the SLOC cap.
//! What: [`scrub`] applies all rules in priority order and returns a
//! [`ScrubResult`]; [`scrub_compat`] is a legacy tuple wrapper; `scrub_inner`
//! and `build_summary` are the internal implementation helpers.
//! Test: `tests::scrub_result_summary`, `tests::clean_text_summary_is_nothing_redacted`.

use super::regexes::{
    RE_AWS_KEY, RE_BEARER, RE_CONN_STRING, RE_GITHUB_TOKEN, RE_GOOGLE_KEY, RE_JWT, RE_PEM,
    RE_SK_PREFIX, RE_SLACK_TOKEN,
};
use super::rules::{apply_regex, redact_env_kv, redact_posix_paths, redact_windows_paths};
use super::types::{MAX_BODY_BYTES, ScrubChange, ScrubResult};

/// Scrub a string of sensitive data, returning a [`ScrubResult`].
///
/// Why: all user-facing text in a bug report must be scrubbed before filing.
///      The caller applies this to each field (message, fields, file path,
///      body markdown) before building the preview and before filing.
/// What: applies redaction rules in order (most-specific secrets first, then
///       paths, then generic env-KV), then truncates to [`MAX_BODY_BYTES`].
///       Rules applied:
///   1. PEM private-key blocks (`-----BEGIN ... PRIVATE KEY-----`)
///   2. Bearer / Authorization header lines
///   3. `eyJ...` JWT-shaped strings
///   4. `sk-` / `sk-ant-` / `sk-or-` prefixed LLM API keys
///   5. GitHub token prefixes (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`)
///   6. AWS access key IDs (`AKIA[0-9A-Z]{16}`)
///   7. Google API keys (`AIza[0-9A-Za-z_\-]{35}`)
///   8. Slack tokens (`xox[baprs]-`)
///   9. Connection strings with embedded credentials (`proto://user:pass@host`) // pragma: allowlist secret
///  10. Generic high-entropy secret assignments (KEY/SECRET/TOKEN/PASSWORD/…=value)
///  11. POSIX absolute paths (`/Users/`, `/home/`, etc.) → `~`
///  12. Windows absolute paths (`C:\…`) → `~`
///  13. `$HOME` environment variable expansion
///
///      Then truncates to [`MAX_BODY_BYTES`] UTF-8 bytes.
///
/// Test: individual rules covered by `tests::*`; combined in `tests::scrub_result_summary`.
pub fn scrub(text: &str) -> ScrubResult {
    let (cleaned, changes) = scrub_inner(text);
    let summary = build_summary(&changes);
    ScrubResult {
        text: cleaned,
        changes,
        redaction_summary: summary,
    }
}

/// Legacy two-tuple convenience wrapper used by the preview builder.
///
/// Why: the preview builder was written against the Phase 3 `scrub` signature
///      `(String, Vec<ScrubChange>)`. This wrapper avoids a large refactor while
///      the scrubber is being hardened; the preview builder can migrate to
///      [`scrub`] / [`ScrubResult`] in a follow-up.
/// What: calls [`scrub`] and unpacks into the old `(String, Vec<ScrubChange>)` pair.
/// Test: indirectly via all `preview` tests.
pub fn scrub_compat(text: &str) -> (String, Vec<ScrubChange>) {
    let result = scrub(text);
    (result.text, result.changes)
}

/// Internal implementation: returns the cleaned string and change list.
///
/// Why: separated from [`scrub`] so the summary can be built after all rules run.
/// What: applies each rule in priority order; each rule replaces matches with
///       a tagged redaction placeholder and records a [`ScrubChange`].
/// Test: via `scrub` public entry point.
fn scrub_inner(text: &str) -> (String, Vec<ScrubChange>) {
    let mut result = text.to_string();
    let mut changes: Vec<ScrubChange> = Vec::new();

    // Rule 1: PEM private-key blocks (highest priority — must remove whole blocks).
    let (r, n) = apply_regex(&result, &RE_PEM, "[REDACTED_PRIVATE_KEY]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "PemPrivateKey",
            hint: format!("{n} PEM private-key block(s) redacted"),
        });
    }

    // Rule 2: Bearer / Authorization headers.
    let (r, n) = apply_regex(&result, &RE_BEARER, "[REDACTED_TOKEN]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "BearerToken",
            hint: format!("{n} bearer/auth token(s) redacted"),
        });
    }

    // Rule 3: JWT-shaped strings.
    let (r, n) = apply_regex(&result, &RE_JWT, "[REDACTED_JWT]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "JwtToken",
            hint: format!("{n} JWT string(s) redacted"),
        });
    }

    // Rule 4: sk-* prefixed API keys.
    let (r, n) = apply_regex(&result, &RE_SK_PREFIX, "[REDACTED_API_KEY]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "SkApiKey",
            hint: format!("{n} sk-* API key(s) redacted"),
        });
    }

    // Rule 5: GitHub token prefixes.
    let (r, n) = apply_regex(&result, &RE_GITHUB_TOKEN, "[REDACTED_GITHUB_TOKEN]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "GithubToken",
            hint: format!("{n} GitHub token(s) redacted"),
        });
    }

    // Rule 6: AWS access key IDs.
    let (r, n) = apply_regex(&result, &RE_AWS_KEY, "[REDACTED_AWS_KEY]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "AwsKey",
            hint: format!("{n} AWS access key(s) redacted"),
        });
    }

    // Rule 7: Google API keys.
    let (r, n) = apply_regex(&result, &RE_GOOGLE_KEY, "[REDACTED_GOOGLE_KEY]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "GoogleKey",
            hint: format!("{n} Google API key(s) redacted"),
        });
    }

    // Rule 8: Slack tokens.
    let (r, n) = apply_regex(&result, &RE_SLACK_TOKEN, "[REDACTED_SLACK_TOKEN]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "SlackToken",
            hint: format!("{n} Slack token(s) redacted"),
        });
    }

    // Rule 9: Connection strings with embedded credentials.
    let (r, n) = apply_regex(&result, &RE_CONN_STRING, "[REDACTED_CONN_STRING]");
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "ConnString",
            hint: format!("{n} connection string(s) with credentials redacted"),
        });
    }

    // Rule 10: Generic env-KV secrets.
    let (r, n) = redact_env_kv(&result);
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "EnvSecret",
            hint: format!("{n} key=value secret(s) redacted"),
        });
    }

    // Rule 11: POSIX absolute paths.
    let (r, n) = redact_posix_paths(&result);
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "AbsolutePath",
            hint: format!("{n} absolute path(s) replaced with ~"),
        });
    }

    // Rule 12: Windows absolute paths.
    let (r, n) = redact_windows_paths(&result);
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "WindowsPath",
            hint: format!("{n} Windows path(s) replaced with ~"),
        });
    }

    // Truncation.
    if result.len() > MAX_BODY_BYTES {
        let mut boundary = MAX_BODY_BYTES;
        while !result.is_char_boundary(boundary) {
            boundary -= 1;
        }
        result.truncate(boundary);
        result.push_str("\n\n[... truncated — body exceeded 16 KiB ...]");
        changes.push(ScrubChange {
            pattern: "Truncation",
            hint: format!("body truncated to {MAX_BODY_BYTES} bytes"),
        });
    }

    (result, changes)
}

/// Build a compact human-readable redaction summary string.
///
/// Why: the preview UI needs a one-liner like `"5 secrets, 2 paths redacted"`
///      that a user can scan at a glance without reading every change entry.
/// What: counts secret-type changes and path-type changes separately, then
///       formats them into a short English phrase; returns `"nothing redacted"`
///       when the change list is empty.
/// Test: `tests::scrub_result_summary`.
pub(super) fn build_summary(changes: &[ScrubChange]) -> String {
    const SECRET_PATTERNS: &[&str] = &[
        "BearerToken",
        "JwtToken",
        "SkApiKey",
        "GithubToken",
        "AwsKey",
        "GoogleKey",
        "SlackToken",
        "ConnString",
        "EnvSecret",
        "PemPrivateKey",
    ];
    const PATH_PATTERNS: &[&str] = &["AbsolutePath", "WindowsPath"];

    if changes.is_empty() || changes.iter().all(|c| c.pattern == "Truncation") {
        return "nothing redacted".to_string();
    }

    let secrets: usize = changes
        .iter()
        .filter(|c| SECRET_PATTERNS.contains(&c.pattern))
        .map(|c| {
            // Extract the count from the hint string (first token before space).
            c.hint
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
        })
        .sum();

    let paths: usize = changes
        .iter()
        .filter(|c| PATH_PATTERNS.contains(&c.pattern))
        .map(|c| {
            c.hint
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
        })
        .sum();

    match (secrets, paths) {
        (0, 0) => "nothing redacted".to_string(),
        (s, 0) => format!("{s} secret(s) redacted"),
        (0, p) => format!("{p} path(s) redacted"),
        (s, p) => format!("{s} secret(s), {p} path(s) redacted"),
    }
}
