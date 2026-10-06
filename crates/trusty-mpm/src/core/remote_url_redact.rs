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
//! is left alone. [`redact_stored_url`] is the variant for one stored URL,
//! whose authority a quote or whitespace does not end (#9227); every site that
//! prints one known URL uses it, and free text keeps [`redact_url`] (#9259).
//! Test: `redact_url_strips_user_and_token` and its siblings in
//! `remote_url_redact_tests.rs`;
//! `a_credentialed_origin_never_reaches_the_log_or_the_error` for the clone
//! log line.

use std::borrow::Cow;

use trusty_common::url_userinfo::{ends_authority, ends_url, userinfo_end};

/// What a URL's userinfo is replaced with.
pub const REDACTED: &str = "***";

/// Query keys whose value is a credential (GitHub, GitLab, OAuth), matched
/// case-insensitively after a `?` or `&`.
pub const TOKEN_KEYS: &[&str] = &["access_token", "private_token", "oauth_token", "token"];

/// A `repo_url` that keeps its raw value but prints redacted under `Debug`
/// (#9124): a derived `Debug` on an error variant would otherwise show it.
/// Test: `daemon::error::tests::project_not_found_message_redacts_the_url_credentials`,
/// `every_single_url_type_masks_a_quoted_or_spaced_password`.
#[derive(Clone, PartialEq, Eq)]
pub struct RedactedUrl(pub String);

impl std::ops::Deref for RedactedUrl {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<String> for RedactedUrl {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RedactedUrl {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl std::fmt::Debug for RedactedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // #9259: one stored URL, whose password may hold a quote or space.
        std::fmt::Debug::fmt(&redact_stored_url(&self.0), f)
    }
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
/// `redact_url_masks_a_password_holding_a_raw_query_or_fragment_char`,
/// `redact_url_masks_query_string_tokens`.
pub fn redact_url(text: &str) -> Cow<'_, str> {
    let userinfo = redact_userinfo(text);
    let query = match redact_query_tokens(&userinfo) {
        Cow::Owned(s) => Some(s),
        Cow::Borrowed(_) => None,
    };
    query.map_or(userinfo, Cow::Owned)
}

/// [`redact_url`] for one stored URL, such as a registry entry, rather than
/// free text (#9227).
///
/// Why: [`redact_url`] ends an authority at a quote or whitespace, so a
/// pre-#9124 entry `https://u:pa'ss@host/o/r` came back unchanged and
/// `tm list` printed the password.
/// What: masks the userinfo [`authority_userinfo_end`] finds after a git
/// scheme's `://`, or the `user:password` of an scp-style `user:pw@host:path`
/// (a bare `git@` is kept), then runs [`redact_url`] over the result for the
/// query tokens and any nested URL. A clean URL comes back borrowed.
/// Test: `redact_stored_url_masks_a_quoted_or_spaced_password`,
/// `redact_stored_url_keeps_a_clean_url_as_redact_url_shows_it`.
pub fn redact_stored_url(url: &str) -> Cow<'_, str> {
    match mask_stored_userinfo(url) {
        Some(masked) => Cow::Owned(redact_url(&masked).into_owned()),
        None => redact_url(url),
    }
}

/// `url` with its stored userinfo replaced by [`REDACTED`]; `None` when the
/// stored-URL boundaries find none.
fn mask_stored_userinfo(url: &str) -> Option<String> {
    if let Some(at) = url.find("://").filter(|&at| is_git_scheme(&url[..at])) {
        let tail = &url[at + 3..];
        let cut = authority_userinfo_end(tail)?;
        return Some(format!("{}{REDACTED}{}", &url[..at + 3], &tail[cut..]));
    }
    // git reads `u:T@h:o/r://x` as scp-style; a bare login such as `git@` is
    // no secret and stays.
    let head = &url[..url.find('/').unwrap_or(url.len())];
    let at = head.rfind('@').filter(|&at| head[at..].contains(':'))?;
    url[..at]
        .contains(':')
        .then(|| format!("{REDACTED}{}", &url[at..]))
}

/// [`userinfo_end`] for a stored URL, which is not free text (#9227).
///
/// What: the authority runs to the first `/`, `?` or `#` only, so a quote or
/// whitespace cannot end it; its last `@` ends the userinfo. With no `@` but a
/// `:`, the search runs to the end of the URL, as `userinfo_end`'s over-read
/// does. The result is never before [`userinfo_end`]'s cut: the larger of the
/// two is returned, so an early end fails closed at runtime.
/// Test: `clone_url_strips_or_refuses`,
/// `redact_stored_url_masks_a_quoted_or_spaced_password`.
pub(crate) fn authority_userinfo_end(tail: &str) -> Option<usize> {
    let authority = &tail[..tail.find(['/', '?', '#']).unwrap_or(tail.len())];
    let cut = match authority.rfind('@') {
        Some(at) => Some(at),
        None => authority
            .find(':')
            .and_then(|colon| tail[colon..].rfind('@').map(|i| colon + i)),
    };
    // `None < Some(_)`, so a free-text cut can only push this one later.
    cut.max(userinfo_end(tail))
}

/// Whether git reads `scheme` as a URL scheme: an alphanumeric, then
/// alphanumerics, `+`, `-` or `.` (git's `is_urlschemechar`).
pub(crate) fn is_git_scheme(scheme: &str) -> bool {
    scheme.starts_with(|c: char| c.is_ascii_alphanumeric())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The userinfo pass of [`redact_url`].
///
/// What: for each `://`, the userinfo is everything before the `@` that
/// [`userinfo_end`] finds, and is replaced. That boundary is trusty-common's,
/// shared with the stripper that runs before identity derivation (#9124), so
/// the two cannot disagree on where a credential ends; see its docs for the
/// raw-`/` password rule.
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
        match userinfo_end(tail) {
            Some(cut) => {
                out.push_str(REDACTED);
                rest = &tail[cut..];
                changed = true;
            }
            None => {
                let end = tail.find(ends_authority).unwrap_or(tail.len());
                out.push_str(&tail[..end]);
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
