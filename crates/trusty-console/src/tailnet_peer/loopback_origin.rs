//! Host/Origin check for the console's loopback listener (#9792).
//!
//! Why: the loopback listener served every route to any origin, so any web
//! page the operator visited could read session paths, pids, tmux names, the
//! disk tree and host metrics; a DNS-rebinding page reached the same data
//! under a foreign `Host`. The tailnet listener already checks Host/Origin
//! (#9035); this is the loopback listener's equivalent.
//! What: [`check_loopback_target`] admits a request whose `Host` is the
//! listener's `ip:port` or `localhost:port` with any `Origin` equal to
//! `http://<Host>`, or — once [`OwnMagicDns`] holds a name — whose `Host` is
//! that name with any `Origin` equal to `https://<name>`. An absent `Origin`
//! is admitted when `Host` matches (`curl`, `tm`, the reqwest proxy).
//! [`guard_loopback`] layers it over the whole router, every method, before
//! routing, so the proxy routes, the SSE stream and the fallback are covered.
//! Test: `tailnet_peer/loopback_tests.rs`.

use std::net::SocketAddr;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::self_origin::check_target_with_scheme;
use super::{OwnMagicDns, SelfAuthorities, check_target};

/// Decide whether a request to the loopback listener on `listen` is addressed
/// to the console itself.
///
/// What: the loopback authorities over `http`, then the MagicDNS name, when
/// known, over `https`. On refusal returns the loopback check's reason.
/// Test: `foreign_host_is_refused_9792`,
/// `wrong_port_origin_is_refused_on_reads_and_writes_9792`,
/// `magic_dns_name_is_served_over_https_when_known_9792`.
pub fn check_loopback_target(
    headers: &HeaderMap,
    uri: &Uri,
    listen: SocketAddr,
    magic_dns: Option<&str>,
) -> Result<(), &'static str> {
    let reason = match check_target(headers, uri, &SelfAuthorities::loopback(listen)) {
        Ok(()) => return Ok(()),
        Err(reason) => reason,
    };
    // #9792 Q4: no name recorded means no second authority (fail closed).
    let Some(name) = magic_dns else {
        return Err(reason);
    };
    check_target_with_scheme(headers, uri, &SelfAuthorities::exact(name), "https")
        .map_err(|_| reason)
}

/// Middleware state for one loopback listener.
#[derive(Clone)]
struct LoopbackGuard {
    listen: SocketAddr,
    magic_dns: OwnMagicDns,
}

/// Refuse with 403 any request [`check_loopback_target`] does not admit.
async fn guard_loopback_target(
    State(guard): State<LoopbackGuard>,
    req: Request,
    next: Next,
) -> Response {
    let checked = check_loopback_target(
        req.headers(),
        req.uri(),
        guard.listen,
        guard.magic_dns.get(),
    );
    if let Err(reason) = checked {
        tracing::warn!(
            listen = %guard.listen,
            path = req.uri().path(),
            reason,
            "loopback listener refused a request not addressed to the console"
        );
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "request is not addressed to this console",
            })),
        )
            .into_response();
    }
    next.run(req).await
}

/// Wrap `router` for a loopback listener bound on `listen`.
///
/// Why: one wrapper, so `serve_listeners` and the tests serve the same thing.
/// What: layers the Host/Origin check outermost; `magic_dns` is read per
/// request, so a name learned after startup takes effect without a restart.
/// Test: `foreign_origin_is_refused_on_reads_proxies_and_sse_9792`,
/// `own_origins_are_served_9792`, `read_route_refuses_cross_origin`.
pub fn guard_loopback(router: Router, listen: SocketAddr, magic_dns: OwnMagicDns) -> Router {
    router.layer(axum::middleware::from_fn_with_state(
        LoopbackGuard { listen, magic_dns },
        guard_loopback_target,
    ))
}
