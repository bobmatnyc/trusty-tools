//! Storage-layout choice and pre-flight for `POST /indexes` (#8147).
//!
//! Why: registration hardcoded the colocated layout, so a read-only or
//! root-owned root answered `500 corpus open failed …` and could never be
//! registered. The caller now chooses the layout, and a colocated request the
//! daemon cannot write is refused before anything is built or recorded.
//! What: [`resolve_layout_from_registry`] reads `CreateIndexRequest::colocated`,
//! deferring to the layout the id's `indexes.toml` row records; [`refuse_live_layout_change`]
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

/// Resolve a non-resident id's layout from its `indexes.toml` row (#8147).
///
/// Why: the row is the authority for an existing id. The cold store is not: a
/// lazy load between a live-registry miss and a cold-store read removes the
/// cold entry, both lookups miss, and the request picked the layout. The row
/// survives a cold→live load and also covers an id held in no in-memory store.
/// What: reads the row, then [`resolve_layout`]. A registry that cannot be
/// read refuses the create with a `500`; it never falls back to the request.
/// Test: `create_index_row_only_id_inherits_its_recorded_layout`,
/// `create_index_refuses_when_the_registry_cannot_be_read`.
pub(super) fn resolve_layout_from_registry(req: &CreateIndexRequest) -> Result<bool, Refusal> {
    let recorded = find_registry_entry(&req.id).map_err(|e| registry_read_refusal(&req.id, &e))?;
    resolve_layout(req, recorded.as_ref())
}

/// Resolve the storage layout a registration gets: `true` is colocated.
///
/// Why: `None` must keep the #403 default byte-identical for a new id. An
/// existing `<root>/.trusty-search/` is no reason to refuse `false`: since
/// #8438 every write path, and the reindex hash-cache decision, follows the
/// registry layout (the `403` below says to retry with `colocated=false`).
/// For an id the daemon already records, that record decides; taking the
/// layout from the request built an empty corpus in the other layout and
/// orphaned the real one.
/// What: `recorded` is the id's `indexes.toml` row. An omitted `colocated`
/// inherits its layout, even at another tree, as relocation does (#1089). At
/// the same tree an explicit value that differs is the `409` from
/// [`layout_mismatch`]; at another tree (#3993 recreate-after-move) an
/// explicit value decides. With no row: `None`/`true` → colocated.
/// Test: `resolve_layout_inherits_or_decides_per_request_and_record`,
/// `create_index_omitted_colocated_keeps_the_colocated_default`,
/// `create_index_cold_data_dir_index_keeps_its_layout_when_colocated_is_omitted`,
/// `create_index_cold_index_refuses_an_explicit_layout_change`.
pub(super) fn resolve_layout(
    req: &CreateIndexRequest,
    recorded: Option<&PersistedIndex>,
) -> Result<bool, Refusal> {
    let Some(entry) = recorded else {
        return Ok(req.colocated.unwrap_or(true));
    };
    let Some(asked) = req.colocated else {
        return Ok(entry.colocated);
    };
    let same_tree = super::helpers::identifies_same_root(&entry.root_path, &req.root_path);
    if same_tree && asked != entry.colocated {
        return Err(layout_mismatch(&req.id, entry.colocated, asked));
    }
    Ok(asked)
}

/// The `500` for a create whose `indexes.toml` could not be read (#8147).
fn registry_read_refusal(id: &str, e: &anyhow::Error) -> Refusal {
    tracing::error!(
        "create_index: refusing '{id}' — indexes.toml could not be read ({e:#}), so the \
         recorded storage layout is unknown (issue #8147)"
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({
            "error": format!(
                "could not read indexes.toml to find the recorded storage layout of '{id}' \
                 ({e:#}); nothing was registered. Fix or restore the registry file and retry"
            ),
            "id": id,
        }),
    )
}

/// Read `id`'s `indexes.toml` row; in test builds an id armed through
/// [`registry_fault::RegistryFault::arm_read`] fails instead.
fn find_registry_entry(id: &str) -> anyhow::Result<Option<PersistedIndex>> {
    #[cfg(test)]
    if registry_fault::is_read_armed(id) {
        anyhow::bail!("injected indexes.toml read failure for '{id}'");
    }
    crate::service::persistence::find_index_registry_entry(id)
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

/// Per-id fault seam for [`upsert_registry_entry`] and [`find_registry_entry`],
/// test builds only.
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

    /// Key for an armed read fault; a NUL cannot occur in an index id.
    fn read_key(id: &str) -> String {
        format!("read\0{id}")
    }

    pub(in crate::service::server) fn is_armed(id: &str) -> bool {
        armed().contains(id)
    }

    pub(in crate::service::server) fn is_read_armed(id: &str) -> bool {
        armed().contains(&read_key(id))
    }

    /// Fails every registry write (or, via `arm_read`, read) for one id until
    /// dropped. Keyed by id, so a concurrent test on another id is unaffected.
    pub(in crate::service::server) struct RegistryFault(String);

    impl RegistryFault {
        pub(in crate::service::server) fn arm(id: &str) -> Self {
            armed().insert(id.to_string());
            Self(id.to_string())
        }

        pub(in crate::service::server) fn arm_read(id: &str) -> Self {
            armed().insert(read_key(id));
            Self(read_key(id))
        }
    }

    impl Drop for RegistryFault {
        fn drop(&mut self) {
            armed().remove(&self.0);
        }
    }
}
