//! Strip embedded credentials from a remote URL before it is logged (#9124).
//!
//! Why: an operator's `remote.origin.url` can be
//! `https://<user>:<token>@github.com/o/r.git`. The managed-clone path logged
//! that value verbatim at INFO, and two daemon log files held a live GitHub
//! token. Every site that logs or formats a remote URL — or text that may
//! quote one, such as `git clone` stderr — goes through this module.
//! What: [`redact_url`] replaces the userinfo of every `scheme://userinfo@`
//! authority in its input with [`REDACTED`], and the value of every
//! token-bearing query key ([`TOKEN_KEYS`]); text with neither is returned
//! borrowed and unchanged. An scp-style `git@host:o/r` carries no secret and
//! is left alone.
//! Test: `redact_url_strips_user_and_token` and its siblings in
//! `remote_url_redact_tests.rs`;
//! `a_credentialed_origin_never_reaches_the_log_or_the_error` for the clone
//! log line.

use std::borrow::Cow;

/// What a URL's userinfo is replaced with.
pub const REDACTED: &str = "***";

/// Query keys whose value is a credential (GitHub, GitLab, OAuth), matched
/// case-insensitively after a `?` or `&`.
pub const TOKEN_KEYS: &[&str] = &["access_token", "private_token", "oauth_token", "token"];

/// Characters that end a URL when it sits inside free text.
fn ends_url(c: char) -> bool {
    matches!(c, '\'' | '"' | '<' | '>' | '`') || c.is_whitespace()
}

/// Characters that end a URL authority when it sits inside free text.
fn ends_authority(c: char) -> bool {
    matches!(c, '/' | '?' | '#') || ends_url(c)
}

/// `text` with every URL credential replaced by [`REDACTED`]: the userinfo of
/// each `scheme://userinfo@host`, and the value of each [`TOKEN_KEYS`] query
/// parameter.
///
/// Why: see the module docs.
/// What: the userinfo pass, then the query pass; borrowed when neither
/// changed anything.
/// Test: `redact_url_strips_user_and_token`,
/// `redact_url_strips_an_x_access_token`, `redact_url_leaves_a_clean_url_alone`,
/// `redact_url_redacts_every_url_in_free_text`,
/// `redact_url_masks_a_password_holding_a_raw_slash`,
/// `redact_url_masks_query_string_tokens`.
pub fn redact_url(text: &str) -> Cow<'_, str> {
    let userinfo = redact_userinfo(text);
    let query = match redact_query_tokens(&userinfo) {
        Cow::Owned(s) => Some(s),
        Cow::Borrowed(_) => None,
    };
    query.map_or(userinfo, Cow::Owned)
}

/// The userinfo pass of [`redact_url`].
///
/// What: for each `://`, the authority runs to the first [`ends_authority`]
/// char; when it holds an `@`, everything before the LAST `@` (the userinfo,
/// as a URL parser splits it) is replaced. When it holds none but has a `:`,
/// the userinfo may be `user:pa/ss` — git accepts a raw `/` in a password,
/// which ends the authority early — so it runs to the first `@` in the rest of
/// the URL. That over-redacts `host:port/path@x`, which is the safe direction.
fn redact_userinfo(text: &str) -> Cow<'_, str> {
    if !text.contains("://") || !text.contains('@') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail.find(ends_authority).unwrap_or(tail.len());
        let authority = &tail[..end];
        let cut = authority
            .rfind('@')
            .or_else(|| slashed_userinfo_end(tail, authority));
        match cut {
            Some(cut) => {
                out.push_str(REDACTED);
                rest = &tail[cut..];
                changed = true;
            }
            None => {
                out.push_str(authority);
                rest = &tail[end..];
            }
        }
    }
    out.push_str(rest);
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

/// Where a `user:pa/ss@host` userinfo ends in `tail`, when `authority` (the
/// prefix of `tail` up to its first `/`) holds a `:` and no `@`.
fn slashed_userinfo_end(tail: &str, authority: &str) -> Option<usize> {
    let colon = authority.find(':')?;
    let url_end = tail
        .find(|c: char| ends_url(c) || matches!(c, '?' | '#'))
        .unwrap_or(tail.len());
    tail.get(colon..url_end)?.find('@').map(|i| colon + i)
}

/// The query pass of [`redact_url`]: the value after each `?key=` / `&key=`
/// for a key in [`TOKEN_KEYS`], up to the next `&`, `#` or [`ends_url`] char.
fn redact_query_tokens(text: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut copied = 0;
    for (sep, _) in text.match_indices(['?', '&']) {
        if sep < copied {
            continue;
        }
        let after = &text[sep + 1..];
        let Some(key) = TOKEN_KEYS.iter().find(|k| {
            after
                .get(..k.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(k))
                && after[k.len()..].starts_with('=')
        }) else {
            continue;
        };
        let start = sep + 1 + key.len() + 1;
        let len = text[start..]
            .find(|c: char| ends_url(c) || matches!(c, '&' | '#'))
            .unwrap_or(text.len() - start);
        if len == 0 {
            continue;
        }
        out.push_str(&text[copied..start]);
        out.push_str(REDACTED);
        copied = start + len;
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

#[cfg(test)]
#[path = "remote_url_redact_tests.rs"]
mod tests;
