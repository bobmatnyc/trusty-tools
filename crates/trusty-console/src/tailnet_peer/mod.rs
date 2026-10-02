//! Peer-identity gate for the console's tailnet listener (#9035).
//!
//! Why: `--tailscale` binds a second listener on the tailnet address, and before
//! #9035 it served every route, writes included, to any tailnet node the ACLs
//! let reach the port. The origin guard does not help there: it only inspects an
//! `Origin` header when one is present, so `curl` from a foreign node passed.
//! What: [`TailnetPeerGate`] decides, per peer address, whether the node behind
//! it belongs to this machine's own Tailscale login and is untagged. Anything
//! else — a foreign login, a tagged node, or an identity that cannot be
//! determined — is refused with `403` before routing, on every route.
//! [`serve_tailnet`] is the one place the tailnet listener is served, so the
//! gate cannot be left off it. The loopback listener does not use this module.
//! Lookups are bounded by a timeout and cached per peer address, single-flight,
//! so a request burst runs one `tailscale whois`, not one per request.
//! Test: `tailnet_peer/tests.rs`.

mod whois;

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tokio::sync::OnceCell;
// #9035: tokio's clock, so a paused test clock can expire cache entries.
use tokio::time::Instant;

pub use whois::{PeerIdentity, PeerResolver, TailscaleCliResolver, WhoisError};

/// Upper bound on one identity lookup; past it the request is refused.
pub const WHOIS_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a resolved identity is reused for the same peer address.
pub const IDENTITY_TTL: Duration = Duration::from_secs(30);
/// How long a failed lookup is reused, so an outage does not spawn per request.
pub const FAILURE_TTL: Duration = Duration::from_secs(5);
/// Cache size past which expired entries are swept on insert.
const CACHE_SWEEP_THRESHOLD: usize = 256;

/// The gate's decision for one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerVerdict {
    /// Same login as the host, untagged node: serve the request.
    Allow,
    /// The peer belongs to a different login.
    ForeignLogin(String),
    /// The peer (or the host) is a tagged node with no human owner.
    Tagged,
    /// The peer's or the host's identity could not be determined.
    Unresolved(String),
}

/// One cached lookup outcome and when it was produced.
struct Resolved {
    at: Instant,
    result: Result<PeerIdentity, String>,
}

impl Resolved {
    fn expired(&self, now: Instant) -> bool {
        let ttl = if self.result.is_ok() {
            IDENTITY_TTL
        } else {
            FAILURE_TTL
        };
        now.duration_since(self.at) >= ttl
    }
}

type Slot = Arc<OnceCell<Resolved>>;

/// Decides whether a tailnet peer may use the console.
///
/// Why: the policy (same login, untagged, fail closed) has to hold for every
/// request on the tailnet listener and be testable without a tailnet.
/// What: holds the injected [`PeerResolver`], the host's own tailnet address
/// (whose identity is resolved the same way and defines "the host's login"),
/// and a per-address cache of lookup outcomes.
/// Test: `tailnet_peer/tests.rs`.
pub struct TailnetPeerGate {
    resolver: Arc<dyn PeerResolver>,
    host_ip: IpAddr,
    timeout: Duration,
    cache: Mutex<HashMap<IpAddr, Slot>>,
}

impl TailnetPeerGate {
    /// Build a gate for a listener bound on `host_ip`, the host's own tailnet
    /// address.
    pub fn new(resolver: Arc<dyn PeerResolver>, host_ip: IpAddr) -> Self {
        Self::with_timeout(resolver, host_ip, WHOIS_TIMEOUT)
    }

    /// [`Self::new`] with an explicit lookup bound, for tests.
    pub fn with_timeout(
        resolver: Arc<dyn PeerResolver>,
        host_ip: IpAddr,
        timeout: Duration,
    ) -> Self {
        Self {
            resolver,
            host_ip,
            timeout,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Decide whether the node at `peer` may be served.
    ///
    /// Why: one decision function, so the middleware has no policy of its own.
    /// What: resolves the host's identity and the peer's, both through the
    /// cache. Allows only when both resolve, neither is tagged, and the logins
    /// are equal. Any lookup failure is [`PeerVerdict::Unresolved`].
    /// Test: `same_login_peer_is_served`, `foreign_login_peer_gets_403_on_every_route`,
    /// `resolver_error_fails_closed`, `tagged_peer_is_refused`.
    pub async fn authorize(&self, peer: IpAddr) -> PeerVerdict {
        let host = match self.identity(self.host_ip).await {
            Ok(id) => id,
            Err(e) => return PeerVerdict::Unresolved(format!("host identity: {e}")),
        };
        let peer_id = match self.identity(peer).await {
            Ok(id) => id,
            Err(e) => return PeerVerdict::Unresolved(e),
        };
        if host.tagged || peer_id.tagged {
            return PeerVerdict::Tagged;
        }
        if peer_id.login == host.login {
            PeerVerdict::Allow
        } else {
            PeerVerdict::ForeignLogin(peer_id.login)
        }
    }

    /// Resolve `ip` through the cache: fresh hits are reused, concurrent misses
    /// share one in-flight lookup, and the lookup is bounded by `self.timeout`.
    async fn identity(&self, ip: IpAddr) -> Result<PeerIdentity, String> {
        let slot = self.slot_for(ip);
        let resolved = slot
            .get_or_init(|| async {
                let result = match tokio::time::timeout(self.timeout, self.resolver.whois(ip)).await
                {
                    Ok(Ok(id)) => Ok(id),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(_) => Err(WhoisError::Timeout(self.timeout).to_string()),
                };
                Resolved {
                    at: Instant::now(),
                    result,
                }
            })
            .await;
        resolved.result.clone()
    }

    /// The cache slot for `ip`: the existing one unless its outcome expired.
    fn slot_for(&self, ip: IpAddr) -> Slot {
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = cache.get(&ip)
            && slot.get().is_none_or(|r| !r.expired(now))
        {
            return Arc::clone(slot);
        }
        if cache.len() >= CACHE_SWEEP_THRESHOLD {
            cache.retain(|_, s| s.get().is_none_or(|r| !r.expired(now)));
        }
        let slot = Slot::default();
        cache.insert(ip, Arc::clone(&slot));
        slot
    }
}

/// The body every refusal carries. One text for every reason, so a probe learns
/// nothing about why it was refused.
fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({
            "error": "tailnet peer is not authorized for this console",
        })),
    )
        .into_response()
}

/// Axum middleware: refuse any request whose tailnet peer the gate does not
/// allow.
///
/// Why: layered outermost on the tailnet listener's router, so it runs before
/// routing, CORS and the origin guard — every route, every method, the fallback
/// included, and regardless of any `Origin` header.
/// What: reads the peer address from `ConnectInfo` (missing → refuse), asks
/// [`TailnetPeerGate::authorize`], and logs every refusal at WARN with the peer
/// address.
/// Test: the `serve_tailnet` cases in `tailnet_peer/tests.rs`.
pub async fn guard_tailnet_peer(
    State(gate): State<Arc<TailnetPeerGate>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() else {
        tracing::warn!("tailnet listener refused a request with no peer address");
        return forbidden();
    };
    match gate.authorize(peer.ip()).await {
        PeerVerdict::Allow => next.run(req).await,
        PeerVerdict::ForeignLogin(login) => {
            tracing::warn!(peer = %peer, login = %login, "tailnet listener refused a peer owned by another login");
            forbidden()
        }
        PeerVerdict::Tagged => {
            tracing::warn!(peer = %peer, "tailnet listener refused a tagged node (or the host is tagged)");
            forbidden()
        }
        PeerVerdict::Unresolved(reason) => {
            tracing::warn!(peer = %peer, reason = %reason, "tailnet listener refused a peer whose identity could not be determined");
            forbidden()
        }
    }
}

/// Serve `router` on the tailnet listener behind the peer gate.
///
/// Why: the one serve path for the tailnet listener, so `run_serve` cannot wire
/// it without the gate, and tests drive the exact production wiring.
/// What: layers [`guard_tailnet_peer`] outermost and serves with
/// `ConnectInfo<SocketAddr>`, which the guard reads the peer address from.
/// Test: `serve_tailnet` cases in `tailnet_peer/tests.rs`.
pub async fn serve_tailnet(
    listener: tokio::net::TcpListener,
    router: Router,
    gate: Arc<TailnetPeerGate>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    // #9035: the tailnet listener must never serve an unauthenticated peer.
    let app = router.layer(axum::middleware::from_fn_with_state(
        gate,
        guard_tailnet_peer,
    ));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

#[cfg(test)]
mod tests;
