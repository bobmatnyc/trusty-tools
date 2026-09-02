//! The wire half of the service: twenty methods over a hardened Unix socket.
//!
//! Why (#6287, ADR-0032): trusty-analyze used to bind TCP loopback HTTP on
//! `127.0.0.1:7879`, publish the address in an `http_addr` discovery file, and
//! serve an embedded admin UI and an SSE stream beside the analysis API.
//! ADR-0032 makes UDS the inter-service transport and `trusty-console` the only
//! HTTP surface in the workspace; `trusty-review` went through this path first
//! (#6277) and this module is that migration applied to a much wider method
//! surface. The framing, the JSON-RPC envelope, the peer check and the accept
//! loop all come from [`trusty_common::uds::server`], so this module is only
//! the part that is trusty-analyze's: which methods exist, what they carry, and
//! how the socket is bound.
//!
//! What:
//! - [`METHODS`] — every method name, in one array, so a consumer contract test
//!   can assert the set rather than re-listing it.
//! - [`build_router`] — those methods mapped onto [`crate::service::handlers`].
//! - [`serve`] — bind, serve until SIGTERM/SIGINT, unlink.
//!
//! Test: `rpc_tests.rs` — `rpc_*` for the wire behaviour, over a real socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use tracing::info;
use trusty_common::uds::server::{RpcRouter, RpcServeOptions};

use crate::service::events::AnalyzerAppState;
use crate::service::handlers::{analysis, deep, facts, graph, review, system};

/// Liveness, dependency reachability and this daemon's version.
///
/// This is the method `trusty-console`'s `AnalyzeConnector` and `tctl`'s health
/// probe dial. Renaming it breaks both, in crates that have no Cargo edge on
/// this one.
pub const METHOD_HEALTH: &str = "analyze.health";

/// Every method this daemon serves, in registration order.
///
/// Why: four crates outside this one dial these names by literal — the console
/// connector, `tctl`'s probe, tga's audit client and trusty-audit's grounding
/// client — because none of them has (or wants) a Cargo edge on an analysis
/// daemon. An array here is what a contract test can compare against, so a
/// rename shows up as a failing assertion rather than as a consumer that
/// silently reports `method_not_found`.
/// What: the `<domain>.<verb>` convention `trusty-review`'s `review.*` set
/// established, one entry per [`build_router`] registration.
/// Test: `rpc_router_registers_every_documented_method`.
pub const METHODS: &[&str] = &[
    METHOD_HEALTH,
    "analyze.list_indexes",
    "analyze.complexity_hotspots",
    "analyze.complexity_distribution",
    "analyze.smells",
    "analyze.refactor_suggestions",
    "analyze.quality",
    "analyze.diagnostics",
    "analyze.graph",
    "analyze.entities",
    "analyze.clusters",
    "analyze.ner",
    "analyze.scip_ingest",
    "analyze.scip_status",
    "analyze.review",
    "analyze.review_github_pr",
    "analyze.deep_analysis",
    "analyze.facts_list",
    "analyze.facts_upsert",
    "analyze.facts_delete",
];

/// Largest REQUEST frame this service will read, in bytes.
///
/// Why: [`trusty_common::uds::MAX_FRAME_BYTES`] is 8 MiB, sized for
/// control-plane frames. Most of these twenty methods fit inside it with room
/// to spare — an index id and a handful of integers. Two do not, and both carry
/// caller-supplied bulk that nothing bounds before it arrives:
///
/// - `analyze.scip_ingest` carries a whole SCIP `Index` protobuf, base64-encoded
///   (see [`graph::ScipIngestRequest`]). Base64 costs 4 bytes per 3, so the
///   frame is roughly 1.34x the protobuf. A SCIP index for a large repository is
///   the biggest thing this daemon is ever handed.
/// - `analyze.review` carries a raw unified diff, which for a whole-branch diff
///   with generated files included runs to megabytes.
///
/// What: 32 MiB, four times the shared default, applied to the server's read —
/// the same figure `trusty-review`'s `rpc::MAX_FRAME_BYTES` uses, for the same
/// reason. The budget is per connection, not per method (the method name lives
/// inside the frame the budget governs the reading of), so it has to cover the
/// largest of the twenty.
///
/// **This figure is inherited, not measured.** It is `trusty-review`'s, adopted
/// because both services face the same shape of problem — caller-supplied bulk
/// that nothing bounds before the frame arrives. No SCIP index has been measured
/// against it: doing so needs a real language indexer (`scip-rust`,
/// `scip-typescript`) run over a large repository, which was out of scope here.
///
/// What is known without measuring: base64 costs 4 bytes per 3, so the frame is
/// ~1.34x the protobuf, and 32 MiB therefore admits a ~24 MB SCIP index. If a
/// real index turns out to exceed that, the symptom is a refused
/// `analyze.scip_ingest` naming the budget — a visible failure, not a silent
/// truncation — and raising it means raising the client's response budget
/// (`mcp::rpc_client::MAX_RESPONSE_FRAME_BYTES`) in the same change.
///
/// 32 MiB is still a bound, not an absence of one: the read is capped, and a
/// frame past the budget is refused rather than buffered.
/// Test: `rpc_refuses_a_request_past_its_own_budget`.
///
/// **Which end this figure binds, precisely.** Each end caps only what it READS,
/// and neither caps what it writes:
///
/// - This constant feeds `RpcServeOptions::max_frame_bytes`, which
///   `handle_connection` applies as `take(..)` on the REQUEST read. It is the
///   reason a large `analyze.scip_ingest` is answered instead of dropped.
/// - A client's `max_frame_bytes` — the fourth argument to
///   [`trusty_common::uds::send_framed_request_capped`] — bounds only its
///   RESPONSE read.
///
/// The MCP client sets the response budget to the same figure, because
/// `analyze.graph` on a large index returns a whole-repository `KgGraph` and is
/// the mirror image of the ingest that produced it. See
/// `mcp::http_client::MAX_RESPONSE_FRAME_BYTES`.
///
/// Test: `rpc_accepts_a_request_larger_than_the_shared_default`,
/// `rpc_refuses_a_request_past_its_own_budget`.
pub const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

/// The params of a method that takes no arguments.
///
/// Why: [`RpcRouter::typed`] decodes `params` into the handler's request type
/// before the handler runs, and `params` is absent — `serde_json::Value::Null` —
/// on a well-formed call to a no-argument method. A plain unit struct refuses
/// `null`, so every health probe would answer `invalid_params`.
/// What: accepts anything and keeps nothing. A caller that sends a stray field
/// is not refused: these methods have no arguments to get wrong, and refusing
/// would turn an additive client change into an outage.
/// Test: `rpc_health_answers_with_no_params`,
/// `rpc_health_answers_with_a_stray_params_object`.
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

/// Map every method onto its handler.
///
/// Why: the only trusty-analyze-specific half of the server. Everything else —
/// the peer-uid check, the framing, the envelope, the accept loop — is
/// [`trusty_common::uds::server`]'s.
/// What: each method is registered with [`RpcRouter::typed`], so the handler
/// sees its own request type and a decode failure becomes a coded
/// `invalid_params` frame rather than a dropped connection. Every handler's
/// `ApiError` converts through `From<ApiError> for RpcError`
/// (`service::events`), which is where the failure-kind-to-code mapping lives.
/// Test: `rpc_router_registers_every_documented_method`,
/// `rpc_reports_method_not_found_for_an_unknown_method`.
///
/// The `state.clone()` per method is one `AnalyzerAppState`, which is `Arc`s
/// and small `Copy` fields — not the stores themselves.
pub fn build_router(state: AnalyzerAppState) -> RpcRouter {
    macro_rules! bind {
        ($router:expr, $name:literal, $req:ty, $resp:ty, $handler:path) => {{
            let state = state.clone();
            $router.typed::<$req, $resp, _, _>($name, move |req| {
                let state = state.clone();
                async move { $handler(&state, req).await.map_err(Into::into) }
            })
        }};
    }

    let health_state = state.clone();
    let indexes_state = state.clone();

    let router = RpcRouter::new()
        .typed::<NoParams, system::HealthResponse, _, _>(METHOD_HEALTH, move |_params| {
            let state = health_state.clone();
            async move { Ok(system::health(&state).await) }
        })
        .typed::<NoParams, Vec<crate::core::IndexSummary>, _, _>(
            "analyze.list_indexes",
            move |_params| {
                let state = indexes_state.clone();
                async move { system::list_indexes(&state).await.map_err(Into::into) }
            },
        );

    let router = bind!(
        router,
        "analyze.complexity_hotspots",
        analysis::HotspotsRequest,
        serde_json::Value,
        analysis::complexity_hotspots
    );
    let router = bind!(
        router,
        "analyze.complexity_distribution",
        analysis::IndexRequest,
        serde_json::Value,
        analysis::complexity_distribution
    );
    let router = bind!(
        router,
        "analyze.smells",
        analysis::SmellsRequest,
        serde_json::Value,
        analysis::smells
    );
    let router = bind!(
        router,
        "analyze.refactor_suggestions",
        analysis::RefactorRequest,
        serde_json::Value,
        analysis::refactor_suggestions
    );
    let router = bind!(
        router,
        "analyze.quality",
        analysis::IndexRequest,
        crate::core::quality::QualityReport,
        analysis::quality_report
    );
    let router = bind!(
        router,
        "analyze.diagnostics",
        analysis::DiagnosticsRequest,
        serde_json::Value,
        analysis::diagnostics_for_index
    );
    let router = bind!(
        router,
        "analyze.graph",
        graph::GraphRequest,
        graph::GraphResponse,
        graph::graph_for_index
    );
    let router = bind!(
        router,
        "analyze.entities",
        graph::EntitiesRequest,
        Vec<crate::types::KgNode>,
        graph::entities_for_index
    );
    let router = bind!(
        router,
        "analyze.clusters",
        graph::ClustersRequest,
        graph::ClusterResponse,
        graph::clusters_for_index
    );
    let router = bind!(
        router,
        "analyze.ner",
        graph::NerRequest,
        Vec<crate::types::RawEntity>,
        graph::ner_for_index
    );
    let router = bind!(
        router,
        "analyze.scip_ingest",
        graph::ScipIngestRequest,
        graph::ScipIngestResponse,
        graph::ingest_scip
    );
    let router = bind!(
        router,
        "analyze.scip_status",
        analysis::IndexRequest,
        graph::ScipOverlayStatus,
        graph::scip_overlay_status
    );
    let router = bind!(
        router,
        "analyze.review",
        review::ReviewRequest,
        crate::core::ReviewReport,
        review::review_diff_handler
    );
    let router = bind!(
        router,
        "analyze.review_github_pr",
        crate::core::GithubPrRequest,
        crate::core::ReviewReport,
        review::review_github_pr_handler
    );
    let router = bind!(
        router,
        "analyze.deep_analysis",
        deep::DeepAnalyzeRequest,
        crate::core::DeepAnalysisReport,
        deep::deep_analyze_handler
    );
    // `analyze.facts_list` is the one method whose every field is optional, so
    // it is the one method a caller can legitimately invoke with no `params` at
    // all — which arrives as `null`. A struct refuses `null` (that is why
    // [`NoParams`] exists), and `Option<T>` is what accepts both it and a real
    // filter object. The other nineteen methods all require `index_id`, so
    // `null` is correctly a decode failure there.
    let facts_state = state.clone();
    let router = router.typed::<Option<facts::FactQueryRequest>, serde_json::Value, _, _>(
        "analyze.facts_list",
        move |req| {
            let state = facts_state.clone();
            async move {
                facts::list_facts(&state, req.unwrap_or_default())
                    .await
                    .map_err(Into::into)
            }
        },
    );
    let router = bind!(
        router,
        "analyze.facts_upsert",
        facts::UpsertFactRequest,
        serde_json::Value,
        facts::upsert_fact
    );
    bind!(
        router,
        "analyze.facts_delete",
        facts::DeleteFactRequest,
        serde_json::Value,
        facts::delete_fact
    )
}

/// Per-connection budgets for this service.
///
/// The read timeout is the shared default; only the frame budget moves. See
/// [`MAX_FRAME_BYTES`] for why. The read bound does NOT cover a handler, which
/// is what makes a multi-minute `analyze.diagnostics` compatible with a
/// 30-second guard against a peer that connects and never writes.
fn serve_options() -> RpcServeOptions {
    RpcServeOptions {
        max_frame_bytes: MAX_FRAME_BYTES,
        ..RpcServeOptions::default()
    }
}

/// Bind `socket` and serve until SIGTERM/SIGINT, then unlink it.
///
/// Why the bind is [`trusty_common::uds::bind_singleton_hardened`] and not
/// [`trusty_common::uds::server::RpcServer::run`]: this daemon is supervised by
/// launchd with `KeepAlive::Always` (`commands::service::launchd_config`). A
/// predecessor that is SIGKILLed — which is what `launchctl kickstart -k` does
/// at the `ExitTimeOut` boundary — never reaches the unlink below and leaves its
/// socket file behind. `RpcServer::run` binds through `bind_hardened`, which
/// refuses an occupied path rather than clobbering what might be a live owner,
/// so the replacement launchd starts would fail its bind, exit, be restarted,
/// and fail again — a crash loop with no operator-visible cause, the same shape
/// as the #2566 port collision. `bind_singleton_hardened` probes first and takes
/// over only a socket the kernel proves nobody is serving, so a live daemon is
/// still never clobbered.
///
/// What: removes the retired discovery files, then delegates to
/// [`serve_with_shutdown`] with the real signal future.
///
/// No discovery file is written. The path is derived, not published:
/// `trusty_common::daemon_socket_path("trusty-analyze")` is what the daemon
/// binds and what every consumer dials, so there is nothing for a stale file to
/// disagree with.
///
/// # Errors
///
/// When the socket cannot be bound — including
/// `UdsSecurityError::AlreadyServing`, which means another trusty-analyze is
/// live on this path and this process must not start.
///
/// Test: `rpc_health_answers_over_a_real_socket`,
/// `rpc_unlinks_its_socket_on_shutdown`.
/// This server's own shutdown budget, as the supervisor contract requires.
///
/// Why it is declared here and not only in `trusty-common`: `ServiceTimeouts`'
/// sourcing rule says the supervisor's `shutdown_flush` must be the supervised
/// binary's REAL budget, and `trusty-common` cannot import this crate to read
/// it. So this is the definition, and
/// `analyze_flush_budget_matches_the_supervisor_contract` pins
/// `trusty_common::uds::ANALYZE_SHUTDOWN_FLUSH` against it — a drift is a test
/// failure rather than a SIGKILL landing mid-shutdown.
///
/// One second is honest for this server: every handler commits its redb write
/// before answering, so a SIGTERM discards nothing that was acked. The budget
/// covers the accept loop returning and the socket unlink below.
pub const SHUTDOWN_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

pub async fn serve(state: AnalyzerAppState, socket: &Path) -> Result<()> {
    // Migration cleanup, deliberately OUTSIDE `serve_with_shutdown`: it resolves
    // the real `$HOME` and the real data directory, so a test that drove it
    // would delete a developer's own files. It also belongs to the process
    // entry point rather than the serve loop — a one-time upgrade concern, not
    // part of serving.
    remove_retired_discovery_files();

    serve_with_shutdown(state, socket, trusty_common::shutdown_signal()).await
}

/// [`serve`], with the idle window resolved from the environment (#6350).
///
/// Why this is the process entry point now: ADR-0032 retires the launchd unit,
/// so nothing restarts this server and nothing stops it either — it has to end
/// itself, or the first request of the day leaves a process resident until the
/// machine reboots. That is the resident daemon this change removed, minus the
/// supervisor that would at least have restarted it after a crash.
///
/// What: reads [`trusty_common::uds::ANALYZE_IDLE_TIMEOUT_ENV`] through the
/// shared parser, so `0` means "never exit" and an unset variable means the
/// ten-minute default. The window is announced on stderr at startup, because an
/// operator watching a foreground server needs to know it will end and when.
///
/// # Errors
///
/// As [`serve`].
///
/// Test: `tests/on_demand.rs`' `serve_exits_on_its_own_idle_window` drives the
/// real binary to an idle exit; the policy itself is covered by
/// `trusty-common`'s `idle_timeout_parses_its_three_meanings`.
pub async fn serve_on_demand(state: AnalyzerAppState, socket: &Path) -> Result<()> {
    remove_retired_discovery_files();

    let idle = trusty_common::uds::analyze_idle_timeout_from_env();
    serve_with_idle(state, socket, trusty_common::shutdown_signal(), idle).await
}

/// [`serve`]'s body, with the shutdown future supplied by the caller.
///
/// Why: `serve` waits on SIGTERM/SIGINT, which a test cannot deliver to its own
/// process without affecting the whole test binary. Taking the future as a
/// parameter is what lets `rpc_unlinks_its_socket_on_shutdown` drive the REAL
/// shutdown path and assert the socket file is gone — rather than
/// re-implementing the loop and deleting the file itself, which is a test that
/// passes whether or not the unlink below exists.
///
/// What: binds through `bind_singleton_hardened`, serves via
/// [`trusty_common::uds::server::serve_until_idle`],
/// then removes the socket file BEFORE dropping the listener — the order
/// `webhook_relay::listener` records, because reversed there is a window in
/// which nothing answers the path but the file is still there, and a successor
/// that rebinds in that window has its fresh socket deleted by this process.
///
/// # Errors
///
/// As [`serve`].
///
/// Test: `rpc_unlinks_its_socket_on_shutdown`,
/// `rpc_accepts_a_request_larger_than_the_shared_default`.
pub async fn serve_with_shutdown(
    state: AnalyzerAppState,
    socket: &Path,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    serve_with_idle(state, socket, shutdown, None).await
}

/// [`serve_with_shutdown`], plus the idle window (#6350).
///
/// Why both signatures exist: `serve_with_shutdown` is what a caller that owns
/// the process lifetime wants — the `--mcp` path runs this server inside a
/// process whose real job is an MCP stdio loop, and an idle exit there would
/// silently drop the transport that loop dials. `None` is therefore a real
/// choice, not a legacy default.
///
/// What: binds through `bind_singleton_hardened`, serves via
/// [`trusty_common::uds::server::serve_until_idle`], then removes the socket
/// file BEFORE dropping the listener — the order `webhook_relay::listener`
/// records, because reversed there is a window in which nothing answers the path
/// but the file is still there, and a successor that rebinds in that window has
/// its fresh socket deleted by this process.
///
/// The unlink runs on BOTH exits. An idle exit that left the file behind would
/// make the next `ensure_running` spawn a child that fails its bind, and
/// `bind_singleton_hardened`'s takeover is a recovery path, not a licence to
/// leave corpses.
///
/// The router is dropped BEFORE the unlink (#6595). The unlink is what tells a
/// client this server is gone, and the client's answer to that is to spawn a
/// successor, whose first act is to open the same two redb files. redb takes an
/// exclusive lock per file, so releasing those locks after the unlink hands the
/// successor a `Database already open. Cannot acquire lock.` — measured at
/// 54–560 ms of exposure on an idle machine, and `Supervisor::ensure_running`
/// does not notice the child died, so it polls the socket for the full 20 s
/// `spawn_probe` budget and reports a spawn timeout. Dropping the router drops
/// every `AnalyzerAppState` clone the handlers hold, and with them the
/// `FactStore` and `ScipOverlayStore` handles, so the locks are free before the
/// path a successor keys off disappears.
///
/// # Errors
///
/// As [`serve`].
///
/// Test: `rpc_unlinks_its_socket_on_shutdown`,
/// `rpc_accepts_a_request_larger_than_the_shared_default`,
/// `tests/on_demand.rs`' `serve_exits_on_its_own_idle_window` and
/// `an_idle_exit_frees_its_redb_locks_before_it_unlinks_the_socket`.
pub async fn serve_with_idle(
    state: AnalyzerAppState,
    socket: &Path,
    shutdown: impl std::future::Future<Output = ()> + Send,
    idle: Option<std::time::Duration>,
) -> Result<()> {
    // #6287: no `SelfOrigins` / `with_guarded_middleware` here, and its absence
    // is deliberate rather than an oversight. That machinery was browser-CSRF
    // defence for the destructive routes (`POST /indexes/{id}/scip`,
    // `POST /facts`, `DELETE /facts/{id}`) on an HTTP surface a page could
    // reach; it has no meaning on a Unix socket, where the trust boundary is
    // the 0700 directory, the 0600 socket, and the `ensure_peer_is_self` uid
    // check `serve_until` runs on every accepted connection before a byte is
    // read.
    let listener = trusty_common::uds::bind_singleton_hardened(socket)
        .await
        .with_context(|| format!("bind trusty-analyze socket at {}", socket.display()))?;

    let router = Arc::new(build_router(state));
    info!(socket = %socket.display(), methods = router.method_names().count(), idle = ?idle, "trusty-analyze serving");
    match idle {
        Some(window) => eprintln!(
            "trusty-analyze: serving on {} (exits after {}s idle)",
            socket.display(),
            window.as_secs()
        ),
        None => eprintln!("trusty-analyze: serving on {}", socket.display()),
    }

    // #6350: the tracker is what makes the exit conditional on real traffic
    // rather than on a bare timer — see `IdleTracker`.
    let tracker = idle.map(trusty_common::uds::server::IdleTracker::new);
    let exit = trusty_common::uds::server::serve_until_idle(
        &listener,
        Arc::clone(&router),
        serve_options(),
        shutdown,
        tracker,
    )
    .await;
    if exit == trusty_common::uds::server::ServeExit::Idle {
        info!(socket = %socket.display(), "trusty-analyze idle; exiting");
        eprintln!("trusty-analyze: idle; exiting");
    }

    // #6595: close the redb stores before the unlink advertises this server as
    // gone, so a successor never opens facts.redb against a lock this process
    // still holds.
    match Arc::into_inner(router) {
        Some(router) => drop(router),
        None => tracing::warn!(
            socket = %socket.display(),
            "a connection task still holds the router at exit; \
             the redb locks may outlive the socket"
        ),
    }

    if let Err(e) = std::fs::remove_file(socket) {
        tracing::debug!(socket = %socket.display(), error = %e, "socket already gone");
    }
    drop(listener);
    Ok(())
}

/// Delete the `http_addr` files the TCP daemon used to write (#6287).
///
/// Why: on every machine that ran trusty-analyze before this change, a file is
/// still on disk with `127.0.0.1:7879` in it. Nothing rewrites it now, so it is
/// permanently stale, and a stale discovery file is not inert: `tctl`'s
/// bootstrap guard reads `read_daemon_addr` to decide which port must be free,
/// and would refuse an install because an unrelated process holds a port this
/// daemon no longer binds. `tctl` also refuses to consult it for a UDS member,
/// so this is the second half of a belt-and-braces pair — the file should not
/// exist, and reading it should not matter either.
///
/// What: best-effort removal at every start, through [`remove_if_present`].
/// Failures never block the daemon; the common case is that the file is already
/// gone.
///
/// Deliberately NOT called from any test: it resolves the real data directory,
/// so a test that ran it would delete a developer's own files. The removal
/// itself is [`remove_if_present`], which is tested against a temp path.
fn remove_retired_discovery_files() {
    if let Ok(dir) = trusty_common::resolve_data_dir("trusty-analyze") {
        remove_if_present(&dir.join("http_addr"));
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
            "removed a retired http_addr discovery file (#6287)"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::debug!(
            path = %path.display(),
            error = %e,
            "could not remove a retired http_addr discovery file"
        ),
    }
}

/// The socket this daemon binds, as both it and its consumers resolve it.
///
/// A thin re-export of the shared entry point so a reader of this module does
/// not have to know that the layout lives in `trusty_common::daemon_addr`.
///
/// # Errors
///
/// When the data directory cannot be resolved or created.
pub fn socket_path() -> Result<PathBuf> {
    trusty_common::daemon_socket_path("trusty-analyze")
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
