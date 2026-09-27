//! Where `POST /indexes` places a new index's store, and the claim that makes
//! its root-collision check atomic (#8499).
//!
//! Why: #8499 moved new indexes out of the work tree into the per-id data dir.
//! Two registrations over one root no longer share a redb file, so redb's
//! single-open no longer catches the #2336 check-then-act race; a claim does.
//! Relocate takes the same claim for its new root (#8499 round 3).
//! Test: `service::server::tests_8499`,
//! `create_index_concurrent_same_root_only_one_wins`,
//! `relocate_races_into_one_root_register_exactly_once`.

use crate::service::storage_layout::{is_write_refusal, StorageLayout};
use axum::http::StatusCode;
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
