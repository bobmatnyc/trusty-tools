//! Paths and URLs as the secret gate reads them (issue #9748).
//!
//! Why: a file reference with a `:line` suffix
//! (`architect/.claude/skills/instance-evo/SKILL.md:38`) was refused because
//! the `:` took the last segment out of every path-segment grammar. A URL with
//! a query string or fragment was admitted whatever it carried, because `?`,
//! `&` and `#` are outside both credential charsets.
//! What: [`strip_file_line_suffix`] removes the line suffix so the rest is
//! judged on its own; [`url_carries_secret`] refuses a URL whose userinfo,
//! query or fragment carries a credential. Neither admits a whole token for
//! looking like a path or a URL.
//! Test: `ordinary_paths_and_urls_are_accepted_after_9748`,
//! `secrets_inside_paths_and_urls_are_refused_after_9748`.

use super::{file_name_stem, looks_like_secret};

/// Query and fragment parameter names whose value is a credential by name.
///
/// Why (#9748): a presigned-URL signature is lowercase hex and an OAuth token
/// can be all-lowercase, so the value's shape alone misses them.
/// What: compared against the lowercased parameter name. `key` and `auth` are
/// left out because they name ordinary values too; a key-shaped value under
/// them is still caught by its shape.
pub(crate) const SECRET_QUERY_KEYS: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "auth_token",
    "private_token",
    "api_key",
    "apikey",
    "secret",
    "client_secret",
    "password",
    "passwd",
    "sig",
    "signature",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "x-goog-signature",
    "x-goog-credential",
];

/// Shortest value under a [`SECRET_QUERY_KEYS`] name that is refused.
pub(crate) const MIN_QUERY_SECRET_LEN: usize = 8;

/// Most `:N` groups a file reference carries: `:line` or `:line:column`.
pub(crate) const MAX_LINE_SUFFIX_GROUPS: usize = 2;

/// Most digits in one `:N` group of a file reference.
pub(crate) const MAX_LINE_SUFFIX_DIGITS: usize = 7;

/// `token` without its `:line` or `:line:column` suffix, when what remains
/// ends in a `<stem>.<ext>` file name.
///
/// Why (#9748): the suffix is position metadata. Removing it lets the path be
/// judged by the rules any path gets. The suffix is digits only, so it can
/// carry no part of a credential.
/// What: strips up to [`MAX_LINE_SUFFIX_GROUPS`] trailing `:` + 1 to
/// [`MAX_LINE_SUFFIX_DIGITS`] digits. Returns `None` when there is no suffix,
/// or when the last `/`-segment of the rest fails [`file_name_stem`]'s
/// grammar (lowercase extension, no provider key).
/// Test: `ordinary_paths_and_urls_are_accepted_after_9748`,
/// `secrets_inside_paths_and_urls_are_refused_after_9748`.
pub(crate) fn strip_file_line_suffix(token: &str) -> Option<&str> {
    let mut base = token;
    for _ in 0..MAX_LINE_SUFFIX_GROUPS {
        match base.rsplit_once(':') {
            Some((head, n))
                if (1..=MAX_LINE_SUFFIX_DIGITS).contains(&n.len())
                    && n.bytes().all(|b| b.is_ascii_digit()) =>
            {
                base = head;
            }
            _ => break,
        }
    }
    if base.len() == token.len() {
        return None;
    }
    let name = base.rsplit('/').next()?;
    file_name_stem(name).map(|_| base)
}

/// True when `token` is a URL that carries a credential in its userinfo, its
/// query or its fragment.
///
/// Why (#9748): these three parts hold credentials by design (basic auth,
/// `?access_token=`, presigned signatures, OAuth implicit-flow fragments), and
/// the `?`, `&` and `#` around them kept the token out of both shape branches
/// of [`looks_like_secret`].
/// What: requires `scheme://`. Refuses a userinfo with a non-empty password.
/// Splits the text after the first `?` or `#` on `&`, `;`, `?` and `#`; a
/// parameter is a credential when its name is in [`SECRET_QUERY_KEYS`] and its
/// value has at least [`MIN_QUERY_SECRET_LEN`] bytes, or when its value alone
/// [`looks_like_secret`]. The recursion is on a strictly shorter string.
/// Test: `secrets_inside_paths_and_urls_are_refused_after_9748`,
/// `ordinary_paths_and_urls_are_accepted_after_9748`.
pub(crate) fn url_carries_secret(token: &str) -> bool {
    let Some((scheme, rest)) = token.split_once("://") else {
        return false;
    };
    if scheme.is_empty() {
        return false;
    }
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let password = authority
        .rsplit_once('@')
        .and_then(|(userinfo, _)| userinfo.split_once(':'))
        .map(|(_, pw)| pw);
    if password.is_some_and(|pw| !pw.is_empty()) {
        return true;
    }
    let Some(at) = rest.find(['?', '#']) else {
        return false;
    };
    rest[at + 1..]
        .split(['&', ';', '?', '#'])
        .any(param_carries_secret)
}

/// True when one `name=value` query or fragment parameter carries a
/// credential; see [`url_carries_secret`].
fn param_carries_secret(param: &str) -> bool {
    let (name, value) = param.split_once('=').unwrap_or(("", param));
    if value.is_empty() {
        return false;
    }
    let named = SECRET_QUERY_KEYS.contains(&name.to_ascii_lowercase().as_str());
    (named && value.len() >= MIN_QUERY_SECRET_LEN) || looks_like_secret(value)
}
