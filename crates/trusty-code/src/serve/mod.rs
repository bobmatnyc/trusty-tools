//! `tcode serve` daemon: STDIO JSON-RPC transport + proof-of-life methods
//! (#2053, M1 control-plane cut line).
//!
//! Why: this module is the foundation the `tcode serve` binary subcommand
//! delegates to. It owns assembling the [`crate::jsonrpc::Router`] and
//! entering the transport loop; `main.rs` stays a thin clap-to-handler shim,
//! matching the rest of the crate's CLI wiring.
//! What: [`build_router`] registers every currently-known method (today:
//! [`methods::register`]'s `ping`/`health` proof-of-life pair — later
//! tickets add `session.*` #2054, `task.*` #2056, `harness.describe` #2066
//! here). [`run_stdio`] builds the router and runs it against
//! [`transport::run_stdio_loop`] until stdin EOF or SIGTERM.
//!
//! Deferred: the parent issue (#2053) also lists an HTTP `POST /rpc`
//! transport; this ticket implements STDIO only. A future ticket adds an
//! HTTP listener that shares the same [`crate::jsonrpc::Router`].
//!
//! Test: `serve::tests::run_stdio_router_recognises_proof_of_life_methods`.

pub mod methods;
pub mod transport;

use std::path::PathBuf;

use anyhow::Result;
use tracing::info;

use crate::jsonrpc::Router;

/// Assemble the router with every method this build of `tcode` knows about.
///
/// Why: single place that lists which method groups are wired in, so a new
/// ticket adding `session.*`/`task.*`/`harness.describe` touches exactly one
/// line here plus its own `register` function.
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
/// SIGTERM).
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
}
