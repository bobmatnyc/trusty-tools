//! `search.project.resolve` — a project to its one live index, over the socket
//! (#9169).
//!
//! Why: callers guessed index ids (`trusty-tools` for `trusty-tools-4e2cf878`)
//! and got `unknown index`. trusty-search owns the project→index map (ruling
//! f6), so it answers the question once for every client.
//! What: [`register`] mounts the one method. Socket-only, like #6524's pair:
//! #6285 is retiring the HTTP listener, so no route is added there. The core is
//! [`crate::service::project_resolve`]; this file loads the registry, runs the
//! core off the async runtime, and renders a miss as an error frame.
//!
//! | Method | HTTP route | Lane |
//! |---|---|---|
//! | `search.project.resolve` | none | free |
//!
//! Free: one registry read and `stat` calls bounded to the matched group and
//! the five nearest candidates, never a `/Volumes` root (#9169). It is the call
//! a client makes before any query, so it must not queue behind the queries it
//! unblocks.
//!
//! Test: `rpc/project_tests.rs`.
//!
//! [`register`]: crate::service::rpc::project::register

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::{RpcError, RpcRouter, CODE_INTERNAL_ERROR};

use crate::service::project_resolve::{
    gather_candidates, resolve, Candidate, LiveDisk, ProjectQuery, ResolveMiss,
};
use crate::service::server::SearchAppState;

use super::error::{CODE_CONFLICT, CODE_NOT_FOUND};

/// Resolve a project name, `owner/repo`, or path to its one live index.
pub const METHOD_PROJECT_RESOLVE: &str = "search.project.resolve";

/// Every method this family registers. Spliced into `service::socket::METHODS`.
/// Test: `every_family_method_is_spliced_into_the_socket_method_list`.
pub const METHODS: &[&str] = &[METHOD_PROJECT_RESOLVE];

/// `search.project.resolve` params.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectResolveParams {
    /// A project name or index id, an `owner/repo`, or an absolute path.
    pub project: String,
}

/// Mount `search.project.resolve` onto `router`.
///
/// Why: the socket entry point for ruling f6 — one answer to "which index is
/// this project?" so no client guesses an id.
/// What: decodes [`ProjectResolveParams`], then [`resolve_report`]. A success
/// is `{index_id, root_path, repo_identity, kind, resident,
/// corpus_modified_unix, matched_by, duplicates}`; a miss is an error frame
/// whose `data` names the nearest candidates.
/// Test: `resolve_over_the_socket_answers_by_name_identity_and_path`,
/// `a_socket_miss_carries_the_nearest_candidates_as_data`,
/// `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    let held = Arc::clone(state);
    router.typed::<ProjectResolveParams, serde_json::Value, _, _>(
        METHOD_PROJECT_RESOLVE,
        move |params| {
            let state = Arc::clone(&held);
            async move { resolve_report(&state, &params.project).await }
        },
    )
}

/// Resolve `project` against this daemon's registrations (#9169).
///
/// Why: `indexes.toml` holds every registration with its identity and storage
/// layout; the hot registry says which are loaded. Both are read once per call.
/// What: parses the query (a bad one is `invalid_params`), loads the registry
/// (an unreadable one is `internal_error`, never an empty map that would turn
/// every call into a miss), and runs the core on the blocking pool, because it
/// `stat`s the roots it reports and a path outside every root shells out to
/// `git` for its identity.
/// Test: `resolve_over_the_socket_answers_by_name_identity_and_path`,
/// `an_unreadable_registry_is_an_internal_error_not_a_miss`.
pub async fn resolve_report(
    state: &Arc<SearchAppState>,
    project: &str,
) -> Result<serde_json::Value, RpcError> {
    let query = ProjectQuery::parse(project).map_err(RpcError::invalid_params)?;
    let persisted = match &state.registry_path_override {
        Some(path) => crate::service::persistence::load_index_registry_at(path),
        None => crate::service::persistence::load_index_registry(),
    }
    .map_err(|e| {
        tracing::error!(error = %e, "search.project.resolve: indexes.toml unreadable");
        RpcError::new(
            CODE_INTERNAL_ERROR,
            format!("could not read the index registry: {e}"),
        )
    })?;
    let resident: Vec<(String, std::path::PathBuf)> = state
        .registry
        .list_handles()
        .iter()
        .map(|h| (h.id.0.clone(), h.root_path.clone()))
        .collect();
    let project = project.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        // #9169: no disk read here — `resolve` probes only what it reports.
        let candidates = gather_candidates(&persisted, &resident);
        let disk = LiveDisk {
            names: crate::service::constants::ephemeral_dir_names(),
        };
        resolve(&query, &candidates, &disk)
    })
    .await
    .map_err(|e| RpcError::new(CODE_INTERNAL_ERROR, format!("resolve task failed: {e}")))?;
    match outcome {
        Ok(resolution) => super::as_http_body(resolution),
        Err(miss) => Err(miss_error(&project, miss)),
    }
}

/// Render a miss as an error frame whose `data` carries the candidates.
///
/// What: `project_not_found` and `no_live_index` answer `CODE_NOT_FOUND`;
/// `project_ambiguous` answers `CODE_CONFLICT`, since the request is valid and
/// the registry holds more than one answer. `data` is `{error, project,
/// candidates}` in every case.
/// Test: `a_socket_miss_carries_the_nearest_candidates_as_data`.
fn miss_error(project: &str, miss: ResolveMiss) -> RpcError {
    let (code, error, message, candidates): (i64, &str, String, Vec<Candidate>) = match miss {
        ResolveMiss::NotFound { nearest } => (
            CODE_NOT_FOUND,
            "project_not_found",
            format!("no index matches project {project:?}; see data.candidates"),
            nearest,
        ),
        ResolveMiss::Ambiguous { matches } => (
            CODE_CONFLICT,
            "project_ambiguous",
            format!("project {project:?} names more than one repo; send owner/repo or a path"),
            matches,
        ),
        ResolveMiss::NoLiveIndex { group } => (
            CODE_NOT_FOUND,
            "no_live_index",
            format!(
                "project {project:?} has only worktree or orphaned indexes; \
                 index its main checkout"
            ),
            group,
        ),
    };
    RpcError::new(code, message).with_data(serde_json::json!({
        "error": error,
        "project": project,
        "candidates": candidates,
    }))
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
