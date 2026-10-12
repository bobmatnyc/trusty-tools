//! Exact Host/Origin match for the console's listeners (#9035, #9792).
//!
//! Why: the write guard lets safe methods through, so without this check a
//! page on any site, opened in the owner's browser, could read `GET`
//! responses; DNS rebinding reaches the same place. Before #9792 the router
//! also answered every origin with permissive CORS.
//! What: [`check_target`] accepts a request only when its `Host` names the
//! listener exactly (the tailnet `ip:port` or the node's MagicDNS name and
//! port; for loopback, `ip:port` and `localhost:port`) and any `Origin` it
//! carries, on every method, is that same `http://` authority. A missing
//! `Host`, or a repeated or unreadable `Host` or `Origin`, is refused. The
//! loopback guard also uses [`check_target_with_scheme`] for the `https://`
//! MagicDNS origin `tailscale serve` fronts.
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

    /// Authorities for a loopback listener on `listen`: its `ip:port` and
    /// `localhost` on the same port.
    ///
    /// Why: #9067 P2 — a page opened at `http://localhost:PORT/` sends
    /// `Host: localhost:PORT`, which names this listener as surely as the
    /// IP does. No other name is added.
    /// What: [`SelfAuthorities::new`] with `localhost` as the name.
    /// Test: `localhost_on_the_bound_port_is_an_own_authority`,
    /// `every_other_host_name_is_still_refused`.
    pub fn loopback(listen: SocketAddr) -> Self {
        Self::new(listen, Some("localhost"))
    }

    /// Exactly the authority `authority`, with no port added (#9792: the
    /// MagicDNS name `tailscale serve` fronts on its default https port).
    pub fn exact(authority: &str) -> Self {
        Self(vec![authority.to_owned()])
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
    check_target_with_scheme(headers, uri, allowed, "http")
}

/// [`check_target`] with `scheme` as the only origin scheme accepted.
///
/// Test: `magic_dns_name_is_served_over_https_when_known_9792`.
pub fn check_target_with_scheme(
    headers: &HeaderMap,
    uri: &Uri,
    allowed: &SelfAuthorities,
    scheme: &str,
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
        && !origin.eq_ignore_ascii_case(&format!("{scheme}://{host}"))
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
