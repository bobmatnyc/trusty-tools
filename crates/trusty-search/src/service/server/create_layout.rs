//! Where `POST /indexes` places a new index's store, and the lock that makes
//! its root-collision check atomic (#8499).
//!
//! Why: #8499 moved new indexes out of the work tree into the per-id data dir.
//! Two registrations over one root no longer share a redb file, so redb's
//! single-open no longer catches the #2336 check-then-act race; the lock does.
//! Test: `service::server::tests_8499`,
//! `create_index_concurrent_same_root_only_one_wins`.

use crate::service::storage_layout::{is_write_refusal, StorageLayout};
use axum::http::StatusCode;
use std::path::Path;

/// Serializes `create_index_report` from its root-collision snapshot to the
/// registry insert, so two racing creates over one root cannot both pass.
pub(super) static CREATE_REGISTRATION_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// The layout for a new registration, or the HTTP refusal.
///
/// Why (#8499): see [`StorageLayout::for_new_registration`]. Fail closed — a
/// store that would land in the repository is refused with `409`, never
/// redirected and never hidden by editing the tracked `.gitignore`.
/// What: `Ok(layout)`; a guard refusal → `409`; any other failure → `500`.
/// Both bodies carry an `error` naming #8499.
/// Test: `create_refuses_when_the_store_would_land_in_the_work_tree`.
pub(super) fn registration_layout(
    id: &str,
    root: &Path,
) -> Result<StorageLayout, (StatusCode, serde_json::Value)> {
    StorageLayout::for_new_registration(id, root).map_err(|e| {
        let status = if is_write_refusal(&e) {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        tracing::error!("create_index: no safe store for '{id}': {e:#} (#8499)");
        let error =
            format!("no safe place for this index's store outside the work tree: {e:#} (#8499)");
        (status, serde_json::json!({ "error": error }))
    })
}
