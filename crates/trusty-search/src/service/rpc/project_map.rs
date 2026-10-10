//! `search.project.map`, `.park`, `.unpark`, `.pin` — the project map over the
//! socket (#9169 PR-B1).
//!
//! Why: trusty-search owns `project-map.toml` (ruling f6); tm and the
//! `trusty-search project` CLI read and change it only through these methods,
//! so the daemon is the one writer that also unloads and reloads indexes.
//! What: [`register`] mounts the four methods. Each runs the blocking rule in
//! [`crate::service::project_map_state`] off the async runtime; park then
//! unloads the index, unpark reloads it and queues a reindex.
//!
//! | Method | HTTP route | Lane |
//! |---|---|---|
//! | `search.project.map` | none | free |
//! | `search.project.park` | none | free |
//! | `search.project.unpark` | none | free |
//! | `search.project.pin` | none | free |
//!
//! Free: a registry read, one map read-modify-write, and `stat`s bounded to
//! the target's group, like `search.project.resolve`.
//!
//! Test: `project_park_tests.rs` (socket) and `project_map_state_tests.rs` (rules).
//!
//! [`register`]: crate::service::rpc::project_map::register

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::{RpcError, RpcRouter, CODE_INTERNAL_ERROR};

use super::error::{CODE_CONFLICT, CODE_NOT_FOUND};
use super::project::map_error;
use crate::core::project_map::PinnedBy;
use crate::service::persistence::PersistedIndex;
use crate::service::project_map_state::{
    ensure_map, park_decision, pin_decision, pin_on_create, unpark_decision, MapContext, MapRefusal,
};
use crate::service::project_resolve::{Candidate, LiveDisk};
use crate::service::server::SearchAppState;
use crate::service::socket::NoParams;

/// Read the whole map (migrating it when absent).
pub const METHOD_PROJECT_MAP: &str = "search.project.map";
/// Park one index.
pub const METHOD_PROJECT_PARK: &str = "search.project.park";
/// Unpark one index and queue its reindex.
pub const METHOD_PROJECT_UNPARK: &str = "search.project.unpark";
/// Pin one index as its project's answer.
pub const METHOD_PROJECT_PIN: &str = "search.project.pin";

/// Every method this family registers. Spliced into `service::socket::METHODS`.
/// Test: `every_family_method_is_spliced_into_the_socket_method_list`.
pub const METHODS: &[&str] = &[
    METHOD_PROJECT_MAP,
    METHOD_PROJECT_PARK,
    METHOD_PROJECT_UNPARK,
    METHOD_PROJECT_PIN,
];

/// Params of the three methods that name one index.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectIndexParams {
    /// The index to park, unpark or pin.
    pub index_id: String,
}

/// Mount the four project-map methods onto `router`.
///
/// Why: the socket is the map's only remote write path.
/// What: `map` answers `{path, schema, projects, parked}`; `park` answers
/// `{index_id, parked, key, reason, canonical_index_id, unloaded}`; `unpark`
/// answers `{index_id, unparked, reindex_queued, reindex}` (or
/// `reindex_error`); `pin` answers `{index_id, key, pinned_by, previous}`.
/// An unreadable map or registry is `CODE_INTERNAL_ERROR` naming the file and
/// the remedy; an unknown or unparked id is `CODE_NOT_FOUND`; a refused park
/// or pin is `CODE_CONFLICT` with `data.error = "park_refused"` or
/// `"pin_refused"`.
/// Test: `the_map_method_lists_pins_and_parks`,
/// `park_refuses_a_main_checkout_and_records_a_worktree`,
/// `park_and_unpark_keep_the_corpus_and_queue_a_reindex`.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    let held = Arc::clone(state);
    let router = router.typed::<NoParams, serde_json::Value, _, _>(METHOD_PROJECT_MAP, move |_| {
        let state = Arc::clone(&held);
        async move { map_report(&state).await }
    });
    let held = Arc::clone(state);
    let router = router.typed::<ProjectIndexParams, serde_json::Value, _, _>(
        METHOD_PROJECT_PARK,
        move |p| {
            let state = Arc::clone(&held);
            async move { park_report(&state, &p.index_id).await }
        },
    );
    let held = Arc::clone(state);
    let router = router.typed::<ProjectIndexParams, serde_json::Value, _, _>(
        METHOD_PROJECT_UNPARK,
        move |p| {
            let state = Arc::clone(&held);
            async move { unpark_report(&state, &p.index_id).await }
        },
    );
    let held = Arc::clone(state);
    router.typed::<ProjectIndexParams, serde_json::Value, _, _>(METHOD_PROJECT_PIN, move |p| {
        let state = Arc::clone(&held);
        async move { pin_report(&state, &p.index_id).await }
    })
}

/// Run `f` with this call's registry, candidates and disk on the blocking
/// pool. An unreadable registry is an internal error; a refusal carries
/// `data.error = kind`.
async fn blocking<T: Send + 'static>(
    state: &Arc<SearchAppState>,
    kind: &'static str,
    f: impl FnOnce(&MapContext, &[Candidate], &LiveDisk) -> Result<T, MapRefusal> + Send + 'static,
) -> Result<(T, MapContext), RpcError> {
    let ctx = MapContext::capture(state).map_err(|e| map_error(&e))?;
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let candidates = ctx.candidates();
        let disk = ctx.disk(Some(runtime));
        f(&ctx, &candidates, &disk).map(|out| (out, ctx))
    })
    .await
    .map_err(|e| RpcError::new(CODE_INTERNAL_ERROR, format!("project-map task failed: {e}")))?
    .map_err(|e| refusal_error(e, kind))
}

/// Render a [`MapRefusal`]; `kind` names a refused park or pin.
fn refusal_error(e: MapRefusal, kind: &str) -> RpcError {
    match e {
        MapRefusal::Map(e) => map_error(&e),
        MapRefusal::UnknownIndex(_) | MapRefusal::NotParked(_) => {
            RpcError::new(CODE_NOT_FOUND, e.to_string())
        }
        MapRefusal::Refused(message) => RpcError::new(CODE_CONFLICT, message.clone())
            .with_data(serde_json::json!({ "error": kind, "message": message })),
    }
}

/// `search.project.map`: the map as stored, with its path.
async fn map_report(state: &Arc<SearchAppState>) -> Result<serde_json::Value, RpcError> {
    let (map, ctx) = blocking(state, "map_refused", |ctx, candidates, disk| {
        ensure_map(&ctx.map_path, candidates, disk).map_err(MapRefusal::from)
    })
    .await?;
    Ok(serde_json::json!({
        "path": ctx.map_path.to_string_lossy(),
        "schema": map.schema,
        "projects": map.projects,
        "parked": map.parked,
    }))
}

/// The registry row for `index_id`, if any.
fn row(ctx: &MapContext, index_id: &str) -> Option<PersistedIndex> {
    ctx.persisted.iter().find(|e| e.id == index_id).cloned()
}

/// `search.project.park`: record the park, then unload the index.
async fn park_report(
    state: &Arc<SearchAppState>,
    index_id: &str,
) -> Result<serde_json::Value, RpcError> {
    let id = index_id.to_string();
    let ((parked, canonical), ctx) =
        blocking(state, "park_refused", move |ctx, candidates, disk| {
            park_decision(&ctx.map_path, &id, candidates, disk)
        })
        .await?;
    // #9169: the map row is written first, so a request racing the unload is
    // already refused as parked.
    let unloaded =
        crate::service::server::detach_parked(state, index_id, row(&ctx, index_id)).await;
    Ok(serde_json::json!({
        "index_id": index_id,
        "parked": true,
        "key": parked.key,
        "reason": parked.reason,
        "canonical_index_id": canonical,
        "unloaded": unloaded,
    }))
}

/// `search.project.unpark`: clear the row, reload, queue a reindex.
async fn unpark_report(
    state: &Arc<SearchAppState>,
    index_id: &str,
) -> Result<serde_json::Value, RpcError> {
    let id = index_id.to_string();
    let (_, ctx) = blocking(state, "unpark_refused", move |ctx, _, _| {
        unpark_decision(&ctx.map_path, &id)
    })
    .await?;
    let queued =
        crate::service::server::reindex_after_unpark(state, index_id, row(&ctx, index_id)).await;
    // The row is gone either way; a reindex that could not be queued is
    // reported, and `trusty-search reindex` retries it.
    Ok(match queued {
        Ok(reindex) => serde_json::json!({
            "index_id": index_id, "unparked": true, "reindex_queued": true, "reindex": reindex,
        }),
        Err((status, body)) => serde_json::json!({
            "index_id": index_id, "unparked": true, "reindex_queued": false,
            "reindex_error": { "status": status.as_u16(), "body": body },
        }),
    })
}

/// `search.project.pin`: pin `index_id` for its group (`operator`).
async fn pin_report(
    state: &Arc<SearchAppState>,
    index_id: &str,
) -> Result<serde_json::Value, RpcError> {
    let id = index_id.to_string();
    let ((pin, previous), _) = blocking(state, "pin_refused", move |ctx, candidates, disk| {
        pin_decision(&ctx.map_path, &id, PinnedBy::Operator, candidates, disk)
    })
    .await?;
    Ok(serde_json::json!({
        "index_id": pin.index_id,
        "key": pin.key,
        "pinned_by": pin.pinned_by,
        "previous": previous,
    }))
}

/// Pin a just-created index's group when it has none (`pinned_by = create`).
///
/// Why: a group created after the migration would otherwise answer only from
/// f7 until an operator pinned it.
/// What: runs [`pin_on_create`]; a failure never undoes the registration —
/// it is logged and returned as text for the create response to carry.
/// Test: `creating_an_index_for_an_unpinned_group_pins_it`.
pub(crate) async fn pin_after_create(
    state: &Arc<SearchAppState>,
    index_id: &str,
) -> Option<String> {
    let id = index_id.to_string();
    let pinned = blocking(state, "pin_refused", move |ctx, candidates, disk| {
        pin_on_create(&ctx.map_path, &id, candidates, disk)
    })
    .await;
    match pinned {
        Ok(_) => None,
        Err(e) => {
            tracing::error!(
                index_id,
                "project-map: no pin written for a new index: {}",
                e.message
            );
            Some(e.message)
        }
    }
}
