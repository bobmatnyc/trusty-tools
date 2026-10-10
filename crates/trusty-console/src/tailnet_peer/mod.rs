//! Peer-identity gate for every non-loopback console listener (#9035, #7524).
//!
//! Why: `--tailscale` binds a second listener on the tailnet address, and before
//! #9035 it served every route, writes included, to any tailnet node the ACLs
//! let reach the port. The origin guard does not help there: it only inspects an
//! `Origin` header when one is present, so `curl` from a foreign node passed.
//! A peer gate alone is not enough either: CORS is permissive, so a foreign
//! page in the owner's own browser on another of the owner's devices could read
//! `GET` responses, and DNS rebinding reaches the same place.
//! What: [`TailnetPeerGate`] decides, per peer address, whether the node behind
//! it belongs to this machine's own Tailscale login and is untagged. Anything
//! else — a foreign login, a tagged node, or an identity that cannot be
//! determined — is refused with `403` before routing, on every route. An
//! allowed peer must then also name this listener exactly in `Host`, and any
//! `Origin` it sends, on any method, must be that same self-origin
//! ([`self_origin::check_target`]). [`serve_listeners`] is the one place the
//! console's listeners are bound and served; it gates every listener whose
//! address is not loopback, whatever bind mode produced it (#7524), so neither
//! check can be left off. Loopback listeners are served ungated.
//! Lookups are bounded by a timeout and cached per peer address, single-flight,
//! so a request burst runs one `tailscale whois`, not one per request.
//! Test: `tailnet_peer/tests.rs`.

mod self_origin;
mod whois;

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::future::BoxFuture;
use tokio::sync::OnceCell;
// #9035: tokio's clock, so a paused test clock can expire cache entries.
use tokio::time::Instant;

pub use self_origin::{SelfAuthorities, check_target};
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
    /// are equal. Any lookup failure is [`PeerVerdict::Unresolved`], and so is
    /// a wildcard host address, without a lookup.
    /// Test: `same_login_peer_is_served`, `foreign_login_peer_gets_403_on_every_route`,
    /// `resolver_error_fails_closed`, `tagged_peer_is_refused`,
    /// `wildcard_host_address_fails_closed_7524`.
    pub async fn authorize(&self, peer: IpAddr) -> PeerVerdict {
        // #7524: a wildcard bind has no tailnet address of its own, so there is
        // no host login to compare against. Refuse rather than ask `whois`.
        if self.host_ip.is_unspecified() {
            return PeerVerdict::Unresolved(format!(
                "listener is bound to the wildcard address {}",
                self.host_ip
            ));
        }
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

    /// The host node's MagicDNS name, once its identity has resolved.
    pub async fn host_node_name(&self) -> Option<String> {
        self.identity(self.host_ip).await.ok()?.node_name
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

/// Middleware state for one tailnet listener.
#[derive(Clone)]
pub struct TailnetGuard {
    /// The peer-identity gate.
    pub gate: Arc<TailnetPeerGate>,
    /// The listener's own bound address, the base of its self-authorities.
    pub listen: SocketAddr,
}

/// Axum middleware: refuse any request whose tailnet peer the gate does not
/// allow, or whose `Host`/`Origin` is not this listener's own.
///
/// Why: layered outermost on the tailnet listener's router, so it runs before
/// routing, CORS and the origin guard — every route, every method, the fallback
/// included.
/// What: reads the peer address from `ConnectInfo` (missing → refuse, with no
/// lookup), asks [`TailnetPeerGate::authorize`], then runs [`check_target`]
/// against the listen address and the host's MagicDNS name. Every refusal is
/// logged at WARN with the peer address.
/// Test: `missing_connect_info_is_refused_without_a_lookup`,
/// `foreign_login_peer_gets_403_on_every_route`,
/// `wrong_host_is_refused_for_an_allowed_peer`,
/// `foreign_origin_is_refused_on_get`, `self_origin_is_served`.
pub async fn guard_tailnet_peer(
    State(guard): State<TailnetGuard>,
    req: Request,
    next: Next,
) -> Response {
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() else {
        tracing::warn!("tailnet listener refused a request with no peer address");
        return forbidden();
    };
    match guard.gate.authorize(peer.ip()).await {
        PeerVerdict::Allow => {}
        PeerVerdict::ForeignLogin(login) => {
            tracing::warn!(peer = %peer, login = %login, "tailnet listener refused a peer owned by another login");
            return forbidden();
        }
        PeerVerdict::Tagged => {
            tracing::warn!(peer = %peer, "tailnet listener refused a tagged node (or the host is tagged)");
            return forbidden();
        }
        PeerVerdict::Unresolved(reason) => {
            tracing::warn!(peer = %peer, reason = %reason, "tailnet listener refused a peer whose identity could not be determined");
            return forbidden();
        }
    }
    let name = guard.gate.host_node_name().await;
    let allowed = SelfAuthorities::new(guard.listen, name.as_deref());
    if let Err(reason) = check_target(req.headers(), req.uri(), &allowed) {
        tracing::warn!(peer = %peer, reason, "tailnet listener refused a request not addressed to itself");
        return forbidden();
    }
    next.run(req).await
}

/// Serve `router` on the tailnet listener behind the peer gate.
///
/// Why: the one serve path for a tailnet listener, so tests drive the exact
/// production wiring.
/// What: layers [`guard_tailnet_peer`] outermost, keyed to the listener's own
/// bound address, and serves with `ConnectInfo<SocketAddr>`, which the guard
/// reads the peer address from.
/// Test: `same_login_peer_is_served`, `foreign_login_peer_gets_403_on_every_route`.
pub async fn serve_tailnet(
    listener: tokio::net::TcpListener,
    router: Router,
    gate: Arc<TailnetPeerGate>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let listen = listener.local_addr()?;
    // #9035: the tailnet listener must never serve an unauthenticated peer.
    let app = router.layer(axum::middleware::from_fn_with_state(
        TailnetGuard { gate, listen },
        guard_tailnet_peer,
    ));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// Every console listener, bound and serving ([`serve_listeners`]).
pub struct ConsoleListeners {
    /// Bound addresses, in the requested order; the first is the primary.
    pub bound: Vec<SocketAddr>,
    primary: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl ConsoleListeners {
    /// Wait until the primary listener stops: graceful shutdown, or an error.
    pub async fn wait_primary(self) -> anyhow::Result<()> {
        self.primary
            .await
            .context("primary listener task failed")?
            .context("server error")
    }
}

/// Bind every address in `addrs` and serve `router` on each; every listener
/// whose address is not loopback is served behind its own peer gate.
///
/// Why: `run_serve` serves all of its listeners through here, so the gate is
/// chosen by the bound IP, never by list position. Before #7524 the first
/// address was served plain, so an Explicit `--http 0.0.0.0:7788` (or
/// `TRUSTY_CONSOLE_BIND`) served every route to any peer. The resolver is a
/// parameter so the tests need no tailnet.
/// What: binds each address in order (a bind failure aborts). A loopback
/// listener is served by plain `axum::serve`, as before; any other goes
/// through [`serve_tailnet`] with a [`TailnetPeerGate`] for its bound IP. A
/// wildcard bind has no host identity, so its gate refuses every request
/// ([`TailnetPeerGate::authorize`]). The first listener is the primary, whose
/// task [`ConsoleListeners::wait_primary`] awaits; the rest log their exit.
/// Test: `explicit_non_loopback_listener_is_gated_7524`,
/// `listener_gating_follows_the_ip_not_the_position_7524`.
pub async fn serve_listeners<S, F>(
    addrs: &[SocketAddr],
    router: &Router,
    resolver: Arc<dyn PeerResolver>,
    shutdown: S,
) -> anyhow::Result<ConsoleListeners>
where
    S: Fn() -> F,
    F: Future<Output = ()> + Send + 'static,
{
    let mut bound = Vec::with_capacity(addrs.len());
    let mut primary = None;
    for &addr in addrs {
        let listener = crate::bind::bind_listener(addr).await?;
        let local = listener.local_addr().context("get local addr")?;
        // #7524: select the gate by the bound IP, never by list position.
        let serve: BoxFuture<'static, std::io::Result<()>> = if local.ip().is_loopback() {
            let (app, stop) = (router.clone(), shutdown());
            Box::pin(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(stop)
                    .await
            })
        } else {
            if local.ip().is_unspecified() {
                tracing::warn!(
                    "trusty-console listener {local} is a wildcard bind; the tailnet peer \
                     gate refuses every request on it (bind the tailnet address, or use --tailscale)"
                );
            }
            eprintln!("trusty-console (tailnet peer gate): http://{local}");
            let gate = Arc::new(TailnetPeerGate::new(Arc::clone(&resolver), local.ip()));
            Box::pin(serve_tailnet(listener, router.clone(), gate, shutdown()))
        };
        if primary.is_none() {
            tracing::info!("trusty-console listening on http://{local}");
            primary = Some(tokio::spawn(serve));
        } else {
            tracing::info!("trusty-console also listening on http://{local}");
            tokio::spawn(async move {
                if let Err(e) = serve.await {
                    tracing::warn!("extra listener {local} exited: {e}");
                }
            });
        }
        bound.push(local);
    }
    let primary = primary.context("bind address list is empty")?;
    Ok(ConsoleListeners { bound, primary })
}

#[cfg(test)]
mod tests;
