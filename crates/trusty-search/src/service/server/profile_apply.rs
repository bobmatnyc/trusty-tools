//! Applying an exclude profile to an index: create, PATCH, purge, status (#10532).
//!
//! Why: the pure rules live in [`crate::core::profile`]; the daemon must turn
//! them into HTTP-shaped refusals, persist the stamp, and, when a PATCH
//! excludes files an index already holds, remove them without a rebuild.
//! What: [`resolve_create`] and [`resolve_patch`] map a request to a
//! [`ResolvedProfile`]; [`purge_newly_excluded`] removes the indexed files the
//! new globs exclude and the old ones did not, through the same batch removal
//! `search.index.files.remove` uses (#10110); [`stored_label`] reads the
//! stored `name@hash` for `index_status`.
//! Test: `service::server::profile_10532_tests`.

use std::collections::BTreeSet;

use axum::http::StatusCode;

use crate::core::path_selection::PathSelection;
use crate::core::profile::{self, ResolvedProfile};
use crate::core::registry::{IndexHandle, IndexId};

type Refusal = (StatusCode, serde_json::Value);

fn unknown_profile(e: &profile::UnknownProfile) -> Refusal {
    (
        StatusCode::BAD_REQUEST,
        serde_json::json!({ "error": "unknown_profile", "message": e.to_string() }),
    )
}

/// The profile a new index runs with: explicit globs, else the named profile.
///
/// What: `exclude_globs` is the caller's list (the CLI folds `.trusty-search.yaml`
/// and `--exclude` into it); an empty or absent list takes `profile`, default
/// `std-v1`. `400 unknown_profile` for any other name.
/// Test: `a_create_with_no_excludes_applies_std_v1`.
pub(crate) fn resolve_create(
    exclude_globs: Option<&[String]>,
    profile_name: Option<&str>,
) -> Result<ResolvedProfile, Refusal> {
    profile::resolve(None, exclude_globs, profile_name).map_err(|e| unknown_profile(&e))
}

/// What a PATCH does to the exclude list: nothing, or a new list with a stamp.
///
/// What: `profile` applies the named profile; `exclude_globs` stores exactly
/// that list as `custom`; both at once is `400 profile_and_exclude_globs`,
/// since each says what the whole list is.
/// Test: `patch_refuses_a_conflicting_pair_and_an_unknown_profile`.
pub(crate) fn resolve_patch(
    exclude_globs: Option<&[String]>,
    profile_name: Option<&str>,
) -> Result<Option<ResolvedProfile>, Refusal> {
    match (exclude_globs, profile_name) {
        (Some(_), Some(_)) => Err((
            StatusCode::BAD_REQUEST,
            serde_json::json!({
                "error": "profile_and_exclude_globs",
                "message": "send `profile` or `exclude_globs`, not both",
            }),
        )),
        (Some(globs), None) => Ok(Some(profile::custom(globs.to_vec()))),
        (None, Some(name)) => profile::resolve(None, None, Some(name))
            .map(Some)
            .map_err(|e| unknown_profile(&e)),
        (None, None) => Ok(None),
    }
}

/// `profile: name@hash` for an index, from its `indexes.toml` row.
///
/// What: `None` for an index persisted before profiles existed, or whose row
/// is missing or unreadable.
/// Test: `a_pre_profile_index_loads_with_no_profile`.
pub(crate) fn stored_label(
    registry_path: Option<&std::path::Path>,
    index_id: &str,
) -> Option<String> {
    let loaded = match registry_path {
        Some(path) => crate::service::persistence::load_index_registry_at(path),
        None => crate::service::persistence::load_index_registry(),
    };
    let row = loaded.ok()?.into_iter().find(|e| e.id == index_id)?;
    Some(profile::label(
        row.profile.as_deref()?,
        row.profile_hash.as_deref()?,
    ))
}

/// What a purge removed, or why it could not.
pub(crate) fn purge_body(result: &Result<(usize, usize), String>) -> serde_json::Value {
    match result {
        Ok((files, chunks)) => serde_json::json!({
            "files_removed": files, "chunks_removed": chunks, "rebuilt": false,
        }),
        Err(e) => serde_json::json!({ "error": e, "rebuilt": false }),
    }
}

/// Remove the indexed files `new_globs` exclude.
///
/// Why: a profile change must take effect on what the index already holds,
/// and a full rebuild of a large corpus to drop a record directory is the
/// cost #10110 removed.
/// What: lists the index's files (`indexed_file_set`), selects every file `new_globs` exclude (idempotent, so a repeat PATCH finishes a purge that failed part-way), and runs
/// the batch removal, the commit stamp and the hash forget of
/// `search.index.files.remove`. The caller holds the index's teardown guard.
/// Nothing selected touches nothing. Returns `(files, chunks)` removed.
/// Test: `a_profile_patch_removes_newly_excluded_files_and_does_not_rebuild`.
pub(crate) async fn purge_newly_excluded(
    handle: &IndexHandle,
    index_id: &IndexId,
    new_globs: &[String],
) -> Result<(usize, usize), String> {
    let indexer = handle.indexer.read().await;
    let files = indexer
        .indexed_file_set()
        .await
        .map_err(|e| format!("could not list indexed files: {e:#}"))?;
    // Stored keys are root-relative (`@root<n>/…` for an additional root; a
    // pre-#9510 key may be absolute). Match like the walker: relative to the root.
    let (primary, additional) = (&handle.root_path, &handle.additional_roots);
    let newly: BTreeSet<String> = files
        .into_iter()
        .filter(|f| {
            let abs = crate::core::index_roots::resolve_absolute(primary, additional, f);
            crate::service::index_admission::globs_exclude(primary, additional, new_globs, &abs)
        })
        .collect();
    if newly.is_empty() {
        return Ok((0, 0));
    }
    let selection = PathSelection::Files(newly);
    let counts = indexer
        .remove_files_matching(&selection)
        .await
        .map_err(|e| e.to_string())?;
    if counts.removed_chunks > 0 {
        indexer.record_incremental_commit("profile_purge").await;
    }
    indexer
        .forget_file_hashes_matching(index_id, &selection)
        .await
        .map_err(|e| format!("rows removed but content hashes not forgotten: {e:#}"))?;
    Ok((counts.removed_files, counts.removed_chunks))
}
