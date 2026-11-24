//! The redaction pass of a clipboard read (#11696 Phase C).
//!
//! Why: the clipboard is where people put passwords, API keys and tokens on
//! their way into a form or a terminal. A granted read must not hand those to
//! the agent, so every read is cleaned before it leaves the broker.
//! What: [`redact_text`] replaces each secret it finds with [`MARKER`] and
//! counts them. It finds a PEM private key block; the value of a secret-named
//! key (`password=`, `pwd:`, `"api_key": "..."`, `AWS_SECRET_ACCESS_KEY=`);
//! the credential after `Bearer`, or after `Basic` / `Token` as an
//! `Authorization` value; and a word with a
//! known secret shape (AWS access key ids, `sk-`, `ghp_`, `github_pat_`,
//! `xox?-`, `glpat-`, `AIza` keys, JWTs). A value is one word, the run of
//! characters up to whitespace, a quote, or one of `,;()[]{}<>|&?`; a
//! passphrase with spaces keeps its later words.
//! Test: `redaction_strips_passwords_and_keys`, `redaction_keeps_plain_text`.

/// What a redacted secret is replaced with.
pub const MARKER: &str = "[REDACTED]";

/// Text with its secrets replaced, and how many were replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub removed: usize,
}

/// Key names whose value is a secret, matched as a suffix of the lowercased key.
const SECRET_KEYS: [&str; 16] = [
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "access_key",
    "accesskey",
    "private_key",
    "privatekey",
    "credential",
    "credentials",
    "authorization",
    "client_secret",
];
/// Short key names, matched only whole or after `_`, `-` or `.`.
const SHORT_KEYS: [&str; 4] = ["pwd", "pass", "pw", "auth"];
/// Auth schemes whose next word is a credential after an `Authorization:` value.
const SCHEMES: [&str; 3] = ["bearer", "basic", "token"];
/// The scheme whose next word is a credential anywhere (`curl -H Bearer x`).
const BARE_SCHEME: &str = "bearer";
/// Prefixes of provider keys, each followed by at least [`MIN_TAIL`] key characters.
const KEY_PREFIXES: [&str; 13] = [
    "sk-",
    "sk_live_",
    "rk_live_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "glpat-",
    "AIza",
];
/// The fewest key characters after a [`KEY_PREFIXES`] entry.
const MIN_TAIL: usize = 16;

fn is_separator(c: char) -> bool {
    c.is_whitespace() || "\"'`,;()[]{}<>|&?".contains(c)
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// True when `key` (already lowercased, punctuation trimmed) names a secret.
fn secret_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    if SECRET_KEYS.iter().any(|k| key.ends_with(k)) {
        return true;
    }
    SHORT_KEYS.iter().any(|k| {
        key == *k
            || key
                .strip_suffix(k)
                .is_some_and(|head| head.ends_with(['_', '-', '.']))
    })
}

/// The lowercased key part of `word`, without leading punctuation.
fn key_of(word: &str) -> String {
    word.trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_ascii_lowercase()
}

/// True when `word` has the shape of a provider key, an AWS key id or a JWT.
fn secret_shape(word: &str) -> bool {
    let tail_ok = |tail: &str| tail.len() >= MIN_TAIL && tail.chars().all(is_key_char);
    if KEY_PREFIXES
        .iter()
        .any(|p| word.strip_prefix(p).is_some_and(tail_ok))
    {
        return true;
    }
    let aws = ["AKIA", "ASIA"].iter().any(|p| {
        word.strip_prefix(p).is_some_and(|t| {
            t.len() >= 16
                && t.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
    });
    let jwt = word.starts_with("eyJ") && {
        let parts: Vec<&str> = word.split('.').collect();
        parts.len() == 3
            && parts[..2]
                .iter()
                .all(|p| p.len() >= 8 && p.chars().all(is_key_char))
    };
    aws || jwt
}

/// Byte ranges of the PEM private key blocks in `text`; an unterminated one runs to the end.
fn pem_blocks(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("-----BEGIN ") {
        let start = from + i;
        let line_end = text[start..].find('\n').map_or(text.len(), |n| start + n);
        if !text[start..line_end].contains("PRIVATE KEY-----") {
            from = start + 1;
            continue;
        }
        let end = text[line_end..]
            .find("-----END ")
            .and_then(|e| {
                let e = line_end + e;
                text[e + 9..].find("-----").map(|t| e + 9 + t + 5)
            })
            .unwrap_or(text.len());
        out.push((start, end));
        from = end;
    }
    out
}

/// The words of `text` outside `skip`, as byte ranges.
fn words(text: &str, skip: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
        if is_separator(c) {
            if let Some(s) = start.take() {
                out.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    out.retain(|(s, _)| !skip.iter().any(|(a, b)| s >= a && s < b));
    out
}

/// What the word after the current one is, in [`redact_text`]'s scan.
enum Next {
    /// Checked on its own.
    Plain,
    /// After a bare secret key: a value only behind a `:` or `=`.
    Delim,
    /// A secret value (after `password:` or an auth scheme).
    Value,
}

/// `text` with every secret replaced by [`MARKER`].
///
/// Postcondition: `removed` is the number of [`MARKER`]s inserted, and text
/// with no secret comes back byte-identical with `removed == 0`.
/// Test: `redaction_strips_passwords_and_keys`, `redaction_keeps_plain_text`.
pub fn redact_text(text: &str) -> Redacted {
    let mut spans = pem_blocks(text);
    let mut next = Next::Plain;
    for (s, e) in words(text, &spans) {
        let word = &text[s..e];
        let delims_only = word.chars().all(|c| c == ':' || c == '=');
        let delim = word.find([':', '=']);
        let key = key_of(delim.map_or(word, |d| &word[..d]));
        // Byte offset of the value after a key's `:` / `=` run.
        let value_at = |d: usize| word.len() - word[d..].trim_start_matches([':', '=']).len();
        match std::mem::replace(&mut next, Next::Plain) {
            Next::Value if delims_only => next = Next::Value,
            Next::Value => {
                // `password =value`: the delimiter stays, the value goes.
                let lead = if delim == Some(0) { value_at(0) } else { 0 };
                spans.push((s + lead, e));
                // `Authorization: Bearer x`: the scheme is hidden and its credential too.
                if SCHEMES.contains(&key.as_str()) {
                    next = Next::Value;
                }
                continue;
            }
            // `password = x` / `password =x`: a delimiter must follow a bare key.
            Next::Delim if delims_only => {
                next = Next::Value;
                continue;
            }
            Next::Delim if delim == Some(0) => {
                spans.push((s + value_at(0), e));
                continue;
            }
            Next::Delim | Next::Plain => {}
        }
        if key == BARE_SCHEME && delim.is_none() {
            next = Next::Value;
        } else if let Some(d) = delim.filter(|_| secret_key(&key)) {
            let value = value_at(d);
            if s + value < e {
                spans.push((s + value, e));
            } else {
                next = Next::Value;
            }
        } else if secret_key(&key) {
            next = Next::Delim;
        } else if secret_shape(word) {
            spans.push((s, e));
        }
    }
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e) in &spans {
        out.push_str(&text[at..*s]);
        out.push_str(MARKER);
        at = *e;
    }
    out.push_str(&text[at..]);
    debug_assert!(!spans.is_empty() || out == text);
    Redacted {
        text: out,
        removed: spans.len(),
    }
}
