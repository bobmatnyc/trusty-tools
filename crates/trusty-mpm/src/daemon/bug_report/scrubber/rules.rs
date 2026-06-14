//! Rule implementations for the scrubber.
//!
//! Why: the regex-application helper and the three char-scanner rules
//! (env-KV, POSIX paths, Windows paths) are logically distinct from the
//! top-level orchestration in `core.rs`; isolating them keeps each file
//! focused and under the SLOC cap.
//! What: [`apply_regex`], [`redact_env_kv`], [`redact_posix_paths`],
//! [`redact_windows_paths`].
//! Test: exercised via the public `scrub`/`scrub_compat` entry points in
//! `super::tests`.

use regex::Regex;

/// Apply a regex to `s`, replacing all matches with `replacement`, returning
/// the modified string and the number of substitutions made.
///
/// Why: removes boilerplate from each rule — all regex-backed rules follow the
///      same replace-count pattern.
/// What: uses `regex::Regex::replace_all`; count is obtained by a separate
///       find pass so we can report accurate numbers.
/// Test: exercised by every regex-backed rule test.
pub(super) fn apply_regex(s: &str, re: &Regex, replacement: &str) -> (String, usize) {
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
pub(super) fn redact_env_kv(s: &str) -> (String, usize) {
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
pub(super) fn redact_posix_paths(s: &str) -> (String, usize) {
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
pub(super) fn redact_windows_paths(s: &str) -> (String, usize) {
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
