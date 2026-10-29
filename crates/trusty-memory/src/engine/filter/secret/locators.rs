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

/// Parameter-name suffixes whose value is a credential whatever its length.
///
/// Why (#9748): a password such as `hunter2` has no shape to read, so the name
/// alone must refuse it.
/// What: matched against the normalised name (lowercase, `-` as `_`) as a raw
/// suffix, so `db_passwd`, `client_secret` and `x_pwd` all match.
/// Test: `credential_parameter_names_are_refused_by_suffix_after_9748`.
pub(crate) const PASSWORD_NAME_SUFFIXES: &[&str] = &["password", "passwd", "pwd", "secret"];

/// Parameter-name suffixes whose value is a credential from
/// [`MIN_QUERY_SECRET_LEN`] bytes.
///
/// Why (#9748): a presigned-URL signature is lowercase hex and an OAuth token
/// can be all-lowercase, so the value's shape alone misses them.
/// What: matched as a raw suffix of the normalised name (`apitoken`,
/// `slack_token`, `x_amz_signature`, `x_amz_credential`). The floor keeps
/// `token=none` and similar short words out.
pub(crate) const TOKEN_NAME_SUFFIXES: &[&str] = &["token", "signature", "credential"];

/// Short names that count only when they are the whole name or follow a `_`:
/// `sig`, `key`, `auth`.
///
/// Why (#9748): a raw suffix match would refuse `monkey`, `turkey` and
/// `keyword`-like names; these three are ordinary words and word endings.
/// What: `sort_key` and `x_api_key` match, `monkey` does not. See
/// [`COMPOUND_KEY_NAMES`] for the run-together spellings.
pub(crate) const BOUNDARY_NAME_SUFFIXES: &[&str] = &["sig", "key", "auth"];

/// Run-together spellings that end a [`BOUNDARY_NAME_SUFFIXES`] word but name
/// a credential: `apikey`, `accesskey`, `secretkey`, `privatekey`, `oauth`.
pub(crate) const COMPOUND_KEY_NAMES: &[&str] =
    &["apikey", "accesskey", "secretkey", "privatekey", "oauth"];

/// Shortest value under a [`TOKEN_NAME_SUFFIXES`] or `sig` name that is
/// refused.
pub(crate) const MIN_QUERY_SECRET_LEN: usize = 8;

/// Shortest value under a `key` or `auth` name that is refused.
///
/// Why (#9748): `key` and `auth` name ordinary values too (`sort_key=asc`,
/// `auth=basic`). The guard is a floor of this many bytes and a value made of
/// ASCII letters, digits and `%` only, so a wordy value (`created_at_desc`,
/// `no-cache`) is admitted and a bare 24-letter key is not.
pub(crate) const MIN_WEAK_NAME_SECRET_LEN: usize = 16;

/// Most `=` a token may hold before [`super::is_structural_token`] stops
/// recursing on the text after each one.
///
/// Why (#9748): the recursion overflowed the stack on a 100 KB query string.
pub(crate) const MAX_STRUCTURAL_EQUALS: usize = 64;

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
/// What: requires `scheme://`. Refuses a userinfo with a non-empty password
/// ([`userinfo_password`]). Splits the text after the first `?` or `#` on `&`,
/// `;`, `?` and `#`; a parameter is a credential by [`param_carries_secret`],
/// or when its value alone [`looks_like_secret`]. A value holds none of the
/// split characters, so the recursion is one level deep.
/// `?code=` is not a credential name: it is an OAuth authorization code but
/// also ordinary text, so only a code whose value is credential-shaped is
/// refused.
/// Test: `secrets_inside_paths_and_urls_are_refused_after_9748`,
/// `ordinary_paths_and_urls_are_accepted_after_9748`,
/// `benign_query_names_and_at_signs_stay_accepted_after_9748`.
pub(crate) fn url_carries_secret(token: &str) -> bool {
    let Some((scheme, rest)) = token.split_once("://") else {
        return false;
    };
    if scheme.is_empty() {
        return false;
    }
    if userinfo_password(rest).is_some_and(|pw| !pw.is_empty()) {
        return true;
    }
    let Some(at) = rest.find(['?', '#']) else {
        return false;
    };
    rest[at + 1..]
        .split(['&', ';', '?', '#'])
        .any(param_carries_secret)
}

/// The password of the userinfo in `rest` (the text after `scheme://`).
///
/// Why (#9748): a password may hold a raw `#` or `?`, so the authority cannot
/// end at them: `mysql://root:hunter#2@db.internal/app`.
/// What: the userinfo is the text before the last `@` that comes before the
/// first `/`; the password is the text after its first `:`. Not userinfo when
/// the user part holds `?` or `#` (`https://host?u=a:b@c` is a query), or when
/// the text is `host:port` followed by a query (`localhost:3000?e=a@b`).
/// Test: `userinfo_with_raw_delimiters_is_refused_after_9748`,
/// `benign_query_names_and_at_signs_stay_accepted_after_9748`.
fn userinfo_password(rest: &str) -> Option<&str> {
    let zone = &rest[..rest.find('/').unwrap_or(rest.len())];
    let (userinfo, _host) = zone.rsplit_once('@')?;
    let (user, password) = userinfo.split_once(':')?;
    if user.contains(['?', '#']) {
        return None;
    }
    if let Some(q) = password.find(['?', '#']) {
        let (port, query) = password.split_at(q);
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && query.contains('=') {
            return None;
        }
    }
    Some(password)
}

/// True when one `name=value` query or fragment parameter carries a
/// credential; see [`url_carries_secret`].
///
/// Why (#9748): the name decides when the value has no shape of its own.
/// What: normalises the name (lowercase, `-` as `_`). A
/// [`PASSWORD_NAME_SUFFIXES`] name refuses any non-empty value; a
/// [`TOKEN_NAME_SUFFIXES`] or `sig` name from [`MIN_QUERY_SECRET_LEN`] bytes; a
/// `key` or `auth` name from [`MIN_WEAK_NAME_SECRET_LEN`] bytes of letters,
/// digits and `%`. Any other name refuses only a value that
/// [`looks_like_secret`]. Percent-encoding counts as part of the value.
/// Test: `credential_parameter_names_are_refused_by_suffix_after_9748`,
/// `benign_query_names_and_at_signs_stay_accepted_after_9748`.
fn param_carries_secret(param: &str) -> bool {
    let (name, value) = param.split_once('=').unwrap_or(("", param));
    if value.is_empty() {
        return false;
    }
    let name = name.to_ascii_lowercase().replace('-', "_");
    let by_name = if PASSWORD_NAME_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        true
    } else if TOKEN_NAME_SUFFIXES.iter().any(|s| name.ends_with(s))
        || is_boundary_name(&name, "sig")
    {
        value.len() >= MIN_QUERY_SECRET_LEN
    } else if is_boundary_name(&name, "key")
        || is_boundary_name(&name, "auth")
        || COMPOUND_KEY_NAMES.iter().any(|s| name.ends_with(s))
    {
        value.len() >= MIN_WEAK_NAME_SECRET_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'%')
    } else {
        false
    };
    by_name || looks_like_secret(value)
}

/// True when `name` is `word` or ends in `_word`.
fn is_boundary_name(name: &str, word: &str) -> bool {
    debug_assert!(BOUNDARY_NAME_SUFFIXES.contains(&word));
    name.strip_suffix(word)
        .is_some_and(|head| head.is_empty() || head.ends_with('_'))
}
