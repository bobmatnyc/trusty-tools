//! Storage-layout choice and pre-flight for `POST /indexes` (#8147).
//!
//! Why: registration hardcoded the colocated layout, so a read-only or
//! root-owned root answered `500 corpus open failed …` and could never be
//! registered. The caller now chooses the layout, and a colocated request the
//! daemon cannot write is refused before anything is built or recorded.
//! What: [`resolve_layout`] reads `CreateIndexRequest::colocated`;
//! [`preflight_colocated_root`] proves `<root>/.trusty-search/` is writable;
//! [`registry_write_refusal`] makes a failed `indexes.toml` write fatal for a
//! data-dir index, which no `roots.toml` scan can rediscover.
//! Test: `service::server::tests_8147`.

use std::io::ErrorKind;
use std::path::Path;

use axum::http::StatusCode;

use super::router::CreateIndexRequest;
use crate::service::colocated_storage::COLOCATED_DIR_NAME;

/// The handler's error shape: a status and a JSON body.
pub(super) type Refusal = (StatusCode, serde_json::Value);

/// Resolve the storage layout a registration asked for: `true` is colocated.
///
/// Why: `None` must keep the #403 default byte-identical. An existing
/// `<root>/.trusty-search/` is no reason to refuse `false`: since #8438 every
/// write path, and the reindex hash-cache decision, follows the registry
/// layout, and a `400` there left a root-owned directory with no way in (the
/// `403` below says to retry with `colocated=false`).
/// What: `None`/`true` → colocated; `false` → the data-dir store.
/// Test: `create_index_omitted_colocated_keeps_the_colocated_default`,
/// `create_index_colocated_false_over_a_read_only_colocated_dir_registers`.
pub(super) fn resolve_layout(req: &CreateIndexRequest) -> bool {
    req.colocated.unwrap_or(true)
}

/// Refuse a colocated registration whose `.trusty-search/` the daemon cannot
/// write, before the indexer is built or anything is registered.
///
/// Why: the builder's failure on such a root surfaced as a generic `500
/// corpus open failed`, which named neither the permission problem nor the
/// `colocated:false` way out.
/// What: creates `<root>/.trusty-search/` (the builder creates it next anyway)
/// or, when it exists, creates and removes a probe file inside it. A
/// permission or read-only-filesystem error is a `403` naming the directory
/// and the fix; any other error is left for the builder to report as before.
/// Test: `create_index_colocated_on_read_only_root_names_the_permission_problem`.
pub(super) fn preflight_colocated_root(id: &str, root: &Path) -> Result<(), Refusal> {
    let dir = root.join(COLOCATED_DIR_NAME);
    let probe = if dir.is_dir() {
        let file = dir.join(format!(".write-probe-{}", std::process::id()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .map(|_| {
                let _ = std::fs::remove_file(&file);
            })
    } else {
        std::fs::create_dir(&dir)
    };
    let Err(e) = probe else {
        return Ok(());
    };
    if !matches!(
        e.kind(),
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
    ) {
        return Ok(());
    }
    tracing::warn!(
        "create_index: refusing '{id}' — cannot write {}: {e} (issue #8147)",
        dir.display(),
    );
    Err((
        StatusCode::FORBIDDEN,
        serde_json::json!({
            "error": format!(
                "permission denied: the daemon cannot write {:?} ({e}). Register with \
                 colocated=false (POST /indexes or search.index.create only) to keep \
                 this index in the daemon's data directory instead.",
                dir.display()
            ),
            "id": id,
        }),
    ))
}

/// Refuse a data-dir registration whose `indexes.toml` row was not written.
///
/// Why: warm boot rediscovers a colocated index from `roots.toml`, but a
/// data-dir index exists only in `indexes.toml`. A warn-only failed write
/// answered `200` for an index that vanished on the next restart.
/// What: `None` for a colocated index, whose write stays best-effort. For a
/// data-dir index, a `500` naming the failure, returned before the handle is
/// registered, so nothing is left half-registered.
/// Test: `create_index_colocated_false_with_an_unwritable_registry_is_a_500`.
pub(super) fn registry_write_refusal(
    id: &str,
    colocated: bool,
    e: &anyhow::Error,
) -> Option<Refusal> {
    if colocated {
        return None;
    }
    tracing::error!(
        "create_index: refusing '{id}' — colocated=false and indexes.toml could not be \
         written ({e:#}); nothing else would restore it at boot (issue #8147)"
    );
    Some((
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({
            "error": format!(
                "could not record colocated=false index '{id}' in indexes.toml ({e:#}); \
                 nothing was registered, since the index would not survive a restart"
            ),
            "id": id,
        }),
    ))
}
