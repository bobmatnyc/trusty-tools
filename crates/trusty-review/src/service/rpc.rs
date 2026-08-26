//! The wire half of the service: three methods over a hardened Unix socket.
//!
//! Why (#6277, ADR-0032): trusty-review used to bind TCP loopback HTTP on
//! `127.0.0.1:7891` and publish the address in an `http_addr` discovery file.
//! ADR-0032 makes UDS the inter-service transport and `trusty-console` the only
//! HTTP surface in the workspace, and this crate is the first daemon through
//! that path. The framing, the JSON-RPC envelope, the peer check and the accept
//! loop all come from [`trusty_common::uds::server`], so this module is only the
//! part that is trusty-review's: which methods exist, what they carry, and how
//! the socket is bound.
//!
//! What:
//! - [`METHOD_HEALTH`] / [`METHOD_STATUS`] / [`METHOD_RUN`] — the method names,
//!   on the `<domain>.<verb>` convention `webhook_relay::RELAY_METHOD` set.
//! - [`build_router`] — the three methods mapped onto
//!   [`crate::service::handlers`], each over its own request and response type.
//! - [`serve`] — bind, serve until SIGTERM/SIGINT, unlink.
//!
//! Test: `tests` below — `rpc_*` for the wire behaviour, over a real socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use tracing::info;
use trusty_common::uds::server::{RpcRouter, RpcServeOptions, serve_until};

use crate::service::handlers::{
    AppState, HealthResponse, ReviewRequest, StatusResponse, handle_health, handle_review,
    handle_status,
};

/// Liveness, dependency reachability and the inference probe.
///
/// This is the method `trusty-console`'s `ReviewConnector` and `tctl`'s health
/// probe dial. Renaming it breaks both, in two crates that have no Cargo edge
/// on this one.
pub const METHOD_HEALTH: &str = "review.health";

/// In-flight review count and last pipeline error.
pub const METHOD_STATUS: &str = "review.status";

/// One synchronous review — what `POST /review` was before #6277.
///
/// Named `run` rather than `review` so the method does not read `review.review`;
/// it is the same verb the CLI spells `trusty-review run`.
pub const METHOD_RUN: &str = "review.run";

/// Largest REQUEST frame this service will read, in bytes.
///
/// Why: [`trusty_common::uds::MAX_FRAME_BYTES`] is 8 MiB, sized for
/// control-plane frames. Two of these three methods fit inside it with room to
/// spare — a `HealthResponse` is a few hundred bytes and a `StatusResponse` is
/// smaller. [`METHOD_RUN`] is the one that does not: it carries
/// `local_diff_text`, a caller-supplied raw unified diff that nothing bounds
/// before it arrives — `pipeline::diff::truncate_diff` cuts it to
/// `MAX_DIFF_CHARS` (160 000) only AFTER the frame has been read. A caller
/// handing over a whole monorepo diff, generated files included, can exceed
/// 8 MiB, and under the default budget that arrives as a dropped connection
/// rather than as an answer.
///
/// What: 32 MiB, four times the shared default, applied to the server's read.
/// The budget is per connection, not per method (the method name lives inside
/// the frame the budget governs the reading of), so it has to cover the largest
/// of the three.
///
/// **Which end this figure binds, precisely.** Each end caps only what it
/// READS, and neither caps what it writes:
///
/// - This constant feeds `RpcServeOptions::max_frame_bytes`, which
///   `handle_connection` applies as `take(..)` on the REQUEST read. It is the
///   reason a large `review.run` is answered instead of dropped.
/// - A client's `max_frame_bytes` — the fourth argument to
///   [`trusty_common::uds::send_framed_request_capped`] — bounds only its
///   RESPONSE read. `dial_and_send` writes the request with a plain
///   `write_all`, uncapped.
///
/// So a `review.run` client does NOT need the capped variant to SEND a large
/// diff; plain [`trusty_common::uds::send_framed_request`] writes it and this
/// server accepts it. It would need the capped variant only if a `ReviewResult`
/// could exceed the shared 8 MiB response default — which it does not: the
/// result is bounded by what the reviewer model emits, which the provider caps
/// in output tokens, and a large real report measures in tens of kilobytes.
///
/// Test: `rpc_accepts_a_review_request_larger_than_the_shared_default`.
pub const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

/// The params of a method that takes no arguments.
///
/// Why: [`RpcRouter::typed`] decodes `params` into the handler's request type
/// before the handler runs, and `params` is absent — `serde_json::Value::Null` —
/// on a well-formed call to a no-argument method. A plain unit struct refuses
/// `null`, so every health probe would answer `invalid_params`.
/// What: accepts anything and keeps nothing. A caller that sends a stray field
/// is not refused: this method has no arguments to get wrong, and refusing
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

/// Map the three methods onto their handlers.
///
/// Why: the only trusty-review-specific half of the server. Everything else —
/// the peer-uid check, the framing, the envelope, the accept loop — is
/// [`trusty_common::uds::server`]'s.
/// What: each method is registered with [`RpcRouter::typed`], so the handler
/// sees its own request type and the decode failure becomes a coded
/// `invalid_params` frame rather than a dropped connection.
/// Test: `rpc_health_answers_with_no_params`,
/// `rpc_run_reports_invalid_params_for_a_request_naming_no_diff`,
/// `rpc_reports_method_not_found_for_an_unknown_method`.
pub fn build_router(state: AppState) -> RpcRouter {
    let health_state = state.clone();
    let status_state = state.clone();
    let run_state = state;

    RpcRouter::new()
        .typed::<NoParams, HealthResponse, _, _>(METHOD_HEALTH, move |_params| {
            let state = health_state.clone();
            async move { Ok(handle_health(&state).await) }
        })
        .typed::<NoParams, StatusResponse, _, _>(METHOD_STATUS, move |_params| {
            let state = status_state.clone();
            async move { Ok(handle_status(&state).await) }
        })
        .typed::<ReviewRequest, crate::models::ReviewResult, _, _>(METHOD_RUN, move |req| {
            let state = run_state.clone();
            async move { handle_review(&state, req).await }
        })
}

/// Per-connection budgets for this service.
///
/// The read timeout is the shared default; only the frame budget moves. See
/// [`MAX_FRAME_BYTES`] for why. The read bound does NOT cover a handler, which
/// is what makes a multi-minute `review.run` compatible with a 30-second guard
/// against a peer that connects and never writes.
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
/// launchd with `KeepAlive::Always` and a 10-second throttle
/// (`commands::service::launchd_config`). A predecessor that is SIGKILLed —
/// which is exactly what `launchctl kickstart -k` does at the `ExitTimeOut`
/// boundary — never reaches the unlink below and leaves its socket file behind.
/// `RpcServer::run` binds through `bind_hardened`, which refuses an occupied
/// path rather than clobbering what might be a live owner, so the replacement
/// launchd starts would fail its bind, exit, be restarted ten seconds later, and
/// fail again — a crash loop with no operator-visible cause, the same shape as
/// the #2566 port collision. `bind_singleton_hardened` probes first and takes
/// over only a socket the kernel proves nobody is serving, so a live daemon is
/// still never clobbered.
///
/// What: binds, logs the path to stderr (stdout stays clean for MCP framing),
/// serves through [`serve_until`], then removes the socket file BEFORE dropping
/// the listener — the order `webhook_relay::listener` records, because reversed
/// there is a window in which nothing answers the path but the file is still
/// there, and a successor that rebinds in that window has its fresh socket
/// deleted by this process.
///
/// No discovery file is written. The path is derived, not published:
/// `trusty_common::daemon_socket_path("trusty-review")` is what the daemon binds
/// and what every consumer dials, so there is nothing for a stale file to
/// disagree with.
///
/// # Errors
///
/// When the socket cannot be bound — including
/// `UdsSecurityError::AlreadyServing`, which means another trusty-review is live
/// on this path and this process must not start.
///
/// Test: `rpc_health_answers_over_a_real_socket`,
/// `rpc_unlinks_its_socket_on_shutdown`.
pub async fn serve(state: AppState, socket: &Path) -> Result<()> {
    // Migration cleanup, deliberately OUTSIDE `serve_with_shutdown`: it resolves
    // the real `$HOME` and the real data directory, so a test that drove it
    // would delete a developer's own files. It also belongs to the process
    // entry point rather than the serve loop — it is a one-time upgrade concern,
    // not part of serving.
    remove_retired_discovery_files();

    serve_with_shutdown(state, socket, trusty_common::shutdown_signal()).await
}

/// [`serve`]'s body, with the shutdown future supplied by the caller.
///
/// Why: `serve` waits on SIGTERM/SIGINT, which a test cannot deliver to its own
/// process without affecting the whole test binary. Taking the future as a
/// parameter is what lets `rpc_unlinks_its_socket_on_shutdown` drive the REAL
/// shutdown path and assert the socket file is gone — rather than re-implementing
/// the loop and deleting the file itself, which is a test that passes whether or
/// not the unlink below exists.
///
/// What: binds through `bind_singleton_hardened`, serves via [`serve_until`],
/// then removes the socket file BEFORE dropping the listener — the order
/// `webhook_relay::listener` records, because reversed there is a window in which
/// nothing answers the path but the file is still there, and a successor that
/// rebinds in that window has its fresh socket deleted by this process.
///
/// # Errors
///
/// As [`serve`].
///
/// Test: `rpc_unlinks_its_socket_on_shutdown`,
/// `rpc_accepts_a_review_request_larger_than_the_shared_default`.
pub async fn serve_with_shutdown(
    state: AppState,
    socket: &Path,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    // #6277: no `SelfOrigins` / `with_guarded_middleware` here, and its absence
    // is deliberate rather than an oversight. That machinery is browser-CSRF
    // defence for an HTTP surface reachable from a page; it has no meaning on a
    // Unix socket, where the trust boundary is the 0700 directory, the 0600
    // socket, and the `ensure_peer_is_self` uid check `serve_until` runs on
    // every accepted connection before a byte is read.
    let listener = trusty_common::uds::bind_singleton_hardened(socket)
        .await
        .with_context(|| format!("bind trusty-review socket at {}", socket.display()))?;

    let router = Arc::new(build_router(state));
    info!(socket = %socket.display(), methods = ?router.method_names().collect::<Vec<_>>(), "trusty-review serving");
    eprintln!("trusty-review: serving on {}", socket.display());

    serve_until(&listener, Arc::clone(&router), serve_options(), shutdown).await;

    if let Err(e) = std::fs::remove_file(socket) {
        tracing::debug!(socket = %socket.display(), error = %e, "socket already gone");
    }
    drop(listener);
    Ok(())
}

/// Delete the `http_addr` files the TCP daemon used to write (#6277).
///
/// Why: on every machine that ran trusty-review before this change, two files
/// are still on disk with `127.0.0.1:7891` in them — the OS-standard one under
/// the data directory and the `~/.trusty-review/http_addr` dotfile. Nothing
/// rewrites them now, so they are permanently stale, and a stale discovery file
/// is not inert: `tctl`'s bootstrap guard reads `read_daemon_addr` to decide
/// which port must be free, and would refuse an install because an unrelated
/// process holds a port this daemon no longer binds. `tctl` also refuses to
/// consult it for a UDS member, so this is the second half of a belt-and-braces
/// pair — the file should not exist, and reading it should not matter either.
///
/// What: best-effort removal of both, at every start, through
/// [`remove_if_present`]. Failures never block the daemon; the common case is
/// that the files are already gone.
///
/// Deliberately NOT called from any test: it resolves the real `$HOME` and the
/// real data directory, so a test that ran it would delete a developer's own
/// files. The removal itself is [`remove_if_present`], which is tested against
/// a temp path.
fn remove_retired_discovery_files() {
    if let Ok(dir) = trusty_common::resolve_data_dir("trusty-review") {
        remove_if_present(&dir.join("http_addr"));
    }
    if let Some(home) = dirs::home_dir() {
        remove_if_present(&home.join(".trusty-review").join("http_addr"));
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
            "removed a retired http_addr discovery file (#6277)"
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
    trusty_common::daemon_socket_path("trusty-review")
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
