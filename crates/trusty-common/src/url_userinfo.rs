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
//! `user@host:path` forms.
//! Test: `strip_userinfo_table`, `userinfo_end_stops_at_free_text`.

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
/// Why: see the module docs.
/// What: the authority runs to the first `/`, `?`, `#`, quote or whitespace.
/// When it holds an `@`, the LAST one ends the userinfo, as a URL parser
/// splits it. When it holds none but has a `:`, the userinfo may be
/// `user:pa/ss` — git accepts a raw `/`, `?` or `#` in a password, which ends
/// the authority early — so the search runs to the last `@` before the URL
/// ends in the surrounding text. That over-reads `host:port/path@x`, which is
/// the safe direction for a credential.
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
    let head = &url[..url.find('/').unwrap_or(url.len())];
    match head.rfind('@') {
        Some(at) if head[at..].contains(':') => Cow::Borrowed(&url[at + 1..]),
        _ => Cow::Borrowed(url),
    }
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

    /// In free text the authority stops at whitespace or a quote, so an `@`
    /// in later prose is not read as the userinfo's end.
    #[test]
    fn userinfo_end_stops_at_free_text() {
        assert_eq!(userinfo_end("host/o/r and mail@x"), None);
        assert_eq!(userinfo_end("u:T@host/o/r"), Some(3));
        assert_eq!(userinfo_end("host:8080/o/r 'a@b'"), None);
    }
}
