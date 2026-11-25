//! Same-origin guard for destructive console write routes: the shared
//! `trusty_common::server::origin_guard` primitive, plus the console's own
//! MagicDNS origins in `--tailscale` mode (#11749).
//!
//! Why: the guard originated here (#1222 review #3, router-wide since #3268,
//! bind-aware since #3269, landed in #3280) but the sibling trusty-* daemons
//! needed the exact same defence, so the implementation was lifted verbatim
//! into `trusty-common` (architecture review tranche 1, #3304). The shared
//! guard trusts loopback and the server's own bind IPs only. A page loaded
//! over the tailnet by name — `https://<node>.ts.net` through `tailscale
//! serve`, or `http://<node>.ts.net:<port>` on the tailnet listener — sends its
//! MagicDNS origin, which the shared guard refused with `cross-origin write
//! requests are not allowed` (#11749).
//! What: re-exports the shared items; [`WriteOrigins`] adds this node's
//! MagicDNS name ([`OwnMagicDns`], learned at runtime, never hardcoded) as
//! exact origins when the console serves with `--tailscale`;
//! [`guard_console_write_origin`] is the router-wide layer using it.
//! Test: `crate::server::tests` (`write_route_rejects_cross_origin`, …) for the
//! shared semantics; `routes::origin_guard_tests` for the MagicDNS origins.

use std::net::SocketAddr;

use axum::extract::{Request, State};
use axum::http::header::ORIGIN;
use axum::middleware::Next;
use axum::response::Response;

pub use trusty_common::server::origin_guard::{
    SelfOrigins, guard_write_origin, origin_is_loopback, origin_matches_self,
};

use crate::tailnet_peer::OwnMagicDns;

/// The origins the console's write routes trust.
///
/// Why: #11749 — in `--tailscale` mode the console is also reached by its
/// MagicDNS name, which no IP allowlist can name.
/// What: the bind-derived [`SelfOrigins`], plus, in `--tailscale` mode, this
/// node's MagicDNS name and the tailnet listener ports. `Default` and
/// `From<SelfOrigins>` carry no name: loopback and bind IPs only.
/// Test: `write_origins_trust_only_the_exact_magic_dns_origins_11749`.
#[derive(Debug, Clone, Default)]
pub struct WriteOrigins {
    addrs: SelfOrigins,
    magic_dns: Option<OwnMagicDns>,
    tailnet_ports: Vec<u16>,
}

impl From<SelfOrigins> for WriteOrigins {
    fn from(addrs: SelfOrigins) -> Self {
        Self {
            addrs,
            ..Self::default()
        }
    }
}

impl WriteOrigins {
    /// Origins for a console bound on `addrs`; `magic_dns` is `Some` only in
    /// `--tailscale` mode.
    ///
    /// What: [`SelfOrigins::from_bind_addrs`], plus the name with the ports
    /// of the non-loopback addresses (the tailnet listeners).
    /// Test: `write_origins_trust_only_the_exact_magic_dns_origins_11749`.
    pub fn from_bind_addrs(addrs: &[SocketAddr], magic_dns: Option<OwnMagicDns>) -> Self {
        Self {
            addrs: SelfOrigins::from_bind_addrs(addrs),
            magic_dns,
            tailnet_ports: addrs
                .iter()
                .filter(|a| !a.ip().is_loopback())
                .map(SocketAddr::port)
                .collect(),
        }
    }

    /// Whether `origin` is loopback, a bind IP, or this node's MagicDNS origin.
    /// Test: `write_origins_trust_only_the_exact_magic_dns_origins_11749`.
    pub fn trusts(&self, origin: &str) -> bool {
        origin_is_loopback(origin)
            || origin_matches_self(origin, &self.addrs)
            || self.matches_magic_dns(origin)
    }

    /// Whether `origin` is exactly `https://<name>` (what `tailscale serve`
    /// fronts on the loopback listener) or `http://<name>:<port>` for a
    /// tailnet listener port. Exact comparison, so no lookalike host, other
    /// scheme or other port matches. No name known → `false` (fail closed).
    fn matches_magic_dns(&self, origin: &str) -> bool {
        let Some(name) = self.magic_dns.as_ref().and_then(OwnMagicDns::get) else {
            return false;
        };
        origin.eq_ignore_ascii_case(&format!("https://{name}"))
            || self
                .tailnet_ports
                .iter()
                .any(|port| origin.eq_ignore_ascii_case(&format!("http://{name}:{port}")))
    }
}

/// The console's router-wide write guard: [`guard_write_origin`], which also
/// admits this node's MagicDNS origin in `--tailscale` mode (#11749).
///
/// What: a state-changing request whose `Origin` is a [`WriteOrigins`]
/// MagicDNS origin passes; every other request goes to the shared guard
/// unchanged, so its refusals keep their `403` and message.
/// Test: `tailnet_origin_write_is_accepted_under_tailscale_11749`,
/// `lookalike_origin_writes_are_refused_11749`.
pub async fn guard_console_write_origin(
    State(origins): State<WriteOrigins>,
    req: Request,
    next: Next,
) -> Response {
    let named = !req.method().is_safe()
        && req
            .headers()
            .get(ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|o| origins.matches_magic_dns(o));
    if named {
        return next.run(req).await;
    }
    guard_write_origin(State(origins.addrs), req, next).await
}
