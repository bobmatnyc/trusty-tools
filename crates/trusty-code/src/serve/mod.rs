//! `tcode serve` daemon: STDIO + HTTP JSON-RPC transports + proof-of-life
//! methods (#2053, M1 control-plane cut line).
//!
//! Why: this module is the foundation the `tcode serve` binary subcommand
//! delegates to. It owns assembling the [`crate::jsonrpc::Router`] and
//! entering whichever transport loop was requested; `main.rs` stays a thin
//! clap-to-handler shim, matching the rest of the crate's CLI wiring.
//! What: [`build_router`] registers every currently-known method (today:
//! [`methods::register`]'s `ping`/`health` proof-of-life pair — later
//! tickets add `session.*` #2054, `task.*` #2056, `harness.describe` #2066
//! here — one line in `build_router` plus each group's own `register`).
//! [`run_stdio`] and [`run_http`] both build a router via `build_router` and
//! hand it to their respective transport ([`transport::run_stdio_loop`] /
//! [`http::run_http`]) — `ping`/`health` behave identically over either
//! transport because both dispatch through the same `Router`.
//!
//! What's NOT here yet: the transports are independent modes (`--stdio` xor
//! `--http`), not simultaneous; running both at once would need a shared
//! `Arc<Router>` across two concurrently-spawned tasks, which no current
//! ticket requires.
//!
//! Test: `serve::tests::*`.

pub mod http;
pub mod methods;
pub mod transport;

use std::path::PathBuf;

use anyhow::Result;
use tracing::info;

use crate::jsonrpc::Router;

/// Default TCP port for `tcode serve --http` when `--port` is omitted.
///
/// Why: the trusty-* family reserves a block of fixed local ports so
/// operators/tooling can find a daemon without a discovery file
/// (`trusty-memory` 7070, `trusty-search` 7878, `trusty-analyze` 7879,
/// `trusty-review` 7880). `7881` is the next free port in that sequence.
/// Pass `--port 0` to bind an OS-assigned ephemeral port instead (e.g. for
/// tests or running multiple instances side by side); the real bound port
/// is always logged to stderr regardless of which is used.
/// What: `7881`.
/// Test: `default_http_port_is_documented_value`.
pub const DEFAULT_HTTP_PORT: u16 = 7881;

/// Assemble the router with every method this build of `tcode` knows about.
///
/// Why: single place that lists which method groups are wired in, so a new
/// ticket adding `session.*`/`task.*`/`harness.describe` touches exactly one
/// line here plus its own `register` function. Both transports
/// (`run_stdio`, `run_http`) call this so they can never drift into
/// different method surfaces.
/// What: builds an empty `Router` and calls `methods::register` on it.
/// Test: `run_stdio_router_recognises_proof_of_life_methods`.
pub fn build_router() -> Router {
    let mut router = Router::new();
    methods::register(&mut router);
    router
}

/// Run `tcode serve --stdio` to completion.
///
/// Why: the top-level entry point `main.rs` calls for the `serve --stdio`
/// subcommand.
/// What: builds the router, logs start/stop to stderr, and enters
/// [`transport::run_stdio_loop`], returning when it returns (stdin EOF or
/// SIGTERM/SIGINT).
/// Test: `run_stdio_router_recognises_proof_of_life_methods` covers the
/// router assembly; the transport loop itself is covered by
/// `transport::tests`.
pub async fn run_stdio(project: PathBuf) -> Result<()> {
    info!(project = %project.display(), "tcode serve --stdio: starting");
    let router = build_router();
    transport::run_stdio_loop(router).await?;
    info!("tcode serve --stdio: stopped");
    Ok(())
}

/// Run `tcode serve --http` to completion.
///
/// Why: the top-level entry point `main.rs` calls for the `serve --http`
/// subcommand.
/// What: builds the router, logs start/stop to stderr, and enters
/// [`http::run_http`] bound to `port` (`0` = OS-assigned ephemeral port),
/// returning when it returns (SIGTERM/SIGINT).
/// Test: `run_stdio_router_recognises_proof_of_life_methods` covers the
/// shared router assembly; `http::tests` cover the routing/dispatch logic.
pub async fn run_http(project: PathBuf, port: u16) -> Result<()> {
    info!(project = %project.display(), port, "tcode serve --http: starting");
    let router = build_router();
    http::run_http(router, port).await?;
    info!("tcode serve --http: stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trusty_common::mcp::Request;

    /// `build_router` must recognise both proof-of-life methods.
    #[tokio::test]
    async fn run_stdio_router_recognises_proof_of_life_methods() {
        let router = build_router();
        for method in ["ping", "health"] {
            let req = Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: method.to_string(),
                params: None,
            };
            let resp = router.dispatch(req).await;
            assert!(
                resp.error.is_none(),
                "{method} must be registered, got {:?}",
                resp.error
            );
        }
    }

    /// `DEFAULT_HTTP_PORT` must stay the documented value (7881) so a
    /// change is a deliberate edit to both the constant and its doc comment,
    /// not an accidental drift.
    #[test]
    fn default_http_port_is_documented_value() {
        assert_eq!(DEFAULT_HTTP_PORT, 7881);
    }
}
