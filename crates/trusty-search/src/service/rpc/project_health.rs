//! `search.project.health` — registry-wide index counts over the socket
//! (#10535 PR 1).
//!
//! Why: `tm doctor` must report index hygiene without waking an index. Every
//! other per-index read either needs a resident index or answers
//! `503 index_not_resident` for a parked one, so tm had no safe way to count.
//! What: [`register`] mounts the one method. [`project_health_report`] reads
//! `indexes.toml`, the resident handle list and `project-map.toml`, and
//! [`project_health_counts`] reduces them to [`ProjectHealthCounts`]. No
//! corpus is opened, no index is loaded, and a missing map is not migrated:
//! the call writes nothing.
//!
//! | Method | HTTP route | Lane |
//! |---|---|---|
//! | `search.project.health` | none | free |
//!
//! Free: two small file reads and an in-memory handle count, like
//! `search.project.map`.
//!
//! Test: `rpc/project_health_tests.rs`.
//!
//! [`register`]: crate::service::rpc::project_health::register

use std::collections::HashSet;
use std::sync::Arc;

use trusty_common::search_rpc::ProjectHealthCounts;
use trusty_common::uds::server::{RpcError, RpcRouter, CODE_INTERNAL_ERROR};

use super::project::map_error;
use crate::core::project_map::{ParkReason, ProjectMap};
use crate::core::project_map_io;
use crate::service::persistence::PersistedIndex;
use crate::service::project_map_state::registry_path;
use crate::service::server::SearchAppState;
use crate::service::socket::NoParams;

/// Registry-wide index counts from stored state.
pub const METHOD_PROJECT_HEALTH: &str = "search.project.health";

/// Every method this family registers. Spliced into `service::socket::METHODS`.
/// Test: `every_family_method_is_spliced_into_the_socket_method_list`.
pub const METHODS: &[&str] = &[METHOD_PROJECT_HEALTH];

/// Mount `search.project.health` onto `router`.
///
/// Why: the socket entry point tm's doctor reads (#10535).
/// What: takes no params; answers [`ProjectHealthCounts`] as
/// `{registered, resident, parked, orphaned, never_queried}`. An unreadable
/// `indexes.toml` or `project-map.toml` is `CODE_INTERNAL_ERROR` naming the
/// file, never zero counts.
/// Test: `project_health_counts_follow_stored_state`,
/// `project_health_loads_no_corpus`,
/// `an_unreadable_project_map_is_an_internal_error_not_zero_counts`,
/// `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    let held = Arc::clone(state);
    router.typed::<NoParams, ProjectHealthCounts, _, _>(METHOD_PROJECT_HEALTH, move |_| {
        let state = Arc::clone(&held);
        async move { project_health_report(&state).await }
    })
}

/// Read the three stored inputs and count them (#10535).
///
/// Why: the counts must come from what the daemon already holds on disk and
/// in memory, so a parked or cold index is never loaded to be counted.
/// What: the resident ids are taken from the handle list first, then the two
/// files are read on the blocking pool. A missing map reads as "nothing
/// parked" and is not written (unlike `search.project.map`, which migrates).
/// Test: `project_health_counts_follow_stored_state`,
/// `project_health_loads_no_corpus`,
/// `a_missing_project_map_counts_nothing_parked_and_writes_no_map`,
/// `an_unreadable_project_map_is_an_internal_error_not_zero_counts`.
pub async fn project_health_report(
    state: &Arc<SearchAppState>,
) -> Result<ProjectHealthCounts, RpcError> {
    let resident: HashSet<String> = state
        .registry
        .list_handles()
        .iter()
        .map(|h| h.id.0.clone())
        .collect();
    let override_path = state.registry_path_override.clone();
    tokio::task::spawn_blocking(move || {
        let registry = registry_path(override_path.as_deref()).map_err(|e| map_error(&e))?;
        let persisted =
            crate::service::persistence::load_index_registry_at(&registry).map_err(|e| {
                tracing::error!(error = %e, "search.project.health: indexes.toml unreadable");
                RpcError::new(
                    CODE_INTERNAL_ERROR,
                    format!("index registry {} unreadable: {e:#}", registry.display()),
                )
            })?;
        let map =
            project_map_io::load(&project_map_io::map_path_beside(&registry)).map_err(|e| {
                tracing::error!(error = %e, "search.project.health: project-map.toml unreadable");
                map_error(&e)
            })?;
        Ok(project_health_counts(&persisted, &resident, map.as_ref()))
    })
    .await
    .map_err(|e| RpcError::new(CODE_INTERNAL_ERROR, format!("health task failed: {e}")))?
}

/// Reduce stored state to the five counts (pure, #10535).
///
/// Why: the definitions must be one function a test can pin row by row.
/// What:
/// - `registered`: rows of `persisted`.
/// - `resident`: ids in `resident` (loaded handles), registered or not.
/// - `parked`: registered ids with a `[[parked]]` row in `map`; a row naming
///   an unregistered id is not counted.
/// - `orphaned`: those parked rows whose stored reason is
///   [`ParkReason::Orphaned`]. The root is not re-checked, so an index whose
///   root vanished after it was parked for another reason is not counted, and
///   a `decommissioned` park never is.
/// - `never_queried`: registered rows with `last_queried_unix: None` — an
///   upper bound, since a pre-#993 row and an index queried only through a
///   path that does not stamp also read `None`.
///
/// Postcondition: `orphaned <= parked <= registered`, `never_queried <= registered`.
/// Test: `project_health_counts_follow_stored_state`.
pub fn project_health_counts(
    persisted: &[PersistedIndex],
    resident: &HashSet<String>,
    map: Option<&ProjectMap>,
) -> ProjectHealthCounts {
    let registered: HashSet<&str> = persisted.iter().map(|r| r.id.as_str()).collect();
    let parked_rows: Vec<_> = map
        .map(|m| {
            m.parked
                .iter()
                .filter(|p| registered.contains(p.index_id.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let counts = ProjectHealthCounts {
        registered: persisted.len() as u64,
        resident: resident.len() as u64,
        parked: parked_rows.len() as u64,
        orphaned: parked_rows
            .iter()
            .filter(|p| p.reason == ParkReason::Orphaned)
            .count() as u64,
        never_queried: persisted
            .iter()
            .filter(|r| r.last_queried_unix.is_none())
            .count() as u64,
    };
    debug_assert!(counts.orphaned <= counts.parked && counts.parked <= counts.registered);
    debug_assert!(counts.never_queried <= counts.registered);
    counts
}

#[cfg(test)]
#[path = "project_health_tests.rs"]
mod tests;
