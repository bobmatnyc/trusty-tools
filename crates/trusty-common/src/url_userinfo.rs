//! Where a URL's userinfo ends, and the URL without it (#9124).
//!
//! Why: `https://user:<token>@host/x.git` is a legal git remote. Every parser
//! that derives an identity from a remote — the `owner/repo` path segments, the
//! palace id, a CLI alias — read the userinfo as part of the host or the path,
//! so for an owner-less URL the token became the owner: it was lowercased into
//! a managed-checkout directory, logged, and echoed in errors. Userinfo is
//! never part of a repository's identity, so it leaves before any derivation.
//! trusty-mpm's log redactor (`core::remote_url_redact`) finds the same span
//! through [`userinfo_end`], so the stripper and the redactor cannot disagree
//! on where a credential ends.
//! What: [`userinfo_end`] locates the `@` that ends the userinfo of the
//! authority following a `scheme://`; [`strip_userinfo`] returns one URL with
//! its userinfo removed, for both the `scheme://` and the scp-style
//! `user@host:path` forms. [`strip_url_secret`] removes only the secret, for a
//! URL that is stored and cloned rather than compared (#9155).
//! Test: `strip_userinfo_table`, `strip_url_secret_table`,
//! `userinfo_end_stops_at_free_text`.
//!
//! [`userinfo_end`]: crate::url_userinfo::userinfo_end
//! [`strip_userinfo`]: crate::url_userinfo::strip_userinfo
//! [`strip_url_secret`]: crate::url_userinfo::strip_url_secret

use std::borrow::Cow;

/// Characters that end a URL when it sits inside free text.
pub fn ends_url(c: char) -> bool {
    matches!(c, '\'' | '"' | '<' | '>' | '`') || c.is_whitespace()
}

/// Characters that end a URL authority when it sits inside free text.
pub fn ends_authority(c: char) -> bool {
    matches!(c, '/' | '?' | '#') || ends_url(c)
}

/// The byte index of the `@` that ends the userinfo in `tail`, the text right
/// after a `scheme://`; `None` when the authority carries no userinfo.
///
/// Why: the log redactor (`core::remote_url_redact` in trusty-mpm) and
/// [`strip_userinfo`] must never let a password through, so this boundary
/// over-reads: masking or dropping too much is the safe direction for a log
/// line or a derived identity. [`strip_url_secret`] stores the URL it returns,
/// where an over-read rewrites the URL, so it uses `stored_userinfo_end`
/// instead (#9124).
/// What: the authority runs to the first `/`, `?`, `#`, quote or whitespace.
/// When it holds an `@`, the LAST one ends the userinfo, as a URL parser
/// splits it. When it holds none but has a `:`, the userinfo may be
/// `user:pa/ss` — git accepts a raw `/`, `?` or `#` in a password, which ends
/// the authority early — so the search runs to the last `@` before the URL
/// ends in the surrounding text. That over-reads `host:port/path@x`.
/// Test: `strip_userinfo_table`, `userinfo_end_stops_at_free_text`.
pub fn userinfo_end(tail: &str) -> Option<usize> {
    let end = tail.find(ends_authority).unwrap_or(tail.len());
    let authority = &tail[..end];
    if let Some(at) = authority.rfind('@') {
        return Some(at);
    }
    let colon = authority.find(':')?;
    let url_end = tail.find(ends_url).unwrap_or(tail.len());
    tail.get(colon..url_end)?.rfind('@').map(|i| colon + i)
}

/// [`userinfo_end`] without its over-read of a userinfo-free authority, for a
/// URL that is stored rather than logged (#9124).
///
/// What: `None` when the authority holds no `@` and parses as a host with an
/// optional numeric port — `host`, `host:8080`, `[::1]`, `[::1]:8080` — so an
/// `@` in the path or query is never read as the userinfo's end. Any other
/// authority, such as `user:pa` cut short by a raw `/` in the password, gets
/// [`userinfo_end`]'s answer.
/// Test: `strip_url_secret_table`.
fn stored_userinfo_end(tail: &str) -> Option<usize> {
    let end = tail.find(ends_authority).unwrap_or(tail.len());
    let authority = &tail[..end];
    if !authority.contains('@') && is_host_port(authority) {
        return None;
    }
    userinfo_end(tail)
}

/// Whether `authority` is a host with an optional numeric port, an IPv6
/// `[...]` literal included.
fn is_host_port(authority: &str) -> bool {
    let after_host = match authority.strip_prefix('[') {
        Some(v6) => match v6.find(']') {
            Some(close) => &v6[close + 1..],
            None => return false,
        },
        None => authority.find(':').map_or("", |colon| &authority[colon..]),
    };
    after_host.is_empty()
        || after_host
            .strip_prefix(':')
            .is_some_and(|port| port.bytes().all(|b| b.is_ascii_digit()))
}

/// `url` with its userinfo removed; borrowed and unchanged when it has none.
///
/// Why: see the module docs.
/// What: with a `scheme://`, drops everything from the authority's start
/// through the `@` [`userinfo_end`] finds. Without one, an scp-style
/// `user@host:path` drops through the last `@` that precedes both the host's
/// `:` and the first `/`; a path such as `dir@x/repo` has no such `:` and is
/// left alone.
/// Test: `strip_userinfo_table`.
pub fn strip_userinfo(url: &str) -> Cow<'_, str> {
    if let Some(at) = url.find("://") {
        let tail = &url[at + 3..];
        return match userinfo_end(tail) {
            Some(cut) => Cow::Owned(format!("{}{}", &url[..at + 3], &tail[cut + 1..])),
            None => Cow::Borrowed(url),
        };
    }
    match scp_userinfo_end(url) {
        Some(at) => Cow::Borrowed(&url[at + 1..]),
        None => Cow::Borrowed(url),
    }
}

/// The byte index of the `@` that ends the userinfo of an scp-style
/// `user@host:path`; `None` for a URL with a `scheme://`, or with no userinfo.
///
/// What: the last `@` before the first `/` that a `:` follows; a path such as
/// `dir@x/repo` has no such `:`.
/// Test: `strip_userinfo_table`, `strip_url_secret_table`.
pub fn scp_userinfo_end(url: &str) -> Option<usize> {
    if url.contains("://") {
        return None;
    }
    let head = &url[..url.find('/').unwrap_or(url.len())];
    head.rfind('@').filter(|&at| head[at..].contains(':'))
}

/// `url` with only its secret removed, for storage; borrowed and unchanged
/// when it carries none (#9124, #9155).
///
/// Why: [`strip_userinfo`] serves identity, which no userinfo is part of. A
/// stored clone URL still needs its login name: `git@github.com:o/r.git`
/// stored as `github.com:o/r.git` makes ssh log in as the local user, and the
/// clone fails.
/// What: removes the `:password` of any userinfo, keeping `user@`. On an
/// `http(s)://` URL (a `+`-prefixed scheme such as `git+https` included) it
/// removes the whole userinfo, because a bare `user@` there is a token. A bare
/// `user@` on any other scheme, or on an scp-style `user@host:path`, is kept.
/// The userinfo ends where `stored_userinfo_end` or [`scp_userinfo_end`]
/// says, so `https://host:8080/@scope/pkg` is stored unchanged.
/// Test: `strip_url_secret_table`.
pub fn strip_url_secret(url: &str) -> Cow<'_, str> {
    let (prefix, userinfo, rest, http) = if let Some(at) = url.find("://") {
        let tail = &url[at + 3..];
        // #9124: `userinfo_end` over-reads `host:port/path@x`; storage must not.
        let Some(cut) = stored_userinfo_end(tail) else {
            return Cow::Borrowed(url);
        };
        let scheme = url[..at].rsplit('+').next().unwrap_or_default();
        let http = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
        (&url[..at + 3], &tail[..cut], &tail[cut + 1..], http)
    } else {
        let Some(at) = scp_userinfo_end(url) else {
            return Cow::Borrowed(url);
        };
        ("", &url[..at], &url[at + 1..], false)
    };
    let user = match userinfo.split_once(':') {
        _ if http => "",
        Some((user, _)) => user,
        // A bare login name on a non-http scheme carries no secret.
        None => return Cow::Borrowed(url),
    };
    let at = if user.is_empty() { "" } else { "@" };
    Cow::Owned(format!("{prefix}{user}{at}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every credential shape loses its userinfo; a clean URL is untouched.
    #[test]
    fn strip_userinfo_table() {
        for (url, want) in [
            ("https://user:TOK@host/x.git", "https://host/x.git"),
            ("https://user:TOK@host/", "https://host/"),
            ("https://TOK@github.com/o/r", "https://github.com/o/r"),
            (
                "https://x-access-token:T@github.com/o/r.git",
                "https://github.com/o/r.git",
            ),
            ("https://u:pa/ss@host/o/r", "https://host/o/r"),
            ("https://u:pa?ss@host/o/r", "https://host/o/r"),
            ("https://u:p@ss@host/o/r", "https://host/o/r"),
            ("ssh://git@host:22/o/r", "ssh://host:22/o/r"),
            ("u:TOK@host:o/r", "host:o/r"),
            ("git@github.com:o/r.git", "github.com:o/r.git"),
            ("https://github.com/o/r", "https://github.com/o/r"),
            ("https://host:8080/o/r", "https://host:8080/o/r"),
            ("/srv/dir@x/repo", "/srv/dir@x/repo"),
            ("dir@x/repo", "dir@x/repo"),
            ("", ""),
        ] {
            assert_eq!(strip_userinfo(url), want, "{url:?}");
        }
    }

    /// #9155: storage keeps the login name and drops only the secret — the
    /// `:password` on any scheme, and the whole userinfo on http(s).
    #[test]
    fn strip_url_secret_table() {
        for (url, want) in [
            ("git@github.com:o/r.git", "git@github.com:o/r.git"),
            ("ssh://git@h:2222/t/r", "ssh://git@h:2222/t/r"),
            ("ssh://u:TOK@h:2222/t/r", "ssh://u@h:2222/t/r"),
            ("ssh://:TOK@h/t/r", "ssh://h/t/r"),
            ("git+ssh://git@h/t/r", "git+ssh://git@h/t/r"),
            ("https://TOK@github.com/o/r", "https://github.com/o/r"),
            ("https://u:TOK@github.com/o/r", "https://github.com/o/r"),
            ("HTTPS://u:TOK@github.com/o/r", "HTTPS://github.com/o/r"),
            (
                "git+https://TOK@github.com/o/r",
                "git+https://github.com/o/r",
            ),
            ("http://u:pa/ss@host/o/r", "http://host/o/r"),
            ("u:TOK@host:o/r", "u@host:o/r"),
            ("u:p@ss@host:o/r", "u@host:o/r"),
            ("https://github.com/o/r", "https://github.com/o/r"),
            ("/srv/dir@x/repo", "/srv/dir@x/repo"),
            ("https://u:T@[::1]:8080/x", "https://[::1]:8080/x"),
            // #9124: an `@` after a userinfo-free authority is path or query.
            (
                "https://host:8080/@scope/pkg",
                "https://host:8080/@scope/pkg",
            ),
            ("ssh://h:2222/o/r@x", "ssh://h:2222/o/r@x"),
            ("https://host:8443/p?who=a@b", "https://host:8443/p?who=a@b"),
            ("https://[::1]:8080/o/r@x", "https://[::1]:8080/o/r@x"),
            ("https://[::1]/o/r?who=a@b", "https://[::1]/o/r?who=a@b"),
            ("", ""),
        ] {
            assert_eq!(strip_url_secret(url), want, "{url:?}");
        }
    }

    /// In free text the authority stops at whitespace or a quote, so an `@`
    /// in later prose is not read as the userinfo's end.
    #[test]
    fn userinfo_end_stops_at_free_text() {
        assert_eq!(userinfo_end("host/o/r and mail@x"), None);
        assert_eq!(userinfo_end("u:T@host/o/r"), Some(3));
        assert_eq!(userinfo_end("host:8080/o/r 'a@b'"), None);
    }
}
