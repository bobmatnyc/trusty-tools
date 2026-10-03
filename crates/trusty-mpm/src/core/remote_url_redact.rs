//! Strip embedded credentials from a remote URL before it is logged (#9124).
//!
//! Why: an operator's `remote.origin.url` can be
//! `https://<user>:<token>@github.com/o/r.git`. The managed-clone path logged
//! that value verbatim at INFO, and two daemon log files held a live GitHub
//! token. Every site that logs or formats a remote URL — or text that may
//! quote one, such as `git clone` stderr — goes through this module.
//! What: [`redact_url`] replaces the userinfo of every `scheme://userinfo@`
//! authority in its input with [`REDACTED`]; text with no such authority is
//! returned borrowed and unchanged. An scp-style `git@host:o/r` carries no
//! secret and is left alone.
//! Test: `redact_url_strips_user_and_token` and its siblings in
//! `remote_url_redact_tests.rs`;
//! `a_credentialed_origin_never_reaches_the_log_or_the_error` for the clone
//! log line.

use std::borrow::Cow;

/// What a URL's userinfo is replaced with.
pub const REDACTED: &str = "***";

/// Characters that end a URL authority when it sits inside free text.
fn ends_authority(c: char) -> bool {
    matches!(c, '/' | '?' | '#' | '\'' | '"' | '<' | '>' | '`') || c.is_whitespace()
}

/// `text` with the userinfo of every `scheme://userinfo@host` replaced by
/// [`REDACTED`].
///
/// Why: see the module docs.
/// What: for each `://`, the authority runs to the first `ends_authority`
/// char; when it holds an `@`, everything before the LAST `@` (the userinfo,
/// as a URL parser splits it) is replaced. Borrowed when nothing changed.
/// Test: `redact_url_strips_user_and_token`,
/// `redact_url_strips_an_x_access_token`, `redact_url_leaves_a_clean_url_alone`,
/// `redact_url_redacts_every_url_in_free_text`.
pub fn redact_url(text: &str) -> Cow<'_, str> {
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
        match authority.rfind('@') {
            Some(cut) => {
                out.push_str(REDACTED);
                out.push_str(&authority[cut..]);
                changed = true;
            }
            None => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
#[path = "remote_url_redact_tests.rs"]
mod tests;
