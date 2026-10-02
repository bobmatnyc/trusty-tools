//! Exact Host/Origin match for the console's tailnet listener (#9035).
//!
//! Why: the console router answers every origin with permissive CORS, and the
//! write guard lets safe methods through. So a page on any site, opened in the
//! owner's browser on another of the owner's tailnet devices, passes the peer
//! gate and can read `GET` responses. DNS rebinding reaches the same place.
//! What: [`check_target`] accepts a request only when its `Host` names this
//! listener exactly (the tailnet `ip:port` or the node's MagicDNS name and
//! port) and any `Origin` it carries, on every method, is that same `http://`
//! authority. A missing `Host`, or a repeated or unreadable `Host` or
//! `Origin`, is refused.
//! Test: `check_target_refuses_every_unknown_shape`,
//! `check_target_accepts_the_listener_authorities`.

use std::net::SocketAddr;

use axum::http::{HeaderMap, HeaderName, Uri, header};

/// The `host:port` authorities the tailnet listener answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfAuthorities(Vec<String>);

impl SelfAuthorities {
    /// Authorities for a listener bound on `listen`, plus `magic_dns` (the
    /// node's MagicDNS name, no trailing dot) on the same port when known.
    pub fn new(listen: SocketAddr, magic_dns: Option<&str>) -> Self {
        let mut authorities = vec![listen.to_string()];
        if let Some(name) = magic_dns {
            authorities.push(format!("{name}:{}", listen.port()));
        }
        Self(authorities)
    }

    fn contains(&self, authority: &str) -> bool {
        self.0.iter().any(|a| a.eq_ignore_ascii_case(authority))
    }
}

/// Decide whether a request's target and origin are this listener's own.
///
/// Why: one pure function, so every refusal shape is unit-testable without a
/// socket.
/// What: takes the single `Host` header (HTTP/2 `:authority` from `uri` when
/// no `Host` is sent) and requires it to be one of `allowed`; a request-target
/// authority that differs from `Host` is refused. Any `Origin` must equal
/// `http://<Host>`. Returns the reason on refusal, for the WARN log.
/// Test: `check_target_refuses_every_unknown_shape`,
/// `check_target_accepts_the_listener_authorities`.
pub fn check_target(
    headers: &HeaderMap,
    uri: &Uri,
    allowed: &SelfAuthorities,
) -> Result<(), &'static str> {
    let uri_authority = uri.authority().map(|a| a.as_str());
    let host = match single_header(headers, &header::HOST)? {
        Some(host) => {
            if uri_authority.is_some_and(|a| !a.eq_ignore_ascii_case(host)) {
                return Err("request target and Host disagree");
            }
            host
        }
        None => uri_authority.ok_or("no Host")?,
    };
    if !allowed.contains(host) {
        return Err("Host is not this listener");
    }
    if let Some(origin) = single_header(headers, &header::ORIGIN)?
        && !origin.eq_ignore_ascii_case(&format!("http://{host}"))
    {
        return Err("Origin is not this listener");
    }
    Ok(())
}

/// The one value of header `name`, or `None` when absent. A repeated or
/// non-UTF-8 value is an error.
fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
) -> Result<Option<&'a str>, &'static str> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err("repeated Host or Origin header");
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| "unreadable Host or Origin header")
}
