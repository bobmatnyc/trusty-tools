//! Per-method admission and deadline lane pins for the whole socket surface.
//!
//! Why: slice 4 shipped two lane tests, each pinning ONE representative method
//! per lane — `search.index.file.put` for bulk, `search.index.delete` for free.
//! Its review recorded the gap those leave: moving a method between
//! `bulk_write!` and `free_write!` by mistake changes what a caller experiences
//! under load and no test sees it, because the `*_over_the_socket_matches_the_
//! http_body` parity tests compare BODIES and say nothing about admission. This
//! file replaces the representative with the whole table.
//!
//! **The table is the assertion, and it is checked against
//! `service::socket::METHODS` first.** A method that reaches the socket without
//! a row here fails `every_socket_method_declares_a_lane` rather than being
//! silently untested — so the pin cannot fall behind the surface it pins.
//!
//! ## What each lane means, and where it comes from
//!
//! `service::server::build_router_on` splits its routes into three groups, and
//! every socket method mirrors the group its HTTP route is in:
//!
//! | Lane | HTTP group | Admission permit | Query deadline |
//! |---|---|---|---|
//! | [`Lane::Free`] | `free` | no | no |
//! | [`Lane::Bulk`] | `bulk_limited` | yes | no |
//! | [`Lane::Interactive`] | `interactive_limited` | yes | yes |
//!
//! ## The two axes are pinned differently, because they are observable
//! differently
//!
//! The admission axis is pinned for EVERY method: hold the only permit, call
//! each one, and the answer is either the limiter's refusal or the core's own
//! verdict. Nothing about the subject matters, so an unknown index does.
//!
//! The deadline axis is not symmetrical. `tokio::time::timeout` polls its inner
//! future BEFORE checking the deadline, so a handler that answers without ever
//! pending answers identically in both lanes however small the deadline is — a
//! zero-deadline sweep over fast subjects would pass whatever the wrapper did,
//! which is a test that cannot fail. The axis is therefore pinned against a
//! subject that genuinely pends: the test holds an index's indexer write lock,
//! so every method that takes that lock is stalled at a real await point. Under
//! a zero deadline the six interactive methods must then answer
//! `CODE_DEADLINE_EXCEEDED`, and the lock-taking methods in the other two lanes
//! must still be running — which is what a method wrongly moved INTO the
//! interactive lane would stop doing.
//!
//! Test: this file IS the test module for `super`.

use std::sync::Arc;
use std::time::Duration;

use trusty_common::uds::server::{RpcOutcome, RpcRouter};

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::concurrency::ConcurrencyLimiter;
use crate::service::query_timeout::QueryTimeoutConfig;
use crate::service::rpc::error::{CODE_DEADLINE_EXCEEDED, CODE_UNAVAILABLE};
use crate::service::server::SearchAppState;
use crate::service::socket::METHODS;

/// The index every case below names. Registered, so a method reaches its core
/// rather than stopping at a registry lookup.
const INDEX: &str = "lanes-6285";

/// Which of `build_router_on`'s three route groups a method mirrors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// No limiter, no deadline.
    Free,
    /// Admission-limited, no deadline — legitimately long-running.
    Bulk,
    /// Admission-limited AND deadline-bounded.
    Interactive,
}

impl Lane {
    /// Whether a call in this lane must take an admission permit.
    fn takes_a_permit(self) -> bool {
        !matches!(self, Self::Free)
    }
}

/// Every method the socket serves, with the lane it must be in and params that
/// reach its core.
///
/// The params are deliberately minimal — enough to decode, never enough to
/// succeed. What each method ANSWERS is the business of its family's parity
/// tests; this file reads only whether the limiter or the deadline spoke first.
fn lanes() -> Vec<(&'static str, Lane, serde_json::Value)> {
    use crate::service::rpc::{queries, reads, streams, writes};

    let index = serde_json::json!({ "index_id": INDEX });
    let query = serde_json::json!({ "index_id": INDEX, "body": { "text": "anything" } });
    // `reads::IndexScoped` flattens `P` into `params`, so a scoped read's own
    // fields sit BESIDE `index_id` rather than nested under it.
    let scoped = serde_json::json!({ "index_id": INDEX });

    vec![
        // The listener's own method, and slice 2's reads: every HTTP route is in
        // `free`.
        (
            crate::service::socket::METHOD_HEALTH,
            Lane::Free,
            serde_json::Value::Null,
        ),
        (
            reads::METHOD_INDEXES_LIST,
            Lane::Free,
            serde_json::json!({}),
        ),
        (reads::METHOD_INDEX_STATUS, Lane::Free, index.clone()),
        (reads::METHOD_INDEX_CONFIG_GET, Lane::Free, index.clone()),
        (
            reads::METHOD_CONFIG_GET,
            Lane::Free,
            serde_json::Value::Null,
        ),
        (reads::METHOD_CHUNKS_LIST, Lane::Free, scoped.clone()),
        (reads::METHOD_GRAPH_GET, Lane::Free, scoped.clone()),
        (reads::METHOD_GRAPH_STATS, Lane::Free, index.clone()),
        (
            reads::METHOD_GRAPH_NEIGHBORS,
            Lane::Free,
            serde_json::json!({ "index_id": INDEX, "symbol": "anything" }),
        ),
        (
            reads::METHOD_CALL_CHAIN,
            Lane::Free,
            serde_json::json!({ "index_id": INDEX, "entry_point": "anything" }),
        ),
        // Slice 3's queries: every HTTP route is in `interactive_limited`.
        (queries::METHOD_QUERY, Lane::Interactive, query.clone()),
        (
            queries::METHOD_QUERY_ALL,
            Lane::Interactive,
            serde_json::json!({ "query": "anything" }),
        ),
        (
            queries::METHOD_GREP,
            Lane::Interactive,
            serde_json::json!({ "index_id": INDEX, "body": { "pattern": "anything" } }),
        ),
        (
            queries::METHOD_GREP_ALL,
            Lane::Interactive,
            serde_json::json!({ "pattern": "anything" }),
        ),
        (
            queries::METHOD_SIMILAR,
            Lane::Interactive,
            serde_json::json!({ "index_id": INDEX, "body": { "file": "src/a.rs" } }),
        ),
        (
            queries::METHOD_TYPEAHEAD,
            Lane::Interactive,
            serde_json::json!({ "index_id": INDEX, "q": "any" }),
        ),
        // Slice 4's writes: the three registry-level routes are in `free`, the
        // four per-index ones in `bulk_limited`.
        (
            writes::METHOD_INDEX_CREATE,
            Lane::Free,
            serde_json::json!({ "id": INDEX, "root_path": "/nonexistent/lanes" }),
        ),
        (writes::METHOD_INDEX_DELETE, Lane::Free, index.clone()),
        (
            writes::METHOD_INDEX_RELOCATE,
            Lane::Free,
            serde_json::json!({ "index_id": INDEX, "body": { "root_path": "/nonexistent/l2" } }),
        ),
        (
            writes::METHOD_INDEX_FILE_PUT,
            Lane::Bulk,
            serde_json::json!({
                "index_id": INDEX, "body": { "path": "src/a.rs", "content": "fn a() {}" },
            }),
        ),
        (
            writes::METHOD_INDEX_FILE_REMOVE,
            Lane::Bulk,
            serde_json::json!({ "index_id": INDEX, "body": { "path": "src/a.rs" } }),
        ),
        (
            writes::METHOD_INDEX_REINDEX,
            Lane::Bulk,
            serde_json::json!({ "index_id": INDEX }),
        ),
        (
            writes::METHOD_GRAPH_INGEST,
            Lane::Bulk,
            serde_json::json!({
                "index_id": INDEX,
                "body": { "producer": "lanes", "nodes": [], "edges": [] },
            }),
        ),
        // Slice 5's streams: both HTTP routes are in `free`.
        (
            streams::METHOD_STATUS_STREAM,
            Lane::Free,
            serde_json::Value::Null,
        ),
        (
            streams::METHOD_INDEX_REINDEX_STREAM,
            Lane::Free,
            index.clone(),
        ),
    ]
}

/// A state with `permits` admission permits and `deadline` as the query budget,
/// carrying one registered index.
fn state_with(permits: usize, deadline: Duration) -> Arc<SearchAppState> {
    let registry = IndexRegistry::new();
    let root = "/nonexistent/lanes-6285";
    registry.register(IndexHandle::bare(
        IndexId::new(INDEX.to_string()),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(INDEX, root))),
        root.into(),
    ));
    Arc::new(SearchAppState::new(registry).with_query_guards(
        ConcurrencyLimiter::with_limits(permits, 0),
        QueryTimeoutConfig::from_duration(deadline),
    ))
}

/// The whole socket router, exactly as `service::socket` assembles it.
fn router(state: &Arc<SearchAppState>) -> RpcRouter {
    use crate::service::rpc::{queries, reads, streams, writes};

    let held = Arc::clone(state);
    let router = RpcRouter::new()
        .typed::<crate::service::socket::NoParams, serde_json::Value, _, _>(
            crate::service::socket::METHOD_HEALTH,
            move |_params| {
                let state = Arc::clone(&held);
                async move { Ok(crate::service::server::health_report(state).await) }
            },
        );
    let router = reads::register(router, state);
    let router = queries::register(router, state);
    let router = writes::register(router, state);
    streams::register(router, state)
}

/// The error code one call answered, whether it is a unary method or a stream.
///
/// A stream's refusal arrives as its first item rather than as a response frame,
/// so both shapes are read here and the caller compares one number.
async fn code_of(rpc: &RpcRouter, method: &str, params: &serde_json::Value) -> Option<i64> {
    // The flag must match the table the name lives in: `"stream": true` against
    // a unary method is refused with `CODE_STREAM_UNSUPPORTED` before any lane
    // wrapper runs, and its absence against a streaming one is refused with
    // `CODE_STREAM_REQUIRED`. Either refusal would read as a lane verdict.
    let streaming = crate::service::rpc::streams::METHODS.contains(&method);
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params, "stream": streaming,
    }))
    .expect("encode the frame");
    match rpc.dispatch_streaming(&frame).await {
        RpcOutcome::Single(response) => response.error.map(|e| e.code),
        RpcOutcome::Stream { mut items, .. } => match items.recv().await {
            Some(Err(error)) => Some(error.code),
            _ => None,
        },
        // `RpcOutcome` is `#[non_exhaustive]`: a variant added upstream must
        // fail this pin loudly rather than be read as "no error".
        other => panic!("{method} answered an outcome this pin cannot read: {other:?}"),
    }
}

/// Why: the table below is only a pin while it covers the surface. A slice that
/// adds a method and forgets a row would otherwise leave that method's lane
/// untested with every other test still green — which is exactly the state
/// slice 4 shipped in, one representative per lane.
/// Test: this function IS the test.
#[test]
fn every_socket_method_declares_a_lane() {
    let mut declared: Vec<&str> = lanes().into_iter().map(|(name, _, _)| name).collect();
    declared.sort_unstable();
    let mut served: Vec<&str> = METHODS.to_vec();
    served.sort_unstable();
    assert_eq!(
        declared, served,
        "every method the socket serves must declare its lane here, and no other"
    );
}

/// Why: the admission limiter is tower middleware on the axum side, which a
/// second transport cannot reach — each socket method re-states its lane in
/// code, and a mis-stated one is invisible in a body comparison. With the only
/// permit held, a limited method must answer the limiter's `-32002` and a free
/// method must reach its core and answer whatever the core says. Twenty-five
/// rows, so moving any single method between lanes fails here.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn every_socket_method_takes_the_admission_lane_its_http_route_takes() {
    let state = state_with(1, Duration::from_secs(30));
    let rpc = router(&state);

    // One permit, held for the whole sweep. Both transports read this limiter
    // off the shared state, so this saturates the daemon rather than a copy.
    let _held = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .expect("the first admission must succeed");

    for (method, lane, params) in lanes() {
        let code = code_of(&rpc, method, &params).await;
        if lane.takes_a_permit() {
            assert_eq!(
                code,
                Some(CODE_UNAVAILABLE),
                "{method} is in the {lane:?} lane and must queue behind a saturated limiter"
            );
        } else {
            assert_ne!(
                code,
                Some(CODE_UNAVAILABLE),
                "{method} is in the free lane and must not queue behind a limiter it is not in"
            );
        }
    }
}

/// Why: the deadline is the second thing the interactive wrapper adds, and a
/// method that kept its permit but lost its deadline would pass the sweep above
/// unchanged. Against a subject that genuinely pends — an index whose indexer
/// write lock this test holds — a zero deadline must cut every interactive
/// method and no other. The non-interactive control set is the methods that take
/// the SAME lock, so "still running" is a statement about the wrapper rather
/// than about whether the call reached the lock at all.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn only_the_interactive_lane_is_deadline_bounded() {
    let state = state_with(8, Duration::ZERO);
    let rpc = Arc::new(router(&state));

    // Stall every handler that touches this index at a real await point.
    let handle = state
        .registry
        .get(&IndexId::new(INDEX.to_string()))
        .expect("the planted index is registered");
    let _write = handle.indexer.write().await;

    for (method, lane, params) in lanes() {
        if lane != Lane::Interactive {
            continue;
        }
        let code = tokio::time::timeout(
            Duration::from_secs(10),
            code_of(&rpc, method, &params.clone()),
        )
        .await
        .unwrap_or_else(|_| panic!("{method} is deadline-bounded and must not hang"));
        assert_eq!(
            code,
            Some(CODE_DEADLINE_EXCEEDED),
            "{method} is in the interactive lane and must report the expired deadline"
        );
    }

    // The control set: the same stall, in the other two lanes. Each must still
    // be running when a deadline a hundred times the budget has passed.
    for method in [
        crate::service::rpc::writes::METHOD_INDEX_FILE_PUT,
        crate::service::rpc::writes::METHOD_INDEX_FILE_REMOVE,
        crate::service::rpc::reads::METHOD_INDEX_STATUS,
        crate::service::rpc::reads::METHOD_CHUNKS_LIST,
    ] {
        let (_, _, params) = lanes()
            .into_iter()
            .find(|(name, _, _)| *name == method)
            .expect("the control method is in the table");
        let outcome = tokio::time::timeout(
            Duration::from_millis(300),
            code_of(&rpc, method, &params.clone()),
        )
        .await;
        assert!(
            outcome.is_err(),
            "{method} is not in the interactive lane and must still be running, \
             not cut by a deadline it does not have: answered {outcome:?}"
        );
    }
}
