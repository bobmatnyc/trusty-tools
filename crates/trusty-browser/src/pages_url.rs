//! Python 3.12 `urllib.parse.urlsplit`, as far as the page ops read it (`pages.py`).
//!
//! Why: `page_navigate` is refused `InvalidUrl` unless the URL passes the URL
//! floor (`floor_host`), and every audit `origin` is `urlsplit(origin).hostname`.
//! A parse that accepts or refuses a URL Python treats otherwise would let a
//! navigation through, or record another host. A `ValueError` from a worker's
//! origin closes the agent session in Python, so it is an error here too.
//! What: [`urlsplit`] keeps Python's whitespace stripping, scheme rule,
//! netloc split, bracket and NFKC checks; [`Split`] carries `username`,
//! `password`, `hostname` and the `port` check. [`floor_host`] and [`host`]
//! are the two `pages.py` helpers.
//! Test: `floor_host_matches_python_cases` below; `page_ops_replay_byte_identically`
//! in `tests/parity_pages.rs` (`page-navigate-floor`, `page-navigate-userinfo`).

use std::net::Ipv6Addr;

use unicode_normalization::UnicodeNormalization;

use crate::pyjson::Json;

/// A `ValueError` raised by `urlsplit` or `SplitResult.port`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid URL: {0}")]
pub struct UrlError(&'static str);

/// The parts of a split URL the page ops use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// Lowercased scheme, or empty.
    pub scheme: String,
    /// Network location, as written.
    pub netloc: String,
}

fn scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// `urlsplit(url)` with Python 3.12's checks.
pub fn urlsplit(url: &str) -> Result<Split, UrlError> {
    let url: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = url.as_str();
    let mut scheme = String::new();
    if let Some(i) = rest.find(':')
        && i > 0
        && rest.as_bytes()[0].is_ascii_alphabetic()
        && rest[..i].chars().all(scheme_char)
    {
        scheme = rest[..i].to_ascii_lowercase();
        rest = &rest[i + 1..];
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_owned();
        let (open, close) = (netloc.contains('['), netloc.contains(']'));
        if open != close {
            return Err(UrlError("Invalid IPv6 URL"));
        }
        if open {
            check_bracketed_netloc(&netloc)?;
        }
    }
    check_netloc(&netloc)?;
    Ok(Split { scheme, netloc })
}

fn partition(s: &str, sep: char) -> (&str, bool, &str) {
    s.split_once(sep)
        .map_or((s, false, ""), |(a, b)| (a, true, b))
}

fn hostinfo(netloc: &str) -> &str {
    netloc.rsplit_once('@').map_or(netloc, |(_, h)| h)
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let invalid = UrlError("Invalid IPv6 URL");
    let (before, open, bracketed) = partition(hostinfo(netloc), '[');
    let hostname = if open {
        if !before.is_empty() {
            return Err(invalid);
        }
        let (hostname, _, port) = partition(bracketed, ']');
        if !port.is_empty() && !port.starts_with(':') {
            return Err(invalid);
        }
        hostname
    } else {
        partition(before, ':').0
    };
    check_bracketed_host(hostname)
}

/// `_check_bracketed_host`: an IPvFuture literal or an IPv6 address (not IPv4).
fn check_bracketed_host(hostname: &str) -> Result<(), UrlError> {
    if let Some(rest) = hostname.strip_prefix('v') {
        let (hex, dot, tail) = partition(rest, '.');
        let ok = dot
            && !hex.is_empty()
            && hex.chars().all(|c| c.is_ascii_hexdigit())
            && !tail.is_empty();
        return ok
            .then_some(())
            .ok_or(UrlError("IPvFuture address is invalid"));
    }
    let (addr, scoped, scope) = partition(hostname, '%');
    let scope_ok = !scoped || (!scope.is_empty() && !scope.contains('%'));
    if scope_ok && addr.parse::<Ipv6Addr>().is_ok() {
        Ok(())
    } else {
        Err(UrlError("not an IPv6 address"))
    }
}

/// `_checknetloc`: no NFKC form of a non-ASCII netloc may add a delimiter.
fn check_netloc(netloc: &str) -> Result<(), UrlError> {
    if netloc.is_ascii() {
        return Ok(());
    }
    let n: String = netloc
        .chars()
        .filter(|c| !matches!(c, '@' | ':' | '#' | '?'))
        .collect();
    let normal: String = n.nfkc().collect();
    if normal != n && normal.contains(['/', '?', '#', '@', ':']) {
        return Err(UrlError("netloc contains invalid characters under NFKC"));
    }
    Ok(())
}

impl Split {
    /// `username` and `password`; `None` when the netloc has no `@`.
    pub fn userinfo(&self) -> (Option<&str>, Option<&str>) {
        match self.netloc.rsplit_once('@') {
            Some((info, _)) => {
                let (user, has, pass) = partition(info, ':');
                (Some(user), has.then_some(pass))
            }
            None => (None, None),
        }
    }

    fn host_and_port(&self) -> (&str, &str) {
        let (_, open, bracketed) = partition(hostinfo(&self.netloc), '[');
        if open {
            let (host, _, after) = partition(bracketed, ']');
            (host, partition(after, ':').2)
        } else {
            let (host, _, port) = partition(hostinfo(&self.netloc), ':');
            (host, port)
        }
    }

    /// `hostname`: lowercased, a `%zone` kept as written; `None` when empty.
    pub fn hostname(&self) -> Option<String> {
        let (host, _) = self.host_and_port();
        if host.is_empty() {
            return None;
        }
        let (name, pct, zone) = partition(host, '%');
        let mut out = name.to_lowercase();
        if pct {
            out.push('%');
            out.push_str(zone);
        }
        Some(out)
    }

    /// `port`: ASCII digits in 0-65535, else `ValueError`.
    pub fn port(&self) -> Result<Option<u16>, UrlError> {
        let (_, port) = self.host_and_port();
        if port.is_empty() {
            return Ok(None);
        }
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(UrlError("Port could not be cast to integer value"));
        }
        let digits = port.trim_start_matches('0');
        match digits.parse::<u32>() {
            _ if digits.is_empty() => Ok(Some(0)),
            Ok(p) if p <= 65535 => Ok(u16::try_from(p).ok()),
            _ => Err(UrlError("Port out of range 0-65535")),
        }
    }
}

/// `floor_host`: the host of an `https` URL, or of `http://127.0.0.1`, else `None`.
pub fn floor_host(url: Option<&Json>) -> Option<String> {
    let url = url?.as_str()?;
    let len = url.chars().count();
    if len == 0 || len > crate::refusal::MAX_STRING {
        return None;
    }
    let parts = urlsplit(url).ok()?;
    parts.port().ok()?;
    let host = parts.hostname()?;
    if parts.userinfo() != (None, None) {
        return None;
    }
    let ok = parts.scheme == "https" || (parts.scheme == "http" && host == "127.0.0.1");
    ok.then_some(host)
}

/// `host(origin)`: `urlsplit(origin).hostname` for a string, else `None`.
pub fn host(origin: Option<&str>) -> Result<Option<String>, UrlError> {
    match origin {
        Some(o) => Ok(urlsplit(o)?.hostname()),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_host_matches_python_cases() {
        // Expected values from Python 3.12.13 `pages.floor_host`.
        let cases: &[(&str, Option<&str>)] = &[
            ("https://Example.TEST/p", Some("example.test")),
            ("  https://a.test", Some("a.test")),
            ("ht\ttps://a.test", Some("a.test")),
            ("http://127.0.0.1:8080/", Some("127.0.0.1")),
            ("http://localhost/", None),
            ("https://u@a.test/", None),
            ("https://@a.test/", None),
            ("https://a.test:99999/", None),
            ("https://a.test:+1/", None),
            ("https://a.test:/", Some("a.test")),
            ("https://[::1]:443/", Some("::1")),
            ("https://[1.2.3.4]/", None),
            ("https://[v1.x]/", Some("v1.x")),
            ("https://x[::1]/", None),
            ("https://a.test\u{2100}b/", None),
            ("https:///p", None),
            ("ftp://a.test/", None),
            ("1https://a.test/", None),
        ];
        for (url, want) in cases {
            let got = floor_host(Some(&Json::from(*url)));
            assert_eq!(got.as_deref(), *want, "{url:?}");
        }
        assert_eq!(floor_host(Some(&Json::Int(1))), None);
        assert_eq!(host(Some("https://[x")).ok(), None, "ValueError");
        assert_eq!(host(None), Ok(None));
    }
}
