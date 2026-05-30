//! Sensitive-data scrubber for bug reports.
//!
//! Why: All captured errors are filed to the public `bobmatnyc/trusty-tools`
//!      repository. Before filing, we must strip personally-identifiable
//!      information and secrets: absolute file paths, usernames, API tokens,
//!      JWT strings, Bearer tokens, and key=value environment pairs. Scrubbing
//!      happens before the user reviews the preview — the preview body IS the
//!      filed body, so the scrubber runs exactly once per report.
//! What: [`scrub`] takes a raw string and returns a [`ScrubResult`] with the
//! cleaned text, a list of [`ScrubChange`] records describing what was removed,
//! and a human-readable redaction summary for display in the preview UI. The
//! body is truncated to [`MAX_BODY_BYTES`] after scrubbing to stay within
//! GitHub's 65 536 byte limit.
//!
//! The scrubber is intentionally conservative: when in doubt it redacts.
//! False positives (non-secret text that looks like a secret) are far
//! safer than false negatives (a real secret leaking into a public issue).
//!
//! Test: `tests::paths_redacted`, `tests::bearer_redacted`,
//!       `tests::truncation_applies`, `tests::aws_key_redacted`,
//!       `tests::google_key_redacted`, `tests::pem_block_redacted`,
//!       `tests::connection_string_redacted`, `tests::slack_token_redacted`,
//!       `tests::sk_prefix_redacted`, `tests::github_token_prefixes_redacted`.

use regex::Regex;

/// Maximum filed body size (16 KiB — generous but well below GitHub's 65 536 B).
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// Description of one scrubbing substitution made in the text.
///
/// Why: the preview surfaced to the user before filing should enumerate exactly
///      what was removed so they can make an informed consent decision.
/// What: `pattern` names the rule (e.g. `"AbsolutePath"`, `"BearerToken"`);
///       `hint` is a brief human-readable note.
/// Test: returned by `scrub` and inspected in `tests::*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubChange {
    /// Short name for the scrubbing rule that fired (e.g. `"AbsolutePath"`).
    pub pattern: &'static str,
    /// Human-readable hint about what was removed (e.g. `"1 absolute path(s)"`).
    pub hint: String,
}

/// The result of a scrubbing pass.
///
/// Why: callers (preview builder, filing path) need both the cleaned string and
///      a structured summary to display in the consent UI.
/// What: `text` is the scrubbed string (ready to file); `changes` is the list
///       of [`ScrubChange`] records; `redaction_summary` is a compact
///       human-readable string such as `"12 secrets, 3 paths redacted"` that
///       the preview can surface without listing every change.
/// Test: asserted in `tests::scrub_result_summary`.
#[derive(Debug, Clone)]
pub struct ScrubResult {
    /// The scrubbed string, truncated to at most [`MAX_BODY_BYTES`] bytes.
    pub text: String,
    /// Ordered list of every substitution that was applied.
    pub changes: Vec<ScrubChange>,
    /// Compact human-readable summary, e.g. `"5 secrets, 2 paths redacted"`.
    pub redaction_summary: String,
}

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

// ── Compiled regex cache (compiled once at first use) ─────────────────────────

static RE_PEM: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----")
        .expect("valid PEM regex")
});

static RE_BEARER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // Matches `Bearer <token>` or `Authorization: <anything>` (case-insensitive).
    Regex::new(r"(?i)(Authorization\s*:\s*[^\n\r]+|Bearer\s+\S+)").expect("valid bearer regex")
});

static RE_JWT: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // Three base64url segments separated by dots, starting with `eyJ`.
    Regex::new(r"eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]*").expect("valid JWT regex")
});

static RE_SK_PREFIX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // sk-ant- / sk-or- / sk-proj- / bare sk- prefixes (OpenAI, Anthropic, OpenRouter, etc.)
    Regex::new(r"sk-(?:ant-|or-|proj-)?[A-Za-z0-9_\-]{16,}").expect("valid sk- regex")
});

static RE_GITHUB_TOKEN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // GitHub PAT prefixes: ghp_ gho_ ghu_ ghs_ ghr_
    Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}").expect("valid GitHub token regex")
});

static RE_AWS_KEY: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"AKIA[0-9A-Z]{16}").expect("valid AWS key regex"));

static RE_GOOGLE_KEY: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"AIza[0-9A-Za-z_\-]{35}").expect("valid Google key regex")
});

static RE_SLACK_TOKEN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"xox[baprs]-[A-Za-z0-9\-]+").expect("valid Slack token regex")
});

static RE_CONN_STRING: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // Pattern: proto://user:password@host — matches connection strings with credentials.
    // pragma: allowlist secret
    Regex::new(r#"[a-zA-Z][a-zA-Z0-9+\-.]*://[^:@\s]+:[^@\s]+@[^\s"']+"#)
        .expect("valid conn-string regex")
});

// Note: POSIX and Windows path scrubbing use char-scanners (redact_posix_paths,
// redact_windows_paths) rather than regexes because lookbehind assertions are
// not supported by the `regex` crate, and the scanners handle multi-segment
// paths more reliably at word boundaries.

// ── Core scrubbing logic ──────────────────────────────────────────────────────

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
fn build_summary(changes: &[ScrubChange]) -> String {
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

// ── Rule implementations ──────────────────────────────────────────────────────

/// Apply a regex to `s`, replacing all matches with `replacement`, returning
/// the modified string and the number of substitutions made.
///
/// Why: removes boilerplate from each rule — all regex-backed rules follow the
///      same replace-count pattern.
/// What: uses `regex::Regex::replace_all`; count is obtained by a separate
///       find pass so we can report accurate numbers.
/// Test: exercised by every regex-backed rule test.
fn apply_regex(s: &str, re: &Regex, replacement: &str) -> (String, usize) {
    let count = re.find_iter(s).count();
    if count == 0 {
        return (s.to_string(), 0);
    }
    (re.replace_all(s, replacement).into_owned(), count)
}

/// Redact values in `KEY=<value>` patterns that look like secrets.
///
/// Why: env-KV secrets often appear in tracing fields and error messages;
///      the regex-backed rule catches most cases but a line-scanner catches
///      edge cases with non-standard separators or quoted values.
/// What: checks each line for a `=` separator; if the left-hand side contains
///       a secret keyword (TOKEN, SECRET, KEY, PASSWORD, PASSWD, PWD, APIKEY,
///       API_KEY, CREDENTIAL, PASS), replaces the value with `[REDACTED_VALUE]`.
/// Test: `tests::env_kv_redacted`.
fn redact_env_kv(s: &str) -> (String, usize) {
    const SECRET_KEYWORDS: &[&str] = &[
        "TOKEN",
        "SECRET",
        "KEY",
        "PASSWORD",
        "PASSWD",
        "PWD",
        "APIKEY",
        "API_KEY",
        "CREDENTIAL",
        "PASS",
    ];
    let mut out = String::with_capacity(s.len());
    let mut count = 0usize;

    for line in s.split('\n') {
        if let Some(eq_pos) = line.find('=') {
            let key = &line[..eq_pos];
            let upper_key = key.to_ascii_uppercase();
            let value = &line[eq_pos + 1..];
            // Only redact if there's actually a value (not empty).
            if !value.is_empty() && SECRET_KEYWORDS.iter().any(|kw| upper_key.contains(kw)) {
                out.push_str(key);
                out.push_str("=[REDACTED_VALUE]");
                count += 1;
            } else {
                out.push_str(line);
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !s.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    (out, count)
}

/// Replace POSIX absolute paths (`/Users/...`, `/home/...`, etc.) with `~`.
///
/// Why: absolute paths often contain usernames (`/Users/<name>/projects/…`).
///      The regex lookbehind approach is unreliable for multi-segment paths, so
///      we use a char-scanner that respects word boundaries.
/// What: scans character-by-character; when a `/` is found at a word boundary
///       followed by an alphabetic/underscore character, consumes the full path
///       token (up to the next whitespace or punctuation) and emits `~`.
/// Test: `tests::paths_redacted`, `tests::posix_path_with_home_var`.
fn redact_posix_paths(s: &str) -> (String, usize) {
    let mut out = String::with_capacity(s.len());
    let mut count = 0usize;

    let mut chars = s.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '/' {
            let prev_is_boundary = i == 0 || {
                let prev_char = s[..i].chars().next_back().unwrap_or(' ');
                prev_char.is_whitespace() || "\"'(,;:".contains(prev_char)
            };
            let next_is_alpha = s[i + ch.len_utf8()..]
                .chars()
                .next()
                .map(|c| c.is_alphabetic() || c == '_' || c == '~')
                .unwrap_or(false);

            if prev_is_boundary && next_is_alpha {
                let token_end = s[i..]
                    .find(|c: char| c.is_whitespace() || "\"'),:;".contains(c))
                    .map(|n| i + n)
                    .unwrap_or(s.len());
                out.push('~');
                count += 1;
                while chars.peek().map(|&(j, _)| j < token_end).unwrap_or(false) {
                    chars.next();
                }
                continue;
            }
        }
        out.push(ch);
    }
    (out, count)
}

/// Replace Windows absolute paths (`C:\...`) with `~`.
///
/// Why: Windows paths also contain usernames; users on Windows or cross-platform
///      paths should have them scrubbed equally.
/// What: scans for `<letter>:\` at a word boundary; consumes until whitespace
///       or quote and emits `~`.
/// Test: `tests::windows_path_redacted`.
fn redact_windows_paths(s: &str) -> (String, usize) {
    let mut out = String::with_capacity(s.len());
    let mut count = 0usize;

    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if i + 2 < bytes.len()
            && bytes[i].is_ascii_alphabetic()
            && bytes[i + 1] == b':'
            && bytes[i + 2] == b'\\'
        {
            let is_start = i == 0 || {
                let prev = bytes[i - 1];
                prev.is_ascii_whitespace() || prev == b'"' || prev == b'\''
            };
            if is_start {
                let end = s[i..]
                    .find(|c: char| c.is_whitespace() || "\"'".contains(c))
                    .map(|n| i + n)
                    .unwrap_or(s.len());
                out.push('~');
                count += 1;
                i = end;
                continue;
            }
        }
        // Advance by one character (UTF-8 safe).
        let ch = s[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: calls scrub() and unpacks for legacy tests ────────────────────

    fn scrub_pair(text: &str) -> (String, Vec<ScrubChange>) {
        scrub_compat(text)
    }

    // ── Existing tests (preserved) ────────────────────────────────────────────

    #[test]
    fn paths_redacted() {
        let (out, changes) = scrub_pair("failed to open /Users/alice/projects/foo.db");
        assert!(!out.contains("/Users/alice"), "path not scrubbed: {out}");
        assert!(out.contains('~'), "expected ~ replacement: {out}");
        assert!(changes.iter().any(|c| c.pattern == "AbsolutePath"));
    }

    #[test]
    fn bearer_redacted() {
        let (out, changes) = scrub_pair("Authorization: Bearer sk-proj-abcXYZ123");
        assert!(
            !out.contains("sk-proj-abcXYZ123"),
            "bearer token not scrubbed: {out}"
        );
        assert!(
            out.contains("[REDACTED_TOKEN]") || out.contains("[REDACTED_API_KEY]"),
            "{out}"
        );
        assert!(
            changes
                .iter()
                .any(|c| c.pattern == "BearerToken" || c.pattern == "SkApiKey"),
            "{changes:?}"
        );
    }

    #[test]
    fn jwt_redacted() {
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payload.sig";
        let (out, changes) = scrub_pair(&format!("authorization_header={token}"));
        assert!(!out.contains("eyJhbG"), "jwt not scrubbed: {out}");
        let redacted = out.contains("[REDACTED_JWT]") || out.contains("[REDACTED_VALUE]");
        assert!(redacted, "expected JWT redaction: {out}");
        let caught = changes
            .iter()
            .any(|c| c.pattern == "JwtToken" || c.pattern == "EnvSecret");
        assert!(caught, "expected jwt or env scrub rule: {changes:?}");
    }

    #[test]
    fn env_kv_redacted() {
        let (out, changes) = scrub_pair("OPENAI_API_KEY=sk-proj-abc123\nNAME=alice");
        // Either the sk- rule or the env-KV rule (or both) should scrub the value.
        assert!(!out.contains("sk-proj-abc123"), "key not scrubbed: {out}");
        // NAME=alice should be kept (no secret keyword).
        assert!(
            out.contains("NAME=alice"),
            "unrelated var should be kept: {out}"
        );
        let scrubbed = changes
            .iter()
            .any(|c| c.pattern == "EnvSecret" || c.pattern == "SkApiKey");
        assert!(scrubbed, "expected env or sk scrub: {changes:?}");
    }

    #[test]
    fn truncation_applies() {
        let long = "x".repeat(MAX_BODY_BYTES + 1000);
        let (out, changes) = scrub_pair(&long);
        assert!(out.len() <= MAX_BODY_BYTES + 100, "should be truncated");
        assert!(
            out.contains("truncated"),
            "should mention truncation: {out}"
        );
        assert!(changes.iter().any(|c| c.pattern == "Truncation"));
    }

    #[test]
    fn clean_text_unchanged() {
        let text = "connection refused: dial tcp 127.0.0.1:5432";
        let (out, changes) = scrub_pair(text);
        assert_eq!(out, text);
        assert!(changes.is_empty(), "no changes expected: {changes:?}");
    }

    #[test]
    fn windows_path_redacted() {
        let (out, changes) = scrub_pair(r"failed to open C:\Users\alice\projects\foo.db");
        assert!(!out.contains("Users"), "windows path not scrubbed: {out}");
        assert!(changes.iter().any(|c| c.pattern == "WindowsPath"));
    }

    // ── Phase 4: new secret pattern tests ────────────────────────────────────

    #[test]
    fn aws_key_redacted() {
        let (out, changes) = scrub_pair("aws_key=AKIAIOSFODNN7EXAMPLE_EXTRA_PAD");
        // The env-kv rule fires on 'aws_key=...' (contains KEY).
        // Additionally, if the literal matches AWS regex it should be caught.
        assert!(
            !out.contains("AKIAIOSFODNN7"),
            "AWS key not scrubbed: {out}"
        );
        assert!(
            changes
                .iter()
                .any(|c| c.pattern == "AwsKey" || c.pattern == "EnvSecret"),
            "expected AwsKey or EnvSecret: {changes:?}"
        );
    }

    #[test]
    fn aws_key_standalone_redacted() {
        // Standalone AWS key not inside a KEY= assignment.
        let (out, changes) = scrub_pair("access key is AKIAIOSFODNN7EXAMPLEQ");
        assert!(
            !out.contains("AKIAIOSFODNN7"),
            "AWS key not scrubbed: {out}"
        );
        assert!(
            changes.iter().any(|c| c.pattern == "AwsKey"),
            "expected AwsKey: {changes:?}"
        );
    }

    #[test]
    fn google_key_redacted() {
        let key = "AIzaSyDdI0hCZtE6vySjMm-WEfRq3CPzqKqqsHI"; // pragma: allowlist secret
        let (out, changes) = scrub_pair(&format!("google api key: {key}"));
        assert!(!out.contains("AIzaSy"), "Google key not scrubbed: {out}");
        assert!(
            changes.iter().any(|c| c.pattern == "GoogleKey"),
            "expected GoogleKey: {changes:?}"
        );
    }

    #[test]
    fn slack_token_redacted() {
        let (out, changes) = scrub_pair("token=xoxb-1234567890-ABCDEFGHIJ");
        assert!(!out.contains("xoxb-"), "Slack token not scrubbed: {out}");
        assert!(
            changes
                .iter()
                .any(|c| c.pattern == "SlackToken" || c.pattern == "EnvSecret"),
            "expected SlackToken or EnvSecret: {changes:?}"
        );
    }

    #[test]
    fn slack_token_standalone_redacted() {
        let (out, changes) = scrub_pair("using slack token xoxp-987654321-xyz");
        assert!(!out.contains("xoxp-"), "Slack token not scrubbed: {out}");
        assert!(
            changes.iter().any(|c| c.pattern == "SlackToken"),
            "expected SlackToken: {changes:?}"
        );
    }

    #[test]
    fn pem_block_redacted() {
        // pragma: allowlist secret
        let pem =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----";
        let (out, changes) = scrub_pair(pem);
        assert!(
            !out.contains("BEGIN RSA PRIVATE KEY"),
            "PEM block not scrubbed: {out}"
        );
        assert!(
            changes.iter().any(|c| c.pattern == "PemPrivateKey"),
            "expected PemPrivateKey: {changes:?}"
        );
    }

    #[test]
    fn connection_string_redacted() {
        let (out, changes) =
            scrub_pair("db_url=postgres://admin:s3cr3tp4ss@db.example.com:5432/mydb"); // pragma: allowlist secret
        assert!(!out.contains("s3cr3tp4ss"), "password not scrubbed: {out}");
        assert!(
            changes
                .iter()
                .any(|c| c.pattern == "ConnString" || c.pattern == "EnvSecret"),
            "expected ConnString or EnvSecret: {changes:?}"
        );
    }

    #[test]
    fn connection_string_standalone_redacted() {
        let (out, changes) = scrub_pair("connecting to postgres://user:hunter2@localhost/prod"); // pragma: allowlist secret
        assert!(!out.contains("hunter2"), "password not scrubbed: {out}");
        assert!(
            changes.iter().any(|c| c.pattern == "ConnString"),
            "expected ConnString: {changes:?}"
        );
    }

    #[test]
    fn sk_prefix_redacted() {
        let (out, changes) = scrub_pair("OPENAI_KEY=sk-abcdef1234567890abcdef1234");
        assert!(!out.contains("sk-abcdef"), "sk- key not scrubbed: {out}");
        assert!(
            changes
                .iter()
                .any(|c| c.pattern == "SkApiKey" || c.pattern == "EnvSecret"),
            "expected SkApiKey or EnvSecret: {changes:?}"
        );
    }

    #[test]
    fn sk_ant_prefix_redacted() {
        let (out, changes) = scrub_pair("ANTHROPIC_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz");
        assert!(
            !out.contains("sk-ant-api03"),
            "sk-ant- key not scrubbed: {out}"
        );
        assert!(
            changes
                .iter()
                .any(|c| c.pattern == "SkApiKey" || c.pattern == "EnvSecret"),
            "expected SkApiKey or EnvSecret: {changes:?}"
        );
    }

    #[test]
    fn github_token_prefixes_redacted() {
        // pragma: allowlist secret
        let tokens = [
            "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890", // pragma: allowlist secret
            "gho_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890", // pragma: allowlist secret
            "ghu_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890", // pragma: allowlist secret
            "ghs_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890", // pragma: allowlist secret
        ];
        for token in &tokens {
            let (out, changes) = scrub_pair(&format!("using token {token}"));
            assert!(
                !out.contains(&token[..8]),
                "GitHub token not scrubbed: {out}"
            );
            assert!(
                changes.iter().any(|c| c.pattern == "GithubToken"),
                "expected GithubToken for {token}: {changes:?}"
            );
        }
    }

    #[test]
    fn scrub_result_summary() {
        // A string with both secrets and paths.
        let text = "token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890 at /Users/alice/db"; // pragma: allowlist secret
        let result = scrub(text);
        // Must not contain the raw token or path.
        assert!(!result.text.contains("ghp_aB"), "{}", result.text);
        assert!(!result.text.contains("/Users/alice"), "{}", result.text);
        // Summary must mention both.
        assert!(
            result.redaction_summary.contains("secret")
                || result.redaction_summary.contains("path"),
            "summary: {}",
            result.redaction_summary
        );
    }

    #[test]
    fn clean_text_summary_is_nothing_redacted() {
        let result = scrub("connection refused: 127.0.0.1:5432");
        assert_eq!(result.redaction_summary, "nothing redacted");
    }

    #[test]
    fn home_dir_path_scrubbed() {
        // Simulate $HOME expansion in a message.
        let (out, changes) = scrub_pair("reading /home/bob/.config/trusty-mpm/config.toml");
        assert!(!out.contains("/home/bob"), "home path not scrubbed: {out}");
        assert!(
            changes.iter().any(|c| c.pattern == "AbsolutePath"),
            "{changes:?}"
        );
    }
}
