//! Storage-layout choice and pre-flight for `POST /indexes` (#8147).
//!
//! Why: registration hardcoded the colocated layout, so a read-only or
//! root-owned root answered `500 corpus open failed …` and could never be
//! registered. The caller now chooses the layout, and a colocated request the
//! daemon cannot write is refused before anything is built or recorded.
//! What: [`resolve_layout`] reads `CreateIndexRequest::colocated`;
//! [`preflight_colocated_root`] proves `<root>/.trusty-search/` is writable.
//! Test: `service::server::tests_8147`.

use std::io::ErrorKind;
use std::path::Path;

use axum::http::StatusCode;

use super::router::CreateIndexRequest;
use crate::service::colocated_storage::{has_colocated_storage, COLOCATED_DIR_NAME};

/// The handler's error shape: a status and a JSON body.
pub(super) type Refusal = (StatusCode, serde_json::Value);

/// Resolve the storage layout a registration asked for: `true` is colocated.
///
/// Why: `None` must keep the #403 default byte-identical, and `false` over a
/// root that already carries `.trusty-search/` stays ambiguous — the reindex
/// runner still probes that directory (`reindex/runner.rs`, the hash-cache
/// root-move decision), so the two layouts would disagree.
/// What: `None`/`true` → `Ok(true)`; `false` → `Ok(false)` unless the root has
/// colocated storage, which is a `400`.
/// Test: `create_index_refuses_colocated_false_over_existing_colocated_storage`,
/// `create_index_omitted_colocated_keeps_the_colocated_default`.
pub(super) fn resolve_layout(req: &CreateIndexRequest) -> Result<bool, Refusal> {
    let colocated = req.colocated.unwrap_or(true);
    if colocated || !has_colocated_storage(&req.root_path) {
        return Ok(colocated);
    }
    tracing::warn!(
        "create_index: refusing '{}' with colocated=false — {} already carries \
         .trusty-search/ (issue #8147)",
        req.id,
        req.root_path.display(),
    );
    Err((
        StatusCode::BAD_REQUEST,
        serde_json::json!({
            "error": format!(
                "colocated=false was requested but {:?} already contains .trusty-search/; \
                 the two storage layouts would disagree. Remove or move it aside, or \
                 register with colocated=true.",
                req.root_path.display()
            ),
            "id": req.id,
        }),
    ))
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
                 colocated=false to keep this index in the daemon's data directory \
                 instead.",
                dir.display()
            ),
            "id": id,
        }),
    ))
}
