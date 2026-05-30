//! Sensitive-data scrubber for bug reports.
//!
//! Why: All captured errors are filed to the public `bobmatnyc/trusty-tools`
//!      repository. Before filing, we must strip personally-identifiable
//!      information and secrets: absolute file paths, usernames, API tokens,
//!      JWT strings, Bearer tokens, and key=value environment pairs. Scrubbing
//!      happens before the user reviews the preview — the preview body IS the
//!      filed body, so the scrubber runs exactly once per report.
//! What: [`scrub`] takes a raw string (message, fields, or code location) and
//!       returns a `(String, Vec<ScrubChange>)` tuple — the cleaned string and
//!       a log of what was replaced. [`ScrubChange`] records the pattern name
//!       and a hint about what was removed so the preview can tell the user.
//!       The body is also truncated to [`MAX_BODY_BYTES`] bytes after scrubbing
//!       to stay within GitHub's 65 536 byte limit.
//! Test: `tests::paths_redacted`, `tests::bearer_redacted`,
//!       `tests::truncation_applies`.

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

/// Scrub a string of sensitive data, returning the cleaned string and a log of
/// what changed.
///
/// Why: all user-facing text in a bug report must be scrubbed before filing.
///      The caller applies this to each field (message, fields, file path,
///      body markdown) before building the preview and before filing.
/// What: applies the following rules in order:
///   1. `Bearer <token>` / `Authorization: ...` → `[REDACTED_TOKEN]`
///   2. `eyJ...` JWT-shaped strings → `[REDACTED_JWT]`
///   3. Key=value pairs that look like env secrets → `[REDACTED_VALUE]`
///   4. Absolute POSIX paths (`/Users/`, `/home/`) → `~`
///   5. Absolute Windows-style paths (`C:\`, `D:\`) → `~`
///
///      Then truncates to [`MAX_BODY_BYTES`] UTF-8 bytes.
///
/// Test: `tests::paths_redacted`, `tests::bearer_redacted`, `tests::jwt_redacted`,
///       `tests::env_kv_redacted`, `tests::truncation_applies`.
pub fn scrub(text: &str) -> (String, Vec<ScrubChange>) {
    let mut result = text.to_string();
    let mut changes: Vec<ScrubChange> = Vec::new();

    // Rule 1: Bearer / Authorization tokens.
    let (r, n) = redact_bearer(&result);
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "BearerToken",
            hint: format!("{n} bearer/auth token(s) redacted"),
        });
    }

    // Rule 2: JWT-shaped strings (eyJ....<base64>).
    let (r, n) = redact_jwt(&result);
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "JwtToken",
            hint: format!("{n} JWT string(s) redacted"),
        });
    }

    // Rule 3: Environment-style key=value secrets.
    let (r, n) = redact_env_kv(&result);
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "EnvSecret",
            hint: format!("{n} key=value secret(s) redacted"),
        });
    }

    // Rule 4: Absolute POSIX paths.
    let (r, n) = redact_posix_paths(&result);
    if n > 0 {
        result = r;
        changes.push(ScrubChange {
            pattern: "AbsolutePath",
            hint: format!("{n} absolute path(s) replaced with ~"),
        });
    }

    // Rule 5: Windows absolute paths (C:\...).
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
        // Truncate at a valid UTF-8 boundary.
        let mut boundary = MAX_BODY_BYTES;
        while !result.is_char_boundary(boundary) {
            boundary -= 1;
        }
        result.truncate(boundary);
        result.push_str("\n\n[... truncated — body exceeded 16 KiB ...]");
        changes.push(ScrubChange {
            pattern: "Truncation",
            hint: format!("body truncated to {} bytes", MAX_BODY_BYTES),
        });
    }

    (result, changes)
}

// ── Rule implementations ──────────────────────────────────────────────────────

/// Replace `Bearer <token>` and `Authorization: <value>` patterns.
fn redact_bearer(s: &str) -> (String, usize) {
    // Simple line-by-line scan; avoids pulling in a full regex engine.
    let mut out = String::with_capacity(s.len());
    let mut count = 0usize;
    for line in s.split('\n') {
        let lower = line.to_ascii_lowercase();
        if lower.contains("bearer ") || lower.starts_with("authorization:") {
            out.push_str("[REDACTED_TOKEN]");
            count += 1;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    // Trim the trailing newline we added after the last line if the original
    // did not end with one.
    if !s.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    (out, count)
}

/// Replace `eyJ...` JWT strings.
fn redact_jwt(s: &str) -> (String, usize) {
    let mut out = String::with_capacity(s.len());
    let mut count = 0usize;
    let marker = "eyJ";
    let mut remaining = s;
    while let Some(pos) = remaining.find(marker) {
        out.push_str(&remaining[..pos]);
        // Find the end of the JWT (first whitespace or quote after the marker).
        let token_start = pos;
        let rest = &remaining[pos..];
        let token_end = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        out.push_str("[REDACTED_JWT]");
        count += 1;
        remaining = &remaining[token_start + token_end..];
    }
    out.push_str(remaining);
    (out, count)
}

/// Redact values in `KEY=<value>` patterns that look like secrets.
///
/// Heuristic: the key contains `TOKEN`, `SECRET`, `KEY`, `PASSWORD`, `PASS`,
/// `APIKEY`, `API_KEY`, or `CREDENTIAL` (case-insensitive) and is followed by
/// `=<non-whitespace>`.
fn redact_env_kv(s: &str) -> (String, usize) {
    let secret_keywords = &[
        "TOKEN",
        "SECRET",
        "KEY",
        "PASSWORD",
        "PASS",
        "APIKEY",
        "API_KEY",
        "CREDENTIAL",
    ];
    let mut out = String::with_capacity(s.len());
    let mut count = 0usize;

    for line in s.split('\n') {
        if let Some(eq_pos) = line.find('=') {
            let key = &line[..eq_pos];
            let upper_key = key.to_ascii_uppercase();
            if secret_keywords.iter().any(|kw| upper_key.contains(kw)) {
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

/// Replace POSIX absolute paths (`/Users/...`, `/home/...`, `/root/...`, any
/// `/something/something`) with `~`.
fn redact_posix_paths(s: &str) -> (String, usize) {
    // Split on whitespace boundaries and replace tokens that look like absolute
    // paths. We avoid a full regex to keep dependencies minimal.
    let mut out = String::with_capacity(s.len());
    let mut count = 0usize;

    let mut chars = s.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '/' {
            // Check that the preceding character (if any) is a word boundary.
            let prev_is_boundary = i == 0 || {
                let prev_char = s[..i].chars().next_back().unwrap_or(' ');
                prev_char.is_whitespace() || "\"'(,;:".contains(prev_char)
            };
            // Peek ahead: next char must be alphabetic or `~` (path component).
            let next_is_alpha = s[i + 1..]
                .chars()
                .next()
                .map(|c| c.is_alphabetic() || c == '_' || c == '~')
                .unwrap_or(false);

            if prev_is_boundary && next_is_alpha {
                // Consume the rest of this path token.
                let token_end = s[i..]
                    .find(|c: char| c.is_whitespace() || "\"'),:;".contains(c))
                    .map(|n| i + n)
                    .unwrap_or(s.len());
                out.push('~');
                count += 1;
                // Advance `chars` past the path token.
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
fn redact_windows_paths(s: &str) -> (String, usize) {
    let mut out = String::with_capacity(s.len());
    let mut count = 0usize;

    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        // Look for `<letter>:\` at the start or after a whitespace / quote.
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
                // Consume until whitespace or quote.
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
        out.push(s[i..].chars().next().unwrap_or('\0'));
        i += s[i..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_redacted() {
        let (out, changes) = scrub("failed to open /Users/alice/projects/foo.db");
        assert!(!out.contains("/Users/alice"), "path not scrubbed: {out}");
        assert!(out.contains('~'), "expected ~ replacement: {out}");
        assert!(changes.iter().any(|c| c.pattern == "AbsolutePath"));
    }

    #[test]
    fn bearer_redacted() {
        let (out, changes) = scrub("Authorization: Bearer sk-proj-abcXYZ123");
        assert!(!out.contains("sk-proj"), "bearer token not scrubbed: {out}");
        assert!(out.contains("[REDACTED_TOKEN]"), "{out}");
        assert!(changes.iter().any(|c| c.pattern == "BearerToken"));
    }

    #[test]
    fn jwt_redacted() {
        // Use a context that doesn't contain secret-keyword in the key name so
        // the env-kv rule doesn't fire first (it runs before the JWT rule).
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payload.sig";
        let (out, changes) = scrub(&format!("authorization_header={token}"));
        assert!(!out.contains("eyJhbG"), "jwt not scrubbed: {out}");
        // Either the JWT was caught by JwtToken or EnvSecret (both are valid;
        // what matters is that the raw eyJ... token is NOT in the output).
        let redacted = out.contains("[REDACTED_JWT]") || out.contains("[REDACTED_VALUE]");
        assert!(redacted, "expected JWT redaction: {out}");
        let caught_by_jwt_or_env = changes
            .iter()
            .any(|c| c.pattern == "JwtToken" || c.pattern == "EnvSecret");
        assert!(
            caught_by_jwt_or_env,
            "expected jwt or env scrub rule: {changes:?}"
        );
    }

    #[test]
    fn env_kv_redacted() {
        let (out, changes) = scrub("OPENAI_API_KEY=sk-proj-abc123\nNAME=alice");
        assert!(!out.contains("sk-proj-abc123"), "key not scrubbed: {out}");
        assert!(out.contains("[REDACTED_VALUE]"), "{out}");
        // OPENAI_API_KEY contains KEY → redacted; NAME does not → kept.
        assert!(
            out.contains("NAME=alice"),
            "unrelated var should be kept: {out}"
        );
        assert!(changes.iter().any(|c| c.pattern == "EnvSecret"));
    }

    #[test]
    fn truncation_applies() {
        // Generate a body that exceeds MAX_BODY_BYTES.
        let long = "x".repeat(MAX_BODY_BYTES + 1000);
        let (out, changes) = scrub(&long);
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
        let (out, changes) = scrub(text);
        assert_eq!(out, text);
        assert!(changes.is_empty(), "no changes expected: {changes:?}");
    }

    #[test]
    fn windows_path_redacted() {
        let (out, changes) = scrub(r"failed to open C:\Users\alice\projects\foo.db");
        assert!(!out.contains("Users"), "windows path not scrubbed: {out}");
        assert!(changes.iter().any(|c| c.pattern == "WindowsPath"));
    }
}
