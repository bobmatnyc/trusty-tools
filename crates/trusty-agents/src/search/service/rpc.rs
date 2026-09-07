//! The wire half of the search daemon: five methods over a hardened Unix socket.
//!
//! Why (#6433, ADR-0032): this daemon used to bind loopback TCP on an
//! auto-assigned port, publish `{pid, port}` to
//! `.trusty-agents/state/search.pid`, and serve five `/search/*` routes through
//! axum. It was the surface #3335 flagged as undeclared — nothing widened it,
//! but nothing enforced that either, and it appeared in no port table. ADR-0032
//! makes UDS the inter-service transport and `trusty-console` the only HTTP
//! surface in the workspace; `trusty-review` (#6277) and `trusty-analyze`
//! (#6287) went through this path first, and this module is that migration
//! applied to five methods. The framing, the JSON-RPC envelope, the peer-uid
//! check and the accept loop are all
//! [`trusty_common::uds::server`]'s, so what is here is only what is this
//! daemon's: which methods exist, what they carry, and how the socket is bound.
//!
//! What:
//! - [`METHODS`] — every method name in one array, so a test asserts the set
//!   rather than re-listing it.
//! - The request and response types both ends share.
//! - [`build_router`] — those methods mapped onto [`super::handlers`].
//! - [`serve`] / [`serve_with_shutdown`] — bind, serve until SIGTERM/SIGINT,
//!   unlink.
//!
//! Test: `rpc_*` in `tests.rs`, over a real socket.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use trusty_common::uds::server::{RpcRouter, RpcServeOptions, serve_until};

use super::{SearchState, handlers};
use crate::search::indexer::CodeChunk;

/// Liveness and version — the method a client dials before it decides a daemon
/// is usable.
///
/// `SearchDaemonClient::connect_if_running` names this literally, as does the
/// `is_daemon_running` probe the PM's background watcher consults before
/// deciding whether to open the code store itself. Renaming it silently turns
/// both into "no daemon".
pub const METHOD_HEALTH: &str = "search.health";

/// Every method this daemon serves, in registration order.
///
/// Why: the `<domain>.<verb>` convention `trusty-review`'s `review.*` set
/// established. Keeping the names in one array is what lets
/// `rpc_router_registers_every_documented_method` fail on a rename rather than
/// leaving a client to discover it as `method_not_found` at runtime.
/// Test: `rpc_router_registers_every_documented_method`.
pub const METHODS: &[&str] = &[
    METHOD_HEALTH,
    "search.query",
    "search.index_file",
    "search.remove_file",
    "search.reindex",
];

/// The params of a method that takes no arguments.
///
/// Why: [`RpcRouter::typed`] decodes `params` into the handler's request type
/// before the handler runs, and `params` is absent — `Value::Null` — on a
/// well-formed call to a no-argument method. A plain unit struct refuses
/// `null`, so every health probe would answer `invalid_params`.
/// What: accepts anything and keeps nothing. A caller that sends a stray field
/// is not refused; these methods have no arguments to get wrong.
/// Test: `rpc_health_answers_with_no_params`.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct NoParams;

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(NoParams)
    }
}

/// `search.health`'s answer.
///
/// Test: `rpc_health_answers_over_a_real_socket`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// `"ok"` whenever the daemon answered at all.
    pub status: String,
    /// This crate's version, so a client can see which build it reached.
    pub version: String,
}

/// `search.query`'s params.
///
/// The three defaults reproduce what the retired HTTP body's `serde(default)`
/// attributes supplied, so a caller that sends only `query` gets the same
/// search it used to.
/// Test: `rpc_query_answers_with_hits`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    /// The search text. Empty is refused.
    pub query: String,
    /// How many hits to return.
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    /// Run KG expansion over the top-K results (#376 B1).
    #[serde(default = "default_expand_graph")]
    pub expand_graph: bool,
    /// Truncate each hit's text to seven lines (#400).
    #[serde(default)]
    pub compact: bool,
}

fn default_top_k() -> usize {
    5
}

fn default_expand_graph() -> bool {
    true
}

/// The params of `search.index_file` and `search.remove_file`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRequest {
    /// The file to index or drop, as an absolute path.
    pub path: String,
}

/// What `search.index_file` and `search.remove_file` answer.
///
/// One type for both because both report the same fact — how many chunks the
/// call touched. The retired HTTP routes spelled it `chunks` and `removed`
/// respectively, which meant two client decoders for one integer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexFileResponse {
    /// Chunks written, or chunks dropped.
    pub chunks: usize,
}

/// `search.reindex`'s answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReindexResponse {
    /// `"started"` when this call spawned the walk, `"already-running"` when
    /// one was already in flight.
    pub status: String,
}

/// Map every method onto its handler.
///
/// Why: the only trusty-agents-specific half of the server. The peer-uid check,
/// the framing, the envelope and the accept loop are
/// [`trusty_common::uds::server`]'s.
/// What: each method registered with [`RpcRouter::typed`], so a handler sees
/// its own request type and a decode failure becomes a coded `invalid_params`
/// frame rather than a dropped connection.
/// Test: `rpc_router_registers_every_documented_method`.
///
/// `search.health` registers with [`RpcRouter::typed_liveness`] for the reason
/// #6621 gives: every caller of a health method is a monitor, and answering one
/// must not count as the traffic that keeps a serve loop alive.
pub fn build_router(state: SearchState) -> RpcRouter {
    let query_state = state.clone();
    let index_state = state.clone();
    let remove_state = state.clone();
    let reindex_state = state.clone();

    RpcRouter::new()
        .typed_liveness::<NoParams, HealthResponse, _, _>(METHOD_HEALTH, move |_| async move {
            Ok(handlers::health())
        })
        .typed::<QueryRequest, Vec<CodeChunk>, _, _>("search.query", move |req| {
            let state = query_state.clone();
            async move { handlers::query(&state, req).await }
        })
        .typed::<PathRequest, IndexFileResponse, _, _>("search.index_file", move |req| {
            let state = index_state.clone();
            async move { handlers::index_file(&state, req).await }
        })
        .typed::<PathRequest, IndexFileResponse, _, _>("search.remove_file", move |req| {
            let state = remove_state.clone();
            async move { handlers::remove_file(&state, req).await }
        })
        .typed::<NoParams, ReindexResponse, _, _>("search.reindex", move |_| {
            let state = reindex_state.clone();
            async move { Ok(handlers::reindex(&state).await) }
        })
}

/// Bind `socket` and serve until SIGTERM/SIGINT, then unlink it.
///
/// # Errors
///
/// As [`serve_with_shutdown`].
///
/// Test: `rpc_health_answers_over_a_real_socket`.
pub async fn serve(state: SearchState, socket: &Path) -> anyhow::Result<()> {
    serve_with_shutdown(state, socket, trusty_common::shutdown_signal()).await
}

/// [`serve`], with the shutdown future supplied by the caller.
///
/// Why both signatures exist: `serve` waits on SIGTERM/SIGINT, which a test
/// cannot deliver to its own process without stopping the whole test binary.
/// Taking the future as a parameter is what lets
/// `rpc_unlinks_its_socket_on_shutdown` drive the real shutdown path and assert
/// the file is gone, instead of re-implementing the loop and deleting the file
/// itself — a test that would pass whether or not the unlink below exists.
///
/// Why the bind is [`trusty_common::uds::bind_singleton_hardened`] rather than
/// [`trusty_common::uds::bind_hardened`]: a predecessor that is SIGKILLed never
/// reaches the unlink, and leaves its socket file behind. `bind_hardened`
/// refuses an occupied path, so the next `--search-service` would fail to start
/// with no operator-visible cause. `bind_singleton_hardened` probes first and
/// takes over only a path the kernel proved nobody is serving, so a live daemon
/// is still never clobbered. That probe is this daemon's stale-socket recovery.
///
/// The unlink runs BEFORE the listener is dropped — the order
/// `trusty_common::webhook_relay::listener` records, because reversed there is
/// a window in which nothing answers the path but the file is still there, and
/// a successor that rebinds in that window has its fresh socket deleted by this
/// process.
///
/// # Errors
///
/// When the socket cannot be bound — including
/// `UdsSecurityError::AlreadyServing`, which means another search daemon is
/// live on this path and this process must not start.
///
/// Test: `rpc_unlinks_its_socket_on_shutdown`,
/// `rpc_takes_over_a_socket_left_by_a_dead_daemon`.
pub async fn serve_with_shutdown(
    state: SearchState,
    socket: &Path,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> anyhow::Result<()> {
    // #6433: no `SelfOrigins` guard is carried over, and its absence is
    // deliberate. That machinery was browser-CSRF defence for an HTTP surface a
    // page could reach; on a Unix socket the trust boundary is the 0700
    // directory, the 0600 socket, and the `ensure_peer_is_self` uid check
    // `serve_until` runs on every accepted connection before a byte is read.
    let listener = trusty_common::uds::bind_singleton_hardened(socket)
        .await
        .map_err(|e| anyhow::anyhow!("bind search daemon socket at {}: {e}", socket.display()))?;

    let router = Arc::new(build_router(state));
    tracing::info!(socket = %socket.display(), methods = router.method_names().count(), "search daemon serving");

    serve_until(&listener, router, RpcServeOptions::default(), shutdown).await;

    if let Err(e) = std::fs::remove_file(socket) {
        tracing::debug!(socket = %socket.display(), error = %e, "socket already gone");
    }
    drop(listener);
    Ok(())
}
