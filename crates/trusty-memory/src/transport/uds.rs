//! The wire half of the daemon: one hardened Unix socket (#6286, ADR-0032).
//!
//! Why: trusty-memory used to bind TCP loopback on `127.0.0.1:7070`, walk to
//! `:7079` when that was taken, publish the winner in an `http_addr` discovery
//! file, and serve forty axum routes plus an `/sse` broadcast and an embedded
//! SPA. ADR-0032 makes UDS the inter-service transport and `trusty-console` the
//! only HTTP surface in the workspace. The framing, the JSON-RPC envelope, the
//! peer check and the accept loop all come from [`trusty_common::uds::server`],
//! so this module is only the part that is trusty-memory's.
//!
//! What, and how it differs from the two daemons that migrated first:
//!
//! - **The whole tool surface mounts as ONE fallback.**
//!   [`crate::transport::rpc::dispatch`] was already a transport-agnostic
//!   `(method, params)` router over ~75 names — the MCP protocol arms,
//!   `tools/call`, and every `palace_*` / `memory_*` / `kg_*` tool. Re-listing
//!   those names here would be a second copy of a table that already exists,
//!   which is the drift the workspace's common-entry-point rule exists to
//!   prevent, so [`RpcFallback`] carries them instead. `trusty-review` and
//!   `trusty-analyze` had no such dispatcher and registered every method by
//!   name; this one does, and [`RpcRouter::fallback`] (#6295) is the seam that
//!   made reusing it possible.
//! - **[`FOLDED_METHODS`] is what the fallback does NOT cover** — the roughly
//!   twenty endpoints that existed only as axum routes. They are registered by
//!   name because they are new names, not because the dispatcher was
//!   re-derived.
//! - **`memory.chat` streams.** It is the reason
//!   `trusty_common::uds::server`'s multi-frame extension exists: the chat
//!   handler pushes LLM tokens as the model produces them, and a
//!   one-frame-per-connection answer would have meant buffering the whole
//!   completion. See [`crate::chat::handler`].
//!
//! **`/sse` is not here, and nothing replaces it.** Its only subscribers were
//! this crate's own embedded SPA (`ui/src/main.js`, `lib/base.js`,
//! `lib/state.svelte.js`, `components/ActivityFeed.svelte`) and its own test —
//! checked, not assumed, the same way #6287 checked trusty-analyze's. The
//! console-hosted dashboard polls `memory.status` and `memory.activity`.
//!
//! **No discovery file is written.** [`socket_path`] is derived from the data
//! directory, so it is what this daemon binds and what every consumer dials;
//! there is nothing for a stale file to disagree with.
//!
//! Test: `uds_tests.rs` — `rpc_*`, over a real socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde_json::Value;
use tracing::info;
use trusty_common::uds::server::{serve_until, RpcError, RpcFallback, RpcRouter, RpcServeOptions};

use crate::transport::methods::{activity, admin, chat, health, kg, palaces};
use crate::transport::rpc::{dispatch, JsonRpcRequest};
use crate::{is_data_dir_override_active, AppState};

/// The method `trusty-console`'s connector and `tctl`'s probe dial.
///
/// Renaming it breaks both, in crates that have no Cargo edge on this one.
pub const METHOD_HEALTH: &str = "memory.health";

/// The methods this module registers by name (#6286).
///
/// Why an array: these are the names a consumer outside this crate dials by
/// literal, and an array is what a contract test can compare against — so a
/// rename shows up as a failing assertion rather than as a consumer silently
/// reporting `method_not_found`.
///
/// **This is not the whole surface.** Everything
/// [`crate::transport::rpc::dispatch`] routes is served too, through the
/// fallback, and is deliberately absent here: the router does not know those
/// names, which is the point of mounting a dispatcher rather than restating
/// its table. [`dispatcher_method_count`] is how a caller learns the size of
/// the other half.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub const FOLDED_METHODS: &[&str] = &[
    METHOD_HEALTH,
    "memory.status",
    "memory.config",
    "memory.palace_get",
    "memory.drawers_list",
    "memory.drawer_create",
    "memory.drawer_delete",
    "memory.kg_all",
    "memory.kg_count",
    "memory.kg_subjects_with_counts",
    "memory.kg_graph",
    "memory.kg_graph_seed",
    "memory.kg_graph_neighbors",
    "memory.kg_delete_triple",
    "memory.dream_status",
    "memory.palace_dream_status",
    "memory.dream_run",
    "memory.activity",
    "memory.logs_tail",
    "memory.admin_stop",
    "memory.remember_async",
    "memory.chat_providers",
    "memory.messages_list",
    "memory.message_send",
    "memory.message_mark_read",
];

/// The streaming method, registered separately because streaming and unary
/// names live in separate tables (#6286).
pub const STREAM_METHODS: &[&str] = &["memory.chat"];

/// Largest frame this service reads or writes, in bytes.
///
/// Why not [`trusty_common::uds::MAX_FRAME_BYTES`] (8 MiB): that figure is
/// sized for control-plane frames. Three of the folded methods carry bulk that
/// nothing bounds before it arrives — `memory.kg_graph` returns a whole
/// palace's active triple set, `memory.kg_all` pages through the same corpus,
/// and `memory.activity` can be asked for 500 rows whose payloads are
/// arbitrary `DaemonEvent` bodies.
///
/// **This figure is inherited, not measured.** It is `trusty-review`'s and
/// `trusty-analyze`'s 32 MiB, adopted because all three face the same shape of
/// problem. What is known without measuring: `kg_graph` is already capped at
/// `KG_GRAPH_MAX_TRIPLES` on the service side and reports `truncated`, so the
/// frame budget is the second bound rather than the first. If a real palace
/// exceeds it, the symptom is a refused method naming the budget — a visible
/// failure, not a silent truncation.
///
/// **It is per frame, in both directions.** On `memory.chat` it governs each
/// token frame separately rather than the stream's total, and a client's own
/// budget must match or one end has only moved which side refuses.
///
/// Test: `rpc_accepts_a_request_larger_than_the_shared_default`,
/// `rpc_refuses_a_request_past_its_own_budget`.
pub const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

/// Mounts [`crate::transport::rpc::dispatch`] as the router's catch-all.
///
/// Why: see the module docs — the dispatcher already owns the tool surface, and
/// registering those names a second time here is the drift this avoids.
/// What: hands the method name and params straight through, wrapping them back
/// into the [`JsonRpcRequest`] the dispatcher takes. The id is `null` on the
/// way in and discarded on the way out: [`RpcRouter`] echoes the caller's real
/// id onto the response frame itself, so the one the dispatcher stamps would
/// only be overwritten.
///
/// A dispatcher error becomes an [`RpcError`] carrying the dispatcher's OWN
/// code — `-32601` for a method it does not know, `-32602` for bad params —
/// rather than a generic internal error, so a caller reads the reason it was
/// given.
///
/// Test: `rpc_dispatcher_method_answers_through_the_fallback`,
/// `rpc_reports_method_not_found_for_an_unknown_method`.
struct DispatchFallback {
    state: AppState,
}

#[async_trait]
impl RpcFallback for DispatchFallback {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let request = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: None,
            method: method.to_string(),
            params: Some(params),
        };
        let response = dispatch(&self.state, request).await;
        match (response.result, response.error) {
            (Some(result), _) => Ok(result),
            (None, Some(error)) => Err(RpcError::new(i64::from(error.code), error.message)),
            // The dispatcher's own contract is that exactly one of the two is
            // present. Saying so beats answering `null` as a success.
            (None, None) => Err(RpcError::internal(format!(
                "dispatcher answered {method} with neither result nor error"
            ))),
        }
    }
}

/// How many methods [`crate::transport::rpc::dispatch`] routes.
///
/// Why: [`FOLDED_METHODS`] deliberately does not list them, so nothing else
/// reports the size of the surface the fallback covers. `memory.health` prints
/// it and the router logs it, which is what turns "the fallback is mounted"
/// from an assumption into something an operator can read.
///
/// Test: `rpc_reports_the_dispatcher_surface_size`.
pub fn dispatcher_method_count() -> usize {
    crate::transport::rpc::method_names().len()
}

/// Map every folded name onto its handler, and mount the dispatcher behind
/// them.
///
/// Why: the only trusty-memory-specific half of the server. Everything else —
/// the peer-uid check, the framing, the envelope, the accept loop — is
/// [`trusty_common::uds::server`]'s.
/// What: each folded method is registered with [`RpcRouter::typed`], so the
/// handler sees its own params type and a decode failure becomes a coded
/// `invalid_params` frame rather than a dropped connection. Every handler's
/// [`ApiError`] converts through `From<ApiError> for RpcError`, which is where
/// the failure-kind-to-code mapping lives. `memory.chat` goes in through
/// [`RpcRouter::typed_stream`], and [`RpcRouter::fallback`] carries the rest.
///
/// [`ApiError`]: crate::transport::api_error::ApiError
///
/// A registered name WINS over the fallback, so a folded method never shadows
/// or is shadowed by a dispatcher one — the `memory.` prefix keeps the two sets
/// disjoint by construction, and
/// `rpc_folded_names_do_not_collide_with_dispatcher_names` asserts it.
///
/// The `state.clone()` per method is one `AppState`, which is `Arc`s and small
/// `Copy` fields — not the stores themselves.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub fn build_router(state: AppState) -> RpcRouter {
    macro_rules! bind {
        ($router:expr, $name:expr, $req:ty, $handler:path) => {{
            let state = state.clone();
            $router.typed::<$req, Value, _, _>($name, move |req| {
                let state = state.clone();
                async move { $handler(&state, req).await.map_err(RpcError::from) }
            })
        }};
    }

    use crate::transport::methods::{NoParams, PalaceParams};

    let router = RpcRouter::new();
    let router = bind!(router, METHOD_HEALTH, health::HealthQuery, health::health);
    let router = bind!(router, "memory.status", NoParams, palaces::status);
    let router = bind!(router, "memory.config", NoParams, palaces::config);
    let router = bind!(
        router,
        "memory.palace_get",
        PalaceParams,
        palaces::get_palace
    );
    let router = bind!(
        router,
        "memory.drawers_list",
        palaces::ListDrawersParams,
        palaces::list_drawers
    );
    let router = bind!(
        router,
        "memory.drawer_create",
        palaces::CreateDrawerParams,
        palaces::create_drawer
    );
    let router = bind!(
        router,
        "memory.drawer_delete",
        palaces::DeleteDrawerParams,
        palaces::delete_drawer
    );
    let router = bind!(router, "memory.kg_all", kg::KgListParams, kg::kg_all);
    let router = bind!(router, "memory.kg_count", PalaceParams, kg::kg_count);
    let router = bind!(
        router,
        "memory.kg_subjects_with_counts",
        kg::KgListParams,
        kg::kg_subjects_with_counts
    );
    let router = bind!(router, "memory.kg_graph", PalaceParams, kg::kg_graph);
    let router = bind!(
        router,
        "memory.kg_graph_seed",
        kg::KgSeedParams,
        kg::kg_graph_seed
    );
    let router = bind!(
        router,
        "memory.kg_graph_neighbors",
        kg::KgNeighborsParams,
        kg::kg_graph_neighbors
    );
    let router = bind!(
        router,
        "memory.kg_delete_triple",
        kg::DeleteTripleParams,
        kg::kg_delete_triple
    );
    let router = bind!(router, "memory.dream_status", NoParams, kg::dream_status);
    let router = bind!(
        router,
        "memory.palace_dream_status",
        PalaceParams,
        kg::palace_dream_status
    );
    let router = bind!(router, "memory.dream_run", NoParams, kg::dream_run);
    let router = bind!(
        router,
        "memory.activity",
        activity::ActivityParams,
        activity::activity
    );
    let router = bind!(
        router,
        "memory.logs_tail",
        admin::LogsTailParams,
        admin::logs_tail
    );
    let router = bind!(router, "memory.admin_stop", NoParams, admin::admin_stop);
    let router = bind!(
        router,
        "memory.remember_async",
        admin::RememberAsyncParams,
        admin::remember_async
    );
    let router = bind!(
        router,
        "memory.chat_providers",
        NoParams,
        chat::chat_providers
    );
    let router = bind!(
        router,
        "memory.messages_list",
        chat::ListMessagesParams,
        chat::messages_list
    );
    let router = bind!(
        router,
        "memory.message_send",
        chat::SendMessageParams,
        chat::message_send
    );
    let router = bind!(
        router,
        "memory.message_mark_read",
        chat::MarkReadParams,
        chat::message_mark_read
    );

    let chat_state = state.clone();
    let router = router.typed_stream::<crate::chat::ChatBody, _, _>("memory.chat", move |body| {
        let state = chat_state.clone();
        async move { crate::chat::chat_stream(&state, body).await }
    });

    router.fallback(DispatchFallback { state })
}

/// Per-connection budgets for this service.
///
/// The read timeout is the shared default; only the frame budget moves. See
/// [`MAX_FRAME_BYTES`]. The read bound does NOT cover a handler, which is what
/// makes a multi-minute `memory.dream_run` compatible with a 30-second guard
/// against a peer that connects and never writes — and what lets `memory.chat`
/// take as long as the model does between token frames.
fn serve_options() -> RpcServeOptions {
    RpcServeOptions {
        max_frame_bytes: MAX_FRAME_BYTES,
        ..RpcServeOptions::default()
    }
}

/// The socket this daemon binds, as both it and its consumers resolve it.
///
/// # Errors
///
/// When the data directory cannot be resolved or created.
pub fn socket_path() -> Result<PathBuf> {
    trusty_common::daemon_socket_path("trusty-memory")
}

/// Bind `socket` and serve until SIGTERM/SIGINT, then unlink it.
///
/// Why the bind is [`trusty_common::uds::bind_singleton_hardened`] rather than
/// `RpcServer::run`'s `bind_hardened`: this daemon is supervised by launchd
/// with `KeepAlive`, and a predecessor that is SIGKILLed never reaches the
/// unlink below. `bind_hardened` refuses an occupied path rather than
/// clobbering what might be a live owner, so the replacement launchd starts
/// would fail its bind, exit, be restarted, and fail again — a crash loop with
/// no operator-visible cause. `bind_singleton_hardened` probes first and takes
/// over only a socket the kernel proves nobody is serving.
///
/// What: removes the retired discovery files, then delegates to
/// [`serve_with_shutdown`] with the real signal future.
///
/// # Errors
///
/// When the socket cannot be bound — including
/// `UdsSecurityError::AlreadyServing`, which means another trusty-memory is
/// live on this path and this process must not start.
///
/// Test: `rpc_health_answers_over_a_real_socket`,
/// `rpc_unlinks_its_socket_on_shutdown`.
pub async fn serve(state: AppState, socket: &Path) -> Result<()> {
    // Deliberately OUTSIDE `serve_with_shutdown`: it resolves the real `$HOME`
    // and the real data directory, so a test that drove it would delete a
    // developer's own files. It is also a one-time upgrade concern rather than
    // part of serving.
    remove_retired_discovery_files();

    serve_with_shutdown(state, socket, trusty_common::shutdown_signal()).await
}

/// [`serve`]'s body, with the shutdown future supplied by the caller.
///
/// Why: `serve` waits on SIGTERM/SIGINT, which a test cannot deliver to its own
/// process without affecting the whole test binary. Taking the future as a
/// parameter is what lets `rpc_unlinks_its_socket_on_shutdown` drive the REAL
/// shutdown path rather than re-implementing the loop and deleting the file
/// itself — a test that would pass whether or not the unlink below exists.
///
/// What: binds, serves via [`serve_until`], flushes the BM25 lane, then removes
/// the socket file BEFORE dropping the listener — with the order reversed there
/// is a window in which nothing answers the path but the file is still there,
/// and a successor that rebinds in that window has its fresh socket deleted by
/// this process.
///
/// # Errors
///
/// As [`serve`].
///
/// Test: `rpc_unlinks_its_socket_on_shutdown`,
/// `rpc_serves_concurrent_connections`.
pub async fn serve_with_shutdown(
    state: AppState,
    socket: &Path,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    // #6286: no `SelfOrigins` / `with_guarded_middleware` here, and its absence
    // is deliberate. That machinery was browser-CSRF defence for the
    // destructive routes — palace and drawer deletion, `POST /api/v1/admin/stop`,
    // and `POST /rpc`, which is the whole mutating tool surface — on an HTTP
    // listener a page could reach. It has no meaning on a Unix socket, where
    // the trust boundary is the 0700 directory, the 0600 socket, and the
    // `ensure_peer_is_self` uid check `serve_until` runs on every accepted
    // connection before a byte is read.
    let listener = trusty_common::uds::bind_singleton_hardened(socket)
        .await
        .with_context(|| format!("bind trusty-memory socket at {}", socket.display()))?;

    // #5329: the lane coalesces writes on a timer, so the last interval's worth
    // of indexing is lost without an explicit flush on the exit path. Cloning
    // is an `Arc` bump and detaches the lane's lifetime from `state`.
    let bm25 = state.bm25.clone();

    let router = Arc::new(build_router(state));
    info!(
        socket = %socket.display(),
        folded = router.method_names().count(),
        streams = router.stream_names().count(),
        dispatcher = dispatcher_method_count(),
        "trusty-memory serving"
    );
    eprintln!("trusty-memory: serving on {}", socket.display());

    serve_until(&listener, Arc::clone(&router), serve_options(), shutdown).await;

    if let Some(lane) = bm25 {
        lane.shutdown().await;
    }

    if let Err(e) = std::fs::remove_file(socket) {
        tracing::debug!(socket = %socket.display(), error = %e, "socket already gone");
    }
    drop(listener);
    Ok(())
}

/// Delete the `http_addr` files the TCP daemon used to write (#6286).
///
/// Why: on every machine that ran trusty-memory before this change, two files
/// are still on disk with `127.0.0.1:7070` in them — the OS-standard one and
/// the `~/.trusty-memory` dotfile #498 added. Nothing rewrites them now, so
/// they are permanently stale, and a stale discovery file is not inert: it is
/// what `tctl`'s bootstrap guard reads to decide which port must be free, so it
/// would refuse an install because an unrelated process held a port this daemon
/// no longer binds.
///
/// What: best-effort removal at every start. Failures never block the daemon;
/// the common case is that the file is already gone.
///
/// Deliberately NOT called from any test: it resolves the real data directory
/// and the real `$HOME`. The removal itself is [`remove_if_present`], which is
/// tested against a temp path.
fn remove_retired_discovery_files() {
    if let Ok(dir) = trusty_common::resolve_data_dir("trusty-memory") {
        remove_if_present(&dir.join("http_addr"));
    }
    // #880, applied to the removal for the same reason it applied to the write:
    // the dotfile is at a fixed `$HOME` path that no data-dir override
    // redirects, so an isolated instance — a test rig, CI, a parallel run —
    // would delete the REAL daemon's file. The override is the signal that this
    // process must not touch shared state.
    if is_data_dir_override_active() {
        return;
    }
    if let Some(home) = dirs::home_dir() {
        remove_if_present(&home.join(".trusty-memory").join("http_addr"));
    }
}

/// Delete `path` if it is there, and say so; report anything else at debug.
///
/// An already-absent file is the expected case on a fresh install and is
/// silent — logging it would put a line in every start-up for a non-event.
///
/// Test: `remove_if_present_deletes_a_stale_file_and_tolerates_an_absent_one`.
fn remove_if_present(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => tracing::info!(
            path = %path.display(),
            "removed a retired http_addr discovery file (#6286)"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::debug!(
            path = %path.display(),
            error = %e,
            "could not remove a retired http_addr discovery file"
        ),
    }
}

#[cfg(test)]
#[path = "uds_tests.rs"]
mod tests;
