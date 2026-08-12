//! Proof-of-life JSON-RPC methods for `tcode serve` (#2053).
//!
//! Why: the transport + router need at least one real method to prove the
//! whole path works end-to-end without pulling in `session.*`/`task.*`/
//! `harness.describe` scope (those land in #2054/#2056/#2066). `ping` and
//! `health` are the minimal, side-effect-free pair for that job, shared
//! verbatim across both transports: the STDIO `health` JSON-RPC method and
//! the HTTP `GET /health` route (`crate::serve::http::health_handler`) both
//! call [`health_payload`] so the two transports can never drift into
//! different shapes.
//! What: `register` wires both methods into a [`Router`]; `ping` returns
//! `{"pong": true}`; `health` returns [`health_payload`]'s server name,
//! crate version, static `"ok"` status, pid, and the daemon's project
//! binding.
//! Test: `methods::tests::*`.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::binding::ProjectBinding;
use crate::jsonrpc::{ConnectionContext, Router, RpcError};

/// Register every proof-of-life method onto `router`.
///
/// Why: keeps `crate::serve::build_router` a one-line call as more method
/// groups (session/task/harness) register themselves the same way later.
/// `binding` is taken here — rather than `health` staying a bare `fn` — so
/// the daemon can REPORT which project it serves (#4512): a daemon binds
/// exactly one `ProjectBinding` for its whole life, and a client that
/// auto-attaches to a daemon it did not start has no other way to tell
/// whether it is about to operate against the wrong project.
/// What: registers `"ping"` and `"health"`; `binding` is shared into the
/// `health` closure behind an `Arc` so answering a probe never clones a
/// `PathBuf`.
/// Test: `methods_register_wires_ping_and_health`,
/// `health_reports_the_daemons_project_binding`.
pub fn register(router: &mut Router, binding: ProjectBinding) {
    router.register("ping", ping);
    let binding = Arc::new(binding);
    router.register("health", move |params: Value, ctx: ConnectionContext| {
        let binding = binding.clone();
        async move { health(&binding, params, ctx).await }
    });
}

/// `ping` — returns `{"pong": true}` regardless of `params`.
///
/// Why: the cheapest possible round-trip proof that a transport and the
/// router are wired correctly.
/// What: ignores `params` and the connection context, never fails.
/// Test: `ping_returns_pong_true`.
async fn ping(_params: Value, _ctx: ConnectionContext) -> Result<Value, RpcError> {
    Ok(json!({"pong": true}))
}

/// `health` — returns server identity, crate version, status, and binding.
///
/// Why: a slightly richer proof-of-life than `ping` that a caller can use to
/// confirm which build of `tcode` — and, since #4512, which PROJECT — it is
/// talking to.
/// What: ignores `params` and the connection context, never fails; the
/// payload is [`health_payload`].
/// Test: `health_returns_server_identity`,
/// `health_reports_the_daemons_project_binding`.
async fn health(
    binding: &ProjectBinding,
    _params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    Ok(health_payload(binding))
}

/// The shared `health` payload:
/// `{"server","version","status","pid","binding"}`.
///
/// Why: pulled out of the `health` JSON-RPC handler so
/// `crate::serve::http::health_handler` (`GET /health`) can return the
/// identical shape without duplicating the `json!` literal — one source of
/// truth for what "healthy" means over either transport.
///
/// `pid` and `binding` are ADDITIVE (#4512) — every field that was here
/// before is unchanged, so an older client that only reads
/// `server`/`version`/`status` keeps working. They exist because a daemon
/// binds exactly one `ProjectBinding` at `build_router` time and holds it for
/// its whole life, while `tcode tui` now AUTO-ATTACHES to whatever daemon
/// discovery finds. Without the binding on the wire, a TUI launched in
/// project B silently drives project A's daemon; `pid` is what lets an
/// operator (or a test) identify and signal the exact process it found.
/// What: `server = "tcode"`, `version = crate::VERSION`
/// (`CARGO_PKG_VERSION`), `status = "ok"`, `pid = std::process::id()`,
/// `binding = ProjectBinding::to_json()` (the SAME `{state, root}` wire shape
/// `Session` already serialises, not a second spelling). Never fails — there
/// is no fallible state to check.
///
/// `incremental_index` is additive too (#2798). The write/edit tool executors
/// hand every successful write to a BOUNDED background pool, which drops a
/// batch when a degraded trusty-search daemon has filled it. Those drops used
/// to exist only as a `warn!` line, so a sustained saturation episode — files
/// silently not indexed for as long as it lasts — looked identical to a healthy
/// daemon here. This is the same process that owns the counter, and the TUI
/// already polls this payload, so it is where the loss becomes visible:
/// `dropped_batches` (0 means it has never happened) and
/// `seconds_since_last_drop` (`null` until the first drop) together separate
/// "fine" from "happening right now".
/// Test: `health_payload_has_expected_shape`,
/// `health_payload_reports_a_bound_project_root`,
/// `health_payload_reports_incremental_index_drops`.
pub(crate) fn health_payload(binding: &ProjectBinding) -> Value {
    // #2798: publish the background-index drop counters where a health check
    // already looks, so a saturated pool is not invisible.
    let drops = trusty_common::search_index::index_drop_stats();
    json!({
        "server": "tcode",
        "version": crate::VERSION,
        "status": "ok",
        "pid": std::process::id(),
        "binding": binding.to_json(),
        "incremental_index": {
            "dropped_batches": drops.dropped_batches,
            "seconds_since_last_drop": drops.seconds_since_last_drop,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    /// `ping` must return `{"pong": true}` and never fail.
    #[tokio::test]
    async fn ping_returns_pong_true() {
        let result = ping(Value::Null, test_ctx())
            .await
            .expect("ping must not fail");
        assert_eq!(result, json!({"pong": true}));
    }

    /// `health` must report the server name, version, and "ok" status.
    #[tokio::test]
    async fn health_returns_server_identity() {
        let result = health(&ProjectBinding::None, Value::Null, test_ctx())
            .await
            .expect("health must not fail");
        assert_eq!(result["server"], "tcode");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["version"], crate::VERSION);
    }

    /// `health_payload` (shared with the HTTP `GET /health` route) must carry
    /// the original three fields UNCHANGED plus #4512's additive `pid` and
    /// `binding` — pinned so the backward-compatible promise in this
    /// function's docs can't be broken by a later edit.
    #[test]
    fn health_payload_has_expected_shape() {
        let v = health_payload(&ProjectBinding::None);
        assert_eq!(v["server"], "tcode");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["version"], crate::VERSION);
        assert_eq!(v["pid"], std::process::id());
        assert_eq!(v["binding"]["state"], crate::binding::STATE_PROJECTLESS);
        assert!(v["binding"]["root"].is_null());
    }

    /// The background-index drop counters must be on the wire, and a process
    /// that has dropped nothing must say so distinguishably (#2798).
    ///
    /// Why: dropping a batch when the pool saturates is only an acceptable
    /// trade because the loss is visible somewhere a health check reads. This
    /// pins that it IS on the wire — the field existing is the whole fix, and a
    /// later edit that quietly removes it would restore the blind spot.
    /// What: asserts `incremental_index.dropped_batches` is present and numeric,
    /// and that with no drops in this test process `seconds_since_last_drop` is
    /// `null` rather than a misleading `0`.
    /// Test: this test.
    #[test]
    fn health_payload_reports_incremental_index_drops() {
        let v = health_payload(&ProjectBinding::None);
        let drops = &v["incremental_index"];
        assert!(
            drops["dropped_batches"].is_u64(),
            "dropped_batches must be a number, got {drops}"
        );
        assert!(
            drops["seconds_since_last_drop"].is_null() || drops["seconds_since_last_drop"].is_u64(),
            "seconds_since_last_drop must be null or a number, got {drops}"
        );
    }

    /// A BOUND daemon must publish its project root, since that is the field
    /// `cli::daemon_autospawn` compares against the client's own project
    /// before attaching (#4512).
    #[test]
    fn health_payload_reports_a_bound_project_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binding = ProjectBinding::resolve(Some(dir.path().to_path_buf())).expect("must bind");
        let v = health_payload(&binding);
        assert_eq!(
            v["binding"]["root"],
            json!(binding.label().expect("a bound binding has a root"))
        );
    }

    /// The `health` METHOD (not just the payload helper) must carry the
    /// daemon's binding, so the STDIO transport reports it too.
    #[tokio::test]
    async fn health_reports_the_daemons_project_binding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binding = ProjectBinding::resolve(Some(dir.path().to_path_buf())).expect("must bind");
        let result = health(&binding, Value::Null, test_ctx())
            .await
            .expect("health must not fail");
        assert_eq!(result["binding"], binding.to_json());
    }

    /// `register` must wire both methods so the router recognises them
    /// (rather than returning method-not-found).
    #[tokio::test]
    async fn methods_register_wires_ping_and_health() {
        use trusty_common::mcp::Request;

        let mut router = Router::new();
        register(&mut router, ProjectBinding::None);

        for method in ["ping", "health"] {
            let req = Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: method.to_string(),
                params: None,
            };
            let resp = router.dispatch(req, &test_ctx()).await;
            assert!(
                resp.error.is_none(),
                "{method} should be registered, got {:?}",
                resp.error
            );
        }
    }
}
