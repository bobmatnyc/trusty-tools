//! Storage-layout choice and pre-flight for `POST /indexes` (#8147).
//!
//! Why: registration hardcoded the colocated layout, so a read-only or
//! root-owned root answered `500 corpus open failed …` and could never be
//! registered. The caller now chooses the layout, and a colocated request the
//! daemon cannot write is refused before anything is built or recorded.
//! What: [`resolve_layout`] reads `CreateIndexRequest::colocated`, deferring
//! to the layout an existing id already records; [`refuse_live_layout_change`]
//! does the same for a resident id; [`preflight_colocated_root`] proves
//! `<root>/.trusty-search/` is writable; [`registry_write_refusal`] makes a
//! failed `indexes.toml` write fatal for a data-dir index, which no
//! `roots.toml` scan can rediscover.
//! Test: `service::server::tests_8147`.

use std::io::ErrorKind;
use std::path::Path;

use axum::http::StatusCode;

use super::router::CreateIndexRequest;
use crate::core::registry::IndexHandle;
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::PersistedIndex;
use crate::service::storage_layout::{layout_of, StorageLayout};

/// The handler's error shape: a status and a JSON body.
pub(super) type Refusal = (StatusCode, serde_json::Value);

/// Resolve the storage layout a registration gets: `true` is colocated.
///
/// Why: `None` must keep the #403 default byte-identical for a new id. An
/// existing `<root>/.trusty-search/` is no reason to refuse `false`: since
/// #8438 every write path, and the reindex hash-cache decision, follows the
/// registry layout (the `403` below says to retry with `colocated=false`).
/// For an id the daemon already records, that record decides: a cold-parked
/// id misses the live-handle early return, and taking the layout from the
/// request built an empty corpus in the other layout and orphaned the real one.
/// What: `recorded` is the id's cold-store entry. When it names the same tree,
/// its `colocated` wins, and an explicit request that differs is the `409`
/// from [`layout_mismatch`]. An entry at another tree is the #3993
/// recreate-after-move path, which registers a new tree, so there the request
/// decides as for a new id: `None`/`true` → colocated, `false` → data dir.
/// Test: `create_index_omitted_colocated_keeps_the_colocated_default`,
/// `create_index_cold_data_dir_index_keeps_its_layout_when_colocated_is_omitted`,
/// `create_index_cold_index_refuses_an_explicit_layout_change`.
pub(super) fn resolve_layout(
    req: &CreateIndexRequest,
    recorded: Option<&PersistedIndex>,
) -> Result<bool, Refusal> {
    let recorded = recorded
        .filter(|entry| super::helpers::identifies_same_root(&entry.root_path, &req.root_path))
        .map(|entry| entry.colocated);
    match (req.colocated, recorded) {
        (Some(asked), Some(have)) if asked != have => Err(layout_mismatch(&req.id, have, asked)),
        (_, Some(have)) => Ok(have),
        (asked, None) => Ok(asked.unwrap_or(true)),
    }
}

/// Refuse a same-id, same-tree create whose explicit `colocated` differs from
/// the resident handle's layout (#8147).
///
/// Why: that request answered `200 created:false`, so the caller believed the
/// index used the layout it asked for.
/// What: `Ok` for an omitted or matching field; otherwise the `409` from
/// [`layout_mismatch`]. Takes a short indexer read lock, so the caller must
/// not hold an indexer guard.
/// Test: `create_index_live_index_refuses_an_explicit_layout_change`.
pub(super) async fn refuse_live_layout_change(
    req: &CreateIndexRequest,
    handle: &IndexHandle,
) -> Result<(), Refusal> {
    let Some(asked) = req.colocated else {
        return Ok(());
    };
    let have = layout_of(handle).await == StorageLayout::Colocated;
    if asked == have {
        return Ok(());
    }
    Err(layout_mismatch(&req.id, have, asked))
}

/// The `409` for a request that would change a registered id's layout.
fn layout_mismatch(id: &str, registered: bool, requested: bool) -> Refusal {
    let name = |colocated: bool| {
        if colocated {
            "colocated=true (<root>/.trusty-search/)"
        } else {
            "colocated=false (the daemon's data directory)"
        }
    };
    tracing::warn!(
        "create_index: refusing '{id}' — registered {}, request asked for {} (issue #8147)",
        name(registered),
        name(requested),
    );
    (
        StatusCode::CONFLICT,
        serde_json::json!({
            "error": format!(
                "index '{id}' is registered with {}; this request asked for {}. A \
                 registration never changes an existing index's storage layout: omit \
                 `colocated`, or delete the index and register it again",
                name(registered),
                name(requested),
            ),
            "id": id,
            "registered_colocated": registered,
            "requested_colocated": requested,
        }),
    )
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

/// Write `entry` to `indexes.toml` (#8147).
///
/// Why: a test that fails this write by planting a bad `indexes.toml` breaks
/// every concurrent test reading the process-global `TRUSTY_DATA_DIR`.
/// What: [`crate::service::persistence::upsert_index_registry_entry`]; in test
/// builds an id armed through [`registry_fault::RegistryFault`] fails instead
/// and touches no file.
/// Test: `create_index_colocated_false_with_an_unwritable_registry_is_a_500`.
pub(super) fn upsert_registry_entry(entry: PersistedIndex) -> anyhow::Result<()> {
    #[cfg(test)]
    if registry_fault::is_armed(&entry.id) {
        anyhow::bail!("injected indexes.toml write failure for '{}'", entry.id);
    }
    crate::service::persistence::upsert_index_registry_entry(entry)
}

/// Per-id fault seam for [`upsert_registry_entry`], test builds only.
#[cfg(test)]
pub(super) mod registry_fault {
    use std::collections::BTreeSet;
    use std::sync::{Mutex, MutexGuard};

    static ARMED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

    fn armed() -> MutexGuard<'static, BTreeSet<String>> {
        ARMED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(in crate::service::server) fn is_armed(id: &str) -> bool {
        armed().contains(id)
    }

    /// Fails every registry write for one id until dropped. Keyed by id, so a
    /// concurrent test registering another id is unaffected.
    pub(in crate::service::server) struct RegistryFault(String);

    impl RegistryFault {
        pub(in crate::service::server) fn arm(id: &str) -> Self {
            armed().insert(id.to_string());
            Self(id.to_string())
        }
    }

    impl Drop for RegistryFault {
        fn drop(&mut self) {
            armed().remove(&self.0);
        }
    }
}
