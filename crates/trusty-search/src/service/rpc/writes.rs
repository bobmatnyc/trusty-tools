//! The write surface, served as JSON-RPC methods (#6285 slice 4).
//!
//! Why: slices 2 and 3 moved the routes that ASK something of an index. This
//! slice moves the seven that CHANGE one — registration, deregistration,
//! relocation, the two per-file writes, the reindex trigger, and
//! contributed-graph ingest. Since #9214 the socket is the only transport;
//! the route column below names the retired HTTP route each method replaced.
//!
//! What: the method-to-route table below, the params each method decodes, the
//! two lane wrappers (`bulk_guarded` and `unguarded`), and [`register`].
//!
//! ## Method → route
//!
//! | Method | Former HTTP route | Lane |
//! |---|---|---|
//! | `search.index.create` | `POST /indexes` | free |
//! | `search.index.delete` | `DELETE /indexes/{id}` | free |
//! | `search.index.relocate` | `PATCH /indexes/{id}` | free |
//! | `search.index.roots.add` | `POST /indexes/{id}/roots` | free |
//! | `search.index.file.put` | `POST /indexes/{id}/index-file` | bulk |
//! | `search.index.file.remove` | `POST /indexes/{id}/remove-file` | bulk |
//! | `search.index.reindex` | `POST /indexes/{id}/reindex` | bulk |
//! | `search.graph.ingest` | `POST /indexes/{id}/graph` | bulk |
//! | `search.index.quantize` | `POST /indexes/{id}/quantize` | bulk |
//!
//! The reindex TRIGGER is here; `GET /indexes/{id}/reindex/stream` and the other
//! SSE routes are slice 5's.
//!
//! ## Two lanes, because the retired axum router put these routes in two groups
//!
//! The retired axum router split its routes three ways, and these seven landed
//! in two of them. The four per-index writes are `bulk_limited`:
//! admission-limited on the SAME `SearchAppState` semaphore the query surface
//! uses, and deliberately NOT deadline-bounded, because a reindex or an ingest
//! can legitimately run for minutes (`bulk_guarded`). The three registry-level
//! routes are in `free`: no limiter and no deadline at all (`unguarded`).
//!
//! That asymmetry is copied, not invented. Slice 3 put the whole query family
//! behind one wrapper because axum puts the whole query family behind one
//! middleware stack; here the router disagrees with itself, so this module
//! disagrees the same way. Wrapping the registry-level three in the limiter for
//! symmetry would make a `search.index.delete` queue behind a running reindex
//! that `DELETE /indexes/{id}` sails past — a difference between the transports
//! introduced by the fix for a difference that was not there.
//!
//! ## What guards these methods
//!
//! Everything slice 2's `reads` module documents — the peer-uid check over a
//! `0600` socket in a `0700` directory. What does NOT carry over is
//! `with_guarded_middleware`, the #3304 same-origin write guard the axum router
//! wraps every destructive route in. It is browser-CSRF defence for a listener a
//! page can reach; a Unix socket has no origin and no browser, and the peer-uid
//! check is the stronger statement it was standing in for (#6277 design review,
//! the same conclusion slice 1 recorded for `SelfOrigins`).
//!
//! ## The frame cap bites `search.graph.ingest`, and only it
//!
//! The listener's frame budget is `service::socket::MAX_FRAME_BYTES`, 64 MiB —
//! the body limit the retired `POST /indexes/{id}/graph` route carried. PR #1129
//! recorded ~20 MB maxima from large pilot corpora. A document over 64 MiB is
//! refused LOUDLY: the listener stops reading at the budget and answers a parse
//! error, and a `trusty_common::uds` client reports `FrameTooLarge` rather than
//! truncating. Nothing partial is stored, because the refusal happens before the
//! frame is ever dispatched to a method. A client must dial through
//! `send_framed_request_capped` with the same figure; the plain helper applies
//! the shared 8 MiB default to the response it reads.
//!
//! Test: `writes_tests.rs` — one `*_over_the_socket_matches_the_http_body` per
//! family plus, for every mutating core, a failure arm proving the refusal is
//! identical AND that registry / on-disk state did not advance behind it.
//!
//! [`register`]: crate::service::rpc::writes::register

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::service::server::{
    AddRootsRequest, CreateIndexRequest, DeleteIndexParams, IndexFileRequest, IngestGraphRequest,
    QuantizeRequest, ReindexRequest, RelocateIndexRequest, RemoveFileRequest, SearchAppState,
};

use super::as_http_body;
use super::error::rpc_error_from_http;

#[cfg(test)]
#[path = "writes_tests.rs"]
mod tests;

/// `POST /indexes` — register a new (or re-join an existing) index.
pub const METHOD_INDEX_CREATE: &str = "search.index.create";
/// `DELETE /indexes/{id}` — deregister an index, optionally destroying its data.
pub const METHOD_INDEX_DELETE: &str = "search.index.delete";
/// `PATCH /indexes/{id}` — rebind an index to a new root path.
pub const METHOD_INDEX_RELOCATE: &str = "search.index.relocate";
/// `POST /indexes/{id}/index-file` — add or replace one file in an index.
pub const METHOD_INDEX_FILE_PUT: &str = "search.index.file.put";
/// `POST /indexes/{id}/remove-file` — drop one file's chunks from an index.
pub const METHOD_INDEX_FILE_REMOVE: &str = "search.index.file.remove";
/// `POST /indexes/{id}/reindex` — queue a full reindex. The trigger only.
pub const METHOD_INDEX_REINDEX: &str = "search.index.reindex";
/// `POST /indexes/{id}/graph` — ingest one producer's contributed graph.
pub const METHOD_GRAPH_INGEST: &str = "search.graph.ingest";
/// Park this index's embedding stage at its next batch boundary (#6524).
/// Socket-only — this pair has no HTTP twin.
pub const METHOD_INDEX_PAUSE_EMBEDDING: &str = "search.index.pause_embedding";
/// Let this index's embedding stage continue (#6524). Socket-only.
pub const METHOD_INDEX_RESUME_EMBEDDING: &str = "search.index.resume_embedding";
/// `POST /indexes/{id}/quantize` — the scalar-precision backfill (#6822).
///
/// Added by the #6285 consumer move: it was the one route the CLI dialled
/// that had no socket twin.
pub const METHOD_INDEX_QUANTIZE: &str = "search.index.quantize";

/// `POST /indexes/{id}/roots` — add directory trees to an index (#7434).
pub const METHOD_INDEX_ROOTS_ADD: &str = "search.index.roots.add";

/// Every method this slice registers, in registration order.
///
/// Why: same contract as `reads::METHODS` and `queries::METHODS` —
/// `service::socket::METHODS` lists these by reference rather than by a second
/// set of literals, so a rename here is a compile error there rather than a
/// drift only a running consumer would find.
/// Test: `rpc_router_registers_every_documented_method` and
/// `every_family_method_is_spliced_into_the_socket_method_list` in
/// `socket_tests.rs`.
pub const METHODS: &[&str] = &[
    METHOD_INDEX_CREATE,
    METHOD_INDEX_DELETE,
    METHOD_INDEX_RELOCATE,
    METHOD_INDEX_FILE_PUT,
    METHOD_INDEX_FILE_REMOVE,
    METHOD_INDEX_REINDEX,
    METHOD_GRAPH_INGEST,
    // #6524 — the embedding pause pair.
    METHOD_INDEX_PAUSE_EMBEDDING,
    METHOD_INDEX_RESUME_EMBEDDING,
    // #6285 consumer move — the quantize backfill.
    METHOD_INDEX_QUANTIZE,
    // #7434 — the multi-root mutation.
    METHOD_INDEX_ROOTS_ADD,
];

/// An index-scoped write whose HTTP form takes a request BODY.
///
/// Why nested rather than flattened: same reason as `queries::IndexBody` — the
/// body is one JSON document on HTTP and stays one here, so `B`'s own
/// `Deserialize` runs unmodified and cannot decode differently because a field
/// now shares a namespace with `index_id`.
/// What: `{"index_id": "x", "body": { … }}`, where `body` is byte-for-byte the
/// JSON a caller would POST.
/// Test: every `*_over_the_socket_matches_the_http_body` case in
/// `writes_tests.rs` sends this shape.
#[derive(Debug, Deserialize)]
pub struct IndexBody<B> {
    /// The index the call is about — the `{id}` path segment on HTTP.
    pub index_id: String,
    /// The request body, exactly as the HTTP route's JSON extractor takes it.
    pub body: B,
}

/// The params of `search.index.delete`.
///
/// Why its own type rather than [`IndexBody`]: `delete_data` is a QUERY
/// parameter on HTTP, not a body, and it is the destructive toggle — #4123 made
/// it strictly opt-in and had axum reject an unparseable value with `400` rather
/// than guess. Decoding it as a plain optional field here keeps the same rule:
/// absent means `false`, and a value that is not a bool is `invalid_params`
/// rather than a silent default either way.
/// What: `{"index_id": "x"}` deregisters; `{"index_id": "x", "delete_data":
/// true}` also destroys the data directory.
/// Test: `an_allowlist_excluded_registration_deletes_over_the_socket_too`.
#[derive(Debug, Deserialize)]
pub struct DeleteIndex {
    /// The index to deregister — the `{id}` path segment on HTTP.
    pub index_id: String,
    /// Opt in to destroying the on-disk data directory (#4123).
    #[serde(default)]
    pub delete_data: bool,
    /// The `root_path` the caller believes this registration still has (#6380).
    ///
    /// Absent ⇒ unchecked. Present ⇒ the delete is refused with `CONFLICT`
    /// unless the registration's current root is this exact string.
    #[serde(default)]
    pub expected_root_path: Option<String>,
}

/// The params of `search.index.reindex`.
///
/// Why the body is optional: `POST /indexes/{id}/reindex` accepts an empty body,
/// which axum's `Option<Json<_>>` decodes to `None` and the handler reads as
/// "no override, no force, interactive". A caller that sends no `body` field
/// here reaches the same arm.
/// Test: `reindex_over_the_socket_matches_the_http_body`.
#[derive(Deserialize)]
pub struct ReindexParams {
    /// The index to reindex — the `{id}` path segment on HTTP.
    pub index_id: String,
    /// The optional request body, exactly as the HTTP route takes it.
    #[serde(default)]
    pub body: Option<ReindexRequest>,
}

/// The params of `search.index.quantize`.
///
/// Why the body is optional: `POST /indexes/{id}/quantize` accepts an empty
/// body and reads it as "convert to the current default, for real"; a caller
/// that sends no `body` here reaches the same arm.
/// What: `{"index_id": "x", "body": {"quant": "f16", "dry_run": true}}`.
/// Test: `quantize_over_the_socket_matches_the_http_body`.
#[derive(Deserialize)]
pub struct QuantizeParams {
    /// The index to convert — the `{id}` path segment on HTTP.
    pub index_id: String,
    /// The optional request body, exactly as the HTTP route takes it.
    #[serde(default)]
    pub body: Option<QuantizeRequest>,
}

/// Run one write body behind the admission limiter, with NO query deadline.
///
/// Why: the four per-index writes were the retired router's `bulk_limited`
/// group. The limiter is there because each in-flight write holds a parsed
/// batch, an embedding buffer and an indexer lock, so an unbounded burst
/// degrades every other response including `search.health` (#41). The deadline
/// is deliberately absent: a reindex or a large ingest legitimately outruns any
/// interactive budget.
/// What: `admit` on the ONE `SearchAppState` limiter, permit held until the body
/// finishes. A refusal's status becomes the JSON-RPC code, so a queue-full
/// `503` reads the same as on the query methods.
/// Test: `writes_are_refused_when_the_shared_limiter_is_saturated`,
/// `a_registry_level_write_is_not_admission_limited`.
async fn bulk_guarded<T, F>(state: &Arc<SearchAppState>, body: F) -> Result<T, RpcError>
where
    F: std::future::Future<Output = Result<T, (axum::http::StatusCode, serde_json::Value)>>,
{
    let _permit = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .map_err(|(status, refusal)| rpc_error_from_http(status, &refusal))?;
    body.await
        .map_err(|(status, refusal)| rpc_error_from_http(status, &refusal))
}

/// Run one registry-level write with no limiter and no deadline.
///
/// Why: create, delete and relocate were the retired router's `free` group —
/// no limiter, no deadline — so a delete never queues behind a running reindex;
/// see the module doc's lane section.
/// What: the refusal mapping [`bulk_guarded`] also applies, and nothing else.
/// Test: `create_over_the_socket_matches_the_http_body` and its siblings.
async fn unguarded<T, F>(body: F) -> Result<T, RpcError>
where
    F: std::future::Future<Output = Result<T, (axum::http::StatusCode, serde_json::Value)>>,
{
    body.await
        .map_err(|(status, refusal)| rpc_error_from_http(status, &refusal))
}

/// Mount every method in [`METHODS`] onto `router`.
///
/// Why: the route-specific half of the write surface, kept beside `reads` and
/// `queries` so each slice adds one file rather than editing one.
/// What: one `typed` registration per method, each cloning the `Arc` handle to
/// the shared [`SearchAppState`] and running its `*_report` core through the
/// wrapper that matches its lane. Every fallible body returns the
/// `(status, body)` pair its core returns, and [`rpc_error_from_http`] turns
/// that into the error frame.
/// Test: `rpc_router_registers_every_documented_method` in `socket_tests.rs`;
/// the `*_over_the_socket_matches_the_http_body` cases and their failure-arm
/// siblings drive each registration against its `*_report` core.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    /// One `free`-lane method whose core answers a `serde_json::Value`.
    macro_rules! free_write {
        ($router:expr, $name:expr, $req:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, serde_json::Value, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { unguarded(async { $call }).await }
            })
        }};
    }

    /// One `bulk_limited`-lane method whose core answers a `serde_json::Value`.
    macro_rules! bulk_write {
        ($router:expr, $name:expr, $req:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, serde_json::Value, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { bulk_guarded(&$s, async { $call }).await }
            })
        }};
    }

    use super::reads::IndexRef;
    use crate::service::server::{
        add_index_roots_report, create_index_report, delete_index_report, index_file_report,
        ingest_graph_report, pause_embedding_report, quantize_report, reindex_report,
        relocate_index_report, remove_file_report, resume_embedding_report,
    };

    let r = router;

    // ---- registry-level lifecycle (axum's `free` group) ---------------------
    let r = free_write!(r, METHOD_INDEX_CREATE, CreateIndexRequest, |s, p| {
        // #9169: a new index's group gets a `create` pin when it has none.
        let id = p.id.clone();
        let mut created = create_index_report(&s, p).await;
        if let Ok(body) = created.as_mut() {
            if let Some(e) = super::project_map::pin_after_create(&s, &id).await {
                body["project_map_error"] = serde_json::json!(e);
            }
        }
        created
    });
    let r = free_write!(r, METHOD_INDEX_DELETE, DeleteIndex, |s, p| {
        delete_index_report(
            &s,
            &p.index_id,
            DeleteIndexParams {
                delete_data: p.delete_data,
                expected_root_path: p.expected_root_path,
            },
        )
        .await
    });
    let r = free_write!(
        r,
        METHOD_INDEX_RELOCATE,
        IndexBody<RelocateIndexRequest>,
        |s, p| relocate_index_report(&s, &p.index_id, p.body).await
    );
    // #7434: free lane, like relocate — it swaps a handle and queues a walk.
    let r = free_write!(
        r,
        METHOD_INDEX_ROOTS_ADD,
        IndexBody<AddRootsRequest>,
        |s, p| add_index_roots_report(&s, &p.index_id, p.body).await
    );

    // ---- per-file writes (axum's `bulk_limited` group) ----------------------
    let r = bulk_write!(
        r,
        METHOD_INDEX_FILE_PUT,
        IndexBody<IndexFileRequest>,
        |s, p| index_file_report(&s, &p.index_id, p.body).await
    );
    let r = bulk_write!(
        r,
        METHOD_INDEX_FILE_REMOVE,
        IndexBody<RemoveFileRequest>,
        |s, p| remove_file_report(&s, &p.index_id, p.body).await
    );

    // ---- reindex trigger (the SSE stream is slice 5) ------------------------
    let r = bulk_write!(r, METHOD_INDEX_REINDEX, ReindexParams, |s, p| {
        reindex_report(&s, &p.index_id, p.body).await
    });

    // ---- quantize backfill (#6822), the bulk lane its HTTP route takes -------
    let r = bulk_write!(r, METHOD_INDEX_QUANTIZE, QuantizeParams, |s, p| {
        quantize_report(&s, &p.index_id, p.body).await
    });

    // ---- embedding pause/resume (#6524) -------------------------------------
    // Deliberately in the `free` lane, not `bulk_limited`: these flip one atomic
    // and return. Putting them behind the admission limiter would let a
    // saturated index refuse the very call that relieves it, which is the
    // opposite of what the control is for.
    let r = free_write!(r, METHOD_INDEX_PAUSE_EMBEDDING, IndexRef, |s, p| {
        pause_embedding_report(&s, &p.index_id)
    });
    let r = free_write!(r, METHOD_INDEX_RESUME_EMBEDDING, IndexRef, |s, p| {
        resume_embedding_report(&s, &p.index_id)
    });

    // ---- contributed-graph ingest, the one TYPED report on this surface -----
    let held = Arc::clone(state);
    r.typed::<IndexBody<IngestGraphRequest>, serde_json::Value, _, _>(
        METHOD_GRAPH_INGEST,
        move |p| {
            let s = Arc::clone(&held);
            async move {
                as_http_body(
                    bulk_guarded(&s, async {
                        ingest_graph_report(&s, &p.index_id, p.body).await
                    })
                    .await?,
                )
            }
        },
    )
}
