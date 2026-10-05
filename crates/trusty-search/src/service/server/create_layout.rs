//! Where `POST /indexes` places a new index's store, and the claim that makes
//! its root-collision check atomic (#8499).
//!
//! Why: #8499 moved new indexes out of the work tree into the per-id data dir.
//! Two registrations over one root no longer share a redb file, so redb's
//! single-open no longer catches the #2336 check-then-act race; a claim does.
//! Relocate takes the same claim for its new root (#8499 round 3). #8147 adds
//! the caller's `colocated: false` opt-out and its refusals.
//! Test: `service::server::tests_8499`,
//! `create_index_concurrent_same_root_only_one_wins`,
//! `relocate_races_into_one_root_register_exactly_once`,
//! `service::server::colocated_8147_tests`.

use super::router::CreateIndexRequest;
use crate::core::registry::IndexHandle;
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::PersistedIndex;
use crate::service::storage_layout::{is_write_refusal, layout_of, StorageLayout};
use axum::http::StatusCode;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::sync::Notify;

/// Registrations between their collision snapshot and their registry insert.
struct InFlight {
    /// Bumped on every insert and removal, so a claimer can tell whether the
    /// snapshot it checked is still the table it is about to join.
    generation: u64,
    next_token: u64,
    claims: Vec<(u64, String, PathBuf)>,
}

static IN_FLIGHT: Mutex<InFlight> = Mutex::new(InFlight {
    generation: 0,
    next_token: 0,
    claims: Vec::new(),
});
/// Woken whenever a claim is released.
static RELEASED: Notify = Notify::const_new();

fn in_flight() -> std::sync::MutexGuard<'static, InFlight> {
    // A panic while holding the guard cannot leave the Vec half-updated.
    IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner())
}

/// A held registration claim; dropping it releases the id and root.
pub(super) struct RegistrationClaim {
    token: u64,
}

impl Drop for RegistrationClaim {
    fn drop(&mut self) {
        let mut table = in_flight();
        table.claims.retain(|(t, _, _)| *t != self.token);
        table.generation += 1;
        drop(table);
        RELEASED.notify_waiters();
    }
}

/// True when two registrations must not run their check-then-insert at once:
/// the same id, the same tree (#2336), or nested trees (#4289).
fn claims_conflict(id_a: &str, root_a: &Path, id_b: &str, root_b: &Path) -> bool {
    id_a == id_b
        || trusty_common::index_id::identifies_same_path(root_a, root_b)
        || super::root_overlap::classify_root_overlap(root_a, root_b).is_some()
}

/// Claim `(id, root)` for a registration, waiting while a conflicting one is
/// in flight.
///
/// Why (#8499): a single process-wide lock held across the indexer build
/// serialized every `POST /indexes`. Only registrations that could collide —
/// same id, same tree, or nested trees — need to exclude each other.
/// What: snapshots the in-flight table, runs [`claims_conflict`] outside the
/// mutex (it stats paths), then inserts only if the table is unchanged since
/// the snapshot; otherwise retries. A conflicting claimer waits on
/// [`RELEASED`], armed before the snapshot so no release is missed.
/// Test: `unrelated_roots_register_concurrently`,
/// `same_root_race_registers_exactly_once`.
pub(super) async fn claim_registration(id: &str, root: &Path) -> RegistrationClaim {
    loop {
        let released = RELEASED.notified();
        tokio::pin!(released);
        released.as_mut().enable();
        let (generation, snapshot) = {
            let table = in_flight();
            let snapshot: Vec<(String, PathBuf)> = table
                .claims
                .iter()
                .map(|(_, i, r)| (i.clone(), r.clone()))
                .collect();
            (table.generation, snapshot)
        };
        let conflict = snapshot
            .iter()
            .any(|(other_id, other_root)| claims_conflict(id, root, other_id, other_root));
        if conflict {
            released.await;
            continue;
        }
        let mut table = in_flight();
        if table.generation != generation {
            continue;
        }
        let token = table.next_token;
        table.next_token += 1;
        table.generation += 1;
        table
            .claims
            .push((token, id.to_string(), root.to_path_buf()));
        return RegistrationClaim { token };
    }
}

/// The refusal for a registration whose freshly built indexer could not open
/// its redb corpus.
///
/// Why (#8499): a deferred embed job, or a delete whose close has not run,
/// can keep an earlier generation of the same index — and its redb file —
/// alive after the registration is gone. The re-registration then loses the
/// single open. That is transient, so answering `500` told a caller to give up
/// on something a retry fixes.
/// What: a transient kind (#4333 `Contention` or `OpenTimeout`) → `503
/// index_corpus_unavailable` with `index_id`, `failure_kind`, `transient` and
/// `retryable: true`, the index-scoped contract's shape. Any other kind keeps
/// the #2336 `500`, which also carries `failure_kind` (`null` when the open
/// failure was not classified). Nothing is registered either way.
/// Test: `re_register_while_an_earlier_handle_holds_the_store_is_retryable`,
/// `corpus_open_refusal_names_the_failure_kind_on_both_arms`.
pub(super) fn corpus_open_refusal(
    id: &str,
    root: &Path,
    kind: Option<crate::core::corpus::CorpusOpenFailure>,
) -> (StatusCode, serde_json::Value) {
    tracing::error!(
        "create_index: corpus open failed for '{id}' at {} ({kind:?}) — refusing to register \
         a broken index handle (#2336, #8499)",
        root.display()
    );
    let Some(kind) = kind.filter(|k| k.is_transient()) else {
        let error = format!(
            "corpus open failed for root_path {:?}; refusing to register a broken index handle",
            root.display()
        );
        // #8499: name the kind here too, as the 503 arm does.
        let failure_kind = kind.map(|k| k.label());
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": error, "failure_kind": failure_kind }),
        );
    };
    let message = format!(
        "the store of index '{id}' is still open under an earlier registration of that index \
         (a background embed pass, or a delete whose close has not finished); nothing was \
         registered — retry once it is released (#8499). {}",
        kind.stage_reason()
    );
    (
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({
            "error": "index_corpus_unavailable",
            "index_id": id,
            "failure_kind": kind.label(),
            "transient": true,
            "retryable": true,
            "message": message,
        }),
    )
}

/// The layout for a new registration, or the HTTP refusal.
///
/// Why (#8499): see [`StorageLayout::for_new_registration`]. Fail closed — a
/// store that would land in the repository is refused with `409`, never
/// redirected and never hidden by editing the tracked `.gitignore`. #8147: a
/// `colocated: false` request never adopts `<root>/.trusty-search/`, and an
/// adopted one the daemon cannot write is named, not reported as a corpus
/// open failure.
/// What: `colocated == Some(false)` →
/// [`StorageLayout::for_new_data_dir_registration`]; otherwise
/// [`StorageLayout::for_new_registration`]. `Ok(layout)`; a guard refusal →
/// `409`; any other failure → `500`, both naming #8499; a `Colocated` result
/// whose directory is unwritable → the `403` of [`preflight_colocated_root`].
/// Test: `create_refuses_when_the_store_would_land_in_the_work_tree`,
/// `colocated_false_over_a_read_only_colocated_corpus_registers_in_the_data_dir`,
/// `omitted_colocated_over_a_read_only_colocated_corpus_is_a_403`.
pub(super) fn registration_layout(
    id: &str,
    root: &Path,
    colocated: Option<bool>,
) -> Result<StorageLayout, Refusal> {
    // #8147: `colocated: false` skips adoption of an in-repo corpus.
    let resolved = if colocated == Some(false) {
        StorageLayout::for_new_data_dir_registration(id, root)
    } else {
        StorageLayout::for_new_registration(id, root)
    };
    let layout = resolved.map_err(|e| {
        let status = if is_write_refusal(&e) {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        tracing::error!("create_index: no safe store for '{id}': {e:#} (#8499)");
        let error =
            format!("no safe place for this index's store outside the work tree: {e:#} (#8499)");
        (status, serde_json::json!({ "error": error }))
    })?;
    if layout == StorageLayout::Colocated {
        preflight_colocated_root(id, root)?;
    }
    Ok(layout)
}

/// A refusal: the HTTP status and its JSON body.
pub(super) type Refusal = (StatusCode, serde_json::Value);

/// Refuse a colocated registration whose `<root>/.trusty-search/` the daemon
/// cannot write (#8147).
///
/// Why: the indexer build failed on such a directory with a generic `500
/// corpus open failed`, which named neither the permission problem nor the
/// `colocated: false` way out.
/// What: creates and removes a probe file in the directory. A permission or
/// read-only-filesystem error is a `403` whose `error` starts `permission
/// denied:` and names the directory and the opt-out. Any other error is left
/// for the indexer build to report, as before. Nothing is registered.
/// Test: `omitted_colocated_over_a_read_only_colocated_corpus_is_a_403`.
fn preflight_colocated_root(id: &str, root: &Path) -> Result<(), Refusal> {
    let dir = root.join(COLOCATED_DIR_NAME);
    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe);
    let e = match opened {
        Ok(_) => {
            if let Err(e) = std::fs::remove_file(&probe) {
                tracing::warn!("create_index: could not remove {}: {e}", probe.display());
            }
            return Ok(());
        }
        Err(e) => e,
    };
    if !matches!(
        e.kind(),
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
    ) {
        return Ok(());
    }
    tracing::warn!(
        "create_index: refusing '{id}' — cannot write {}: {e} (#8147)",
        dir.display()
    );
    let error = format!(
        "permission denied: the daemon cannot write {:?} ({e}). Register with \
         colocated=false to keep this index in the daemon's data directory and leave \
         that directory untouched (#8147)",
        dir.display()
    );
    Err((
        StatusCode::FORBIDDEN,
        serde_json::json!({ "error": error, "id": id }),
    ))
}

/// Refuse `colocated: false` for an id registered colocated at this root
/// (#8147).
///
/// Why: a registration never changes an existing index's layout. Answering
/// `200 created:false`, or building an empty data-dir store beside the
/// recorded colocated one, would tell the caller its opt-out took effect.
/// What: `Ok` unless the request says `colocated: false`. A `resident` handle
/// (already checked to be at this root) is judged by its live layout; with
/// none, the id's `indexes.toml` row is, since a lazy load can move an id
/// between the in-memory stores mid-request. A colocated row at another root
/// is the #3993 recreate-after-move case and does not conflict. An unreadable
/// registry is a `500`: the recorded layout is unknown. Takes a short indexer
/// read lock, so the caller must hold no indexer guard.
/// Test: `colocated_false_against_a_colocated_registration_is_a_409`,
/// `colocated_false_conflict_is_a_409_while_the_embedder_warms`,
/// `colocated_false_with_an_unreadable_registry_is_a_500`.
pub(super) async fn refuse_layout_change(
    req: &CreateIndexRequest,
    resident: Option<&IndexHandle>,
) -> Result<(), Refusal> {
    if req.colocated != Some(false) {
        return Ok(());
    }
    let registered_colocated = match resident {
        Some(handle) => layout_of(handle).await == StorageLayout::Colocated,
        None => find_registry_entry(&req.id)
            .map_err(|e| registry_read_refusal(&req.id, &e))?
            .is_some_and(|row| {
                row.colocated
                    && super::helpers::identifies_same_root(&row.root_path, &req.root_path)
            }),
    };
    if !registered_colocated {
        return Ok(());
    }
    tracing::warn!(
        "create_index: refusing '{}' — registered colocated, request asked for \
         colocated=false (#8147)",
        req.id
    );
    let error = format!(
        "index '{}' is registered with colocated=true (<root>/.trusty-search/); this \
         request asked for colocated=false. A registration never changes an existing \
         index's storage layout: omit `colocated`, or delete the index and register it \
         again (#8147)",
        req.id
    );
    Err((
        StatusCode::CONFLICT,
        serde_json::json!({
            "error": error,
            "id": req.id,
            "registered_colocated": true,
            "requested_colocated": false,
        }),
    ))
}

/// The `500` for a `colocated: false` create whose `indexes.toml` could not be
/// read (#8147).
fn registry_read_refusal(id: &str, e: &anyhow::Error) -> Refusal {
    tracing::error!("create_index: refusing '{id}' — indexes.toml unreadable: {e:#} (#8147)");
    let error = format!(
        "could not read indexes.toml to find the recorded storage layout of '{id}' ({e:#}); \
         nothing was registered. Fix or restore the registry file and retry (#8147)"
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({ "error": error, "id": id }),
    )
}

/// Read `id`'s `indexes.toml` row; in test builds an id armed through
/// `registry_fault::ReadFault` fails instead, so a test never plants a bad
/// file in the process-global `TRUSTY_DATA_DIR`.
fn find_registry_entry(id: &str) -> anyhow::Result<Option<PersistedIndex>> {
    #[cfg(test)]
    if registry_fault::is_armed(id) {
        anyhow::bail!("injected indexes.toml read failure for '{id}'");
    }
    crate::service::persistence::find_index_registry_entry(id)
}

/// Per-id `indexes.toml` read-fault seam for [`find_registry_entry`], test
/// builds only (#8147).
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

    /// Fails every registry read for one id until dropped.
    pub(in crate::service::server) struct ReadFault(String);

    impl ReadFault {
        pub(in crate::service::server) fn arm(id: &str) -> Self {
            armed().insert(id.to_string());
            Self(id.to_string())
        }
    }

    impl Drop for ReadFault {
        fn drop(&mut self) {
            armed().remove(&self.0);
        }
    }
}
