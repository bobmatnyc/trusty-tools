//! `search.project.ensure`: find-or-create the one index of a tm project (#10531).
//!
//! Why: `project_register` created no index (#10529 epic, section 3). The index
//! came later, from whichever session launched first, under whichever id that
//! caller derived, and nothing refused a root nested inside another index's
//! (#6866 criterion 1: `cto-assistant` sits inside `cto`). tm needs one
//! idempotent call that leaves the project with exactly one pinned index.
//! What: [`ensure_project_report`] canonicalises and gates the root, then
//! (1) answers the index already registered at that exact tree, else
//! (2) refuses a root that contains or sits inside any registered root with the
//! overlapping id, else (3) creates the index under the id
//! [`trusty_common::index_id_for_root`] names (the registry-derived id when the
//! legacy basename is taken by another tree), pins its group, and queues the
//! first reindex. A found index queues no reindex, so a second call changes
//! nothing. #10532: the new index's excludes come from the checkout's
//! `.trusty-search.yaml`, else the request's `exclude` (the registry override),
//! else the `profile` (default `std-v1`); the response carries the stored
//! `name@hash` and `profile_applied: true` for the creating call.
//! Test: `project_ensure_tests.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use serde::Deserialize;

use super::helpers::{find_root_path_collision, validate_root_path};
use super::indexes::create_index_report;
use super::reindex_handlers::reindex_report;
use super::root_overlap::{
    find_root_overlap, overlap_check_failed_response, root_overlap_response,
};
use super::state::SearchAppState;

/// The params of `search.project.ensure`.
///
/// Why: tm hands over what it stored once at register (`project_key`, `name`)
/// and the checkout it resolved, so the daemon derives no identity of its own.
/// Test: `project_ensure_tests.rs`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnsureProjectRequest {
    /// `owner/repo` or `local:<uuid>`, assigned by tm at register.
    pub project_key: String,
    /// The tm registry name; only the label of a registry-derived id.
    pub name: String,
    /// Absolute path of the project checkout.
    pub root: PathBuf,
    /// Exclude profile name (#10532). Absent: `std-v1`; any other name is 400.
    #[serde(default)]
    pub profile: Option<String>,
    /// The registry override (`index.exclude`, #10532): used when the checkout
    /// has no `.trusty-search.yaml` exclude list, and ahead of the profile.
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
}

/// The `exclude` list of the checkout's `.trusty-search.yaml`, if it has one.
///
/// What: `(None, None)` for no file or no list; `(None, Some(why))` for a file
/// that does not parse or holds a bad glob, which the response reports and
/// which leaves the lower tiers to decide.
/// Test: `a_repo_yaml_exclude_beats_the_registry_override`.
fn repo_config_excludes(root: &std::path::Path) -> (Option<Vec<String>>, Option<String>) {
    match crate::core::project_config::ProjectConfig::load(root) {
        Ok(Some(cfg)) => match cfg.exclude {
            Some(list) => match crate::core::repo_config::validate_exclude_globs(&list) {
                Ok(()) => (Some(list), None),
                Err(e) => (None, Some(e)),
            },
            None => (None, None),
        },
        Ok(None) => (None, None),
        Err(e) => (None, Some(format!("{e:#}"))),
    }
}

/// A 4xx refusal for a malformed or unindexable request.
fn refuse(
    status: StatusCode,
    error: &str,
    detail: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut body = serde_json::json!({ "error": error });
    if let (Some(map), Some(extra)) = (body.as_object_mut(), detail.as_object()) {
        map.extend(extra.clone());
    }
    (status, body)
}

/// Is `id` registered, live or parked, at any tree?
fn id_in_use(
    handles: &[Arc<crate::core::registry::IndexHandle>],
    cold: &[crate::service::persistence::PersistedIndex],
    id: &str,
) -> bool {
    handles.iter().any(|h| h.id.0 == id) || cold.iter().any(|e| e.id == id)
}

/// The id to create for `req`: the naming id, else the registry-derived one.
///
/// Why: the naming id is the root basename until the registry id is adopted
/// (D7), so two projects named `app` collide; the digest id cannot.
/// What: `Err` carries the 409 when both are taken by other trees.
/// Test: `a_taken_basename_falls_back_to_the_registry_id`.
fn pick_id(
    req: &EnsureProjectRequest,
    root: &std::path::Path,
    handles: &[Arc<crate::core::registry::IndexHandle>],
    cold: &[crate::service::persistence::PersistedIndex],
) -> Result<String, (StatusCode, serde_json::Value)> {
    let project = trusty_common::RegisteredProject::new(&req.name, &req.project_key);
    let naming = trusty_common::index_id_for_root(root, Some(&project));
    if !naming.trim().is_empty() && !id_in_use(handles, cold, &naming) {
        return Ok(naming);
    }
    let derived = trusty_common::registry_id_for_root(root, Some(&project));
    if id_in_use(handles, cold, &derived) {
        return Err(refuse(
            StatusCode::CONFLICT,
            "project_id_taken",
            serde_json::json!({
                "message": format!("index id '{derived}' is registered at another tree"),
                "index_id": derived,
                "requested_root_path": root.display().to_string(),
            }),
        ));
    }
    Ok(derived)
}

/// The body `search.project.ensure` serves.
///
/// Why: see the module doc. Every refusal happens before anything registers.
/// What: `{index_id, project_key, created, root_path, reindex_queued,
/// profile, profile_applied, profile_source}` plus `reindex` or `reindex_error` when created,
/// and `project_map_error` when the pin could not be written (the index still
/// exists). A request with an empty key or name is 400; a home or filesystem
/// root is 409 `project_root_unindexable`; an overlapping root is 409
/// `project_root_overlap` naming `overlapping_index_id`; the root gate
/// (absolute, a directory, allowlist) is `validate_root_path`'s.
/// Test: `ensure_creates_pins_and_queues_one_reindex`,
/// `a_second_ensure_returns_the_same_id_and_queues_no_reindex`,
/// `a_root_inside_a_registered_root_is_refused_with_its_id`,
/// `a_root_enclosing_a_registered_root_is_refused_with_its_id`,
/// `an_unindexable_or_empty_request_is_refused`.
pub(crate) async fn ensure_project_report(
    state: &Arc<SearchAppState>,
    req: EnsureProjectRequest,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    if req.project_key.trim().is_empty() || req.name.trim().is_empty() {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "project_key and name must not be empty",
            serde_json::json!({}),
        ));
    }
    let root = validate_root_path(&req.root, false, &state.allowlist_paths).await?;
    if let Some(why) = trusty_common::refuse_unindexable_root(&root) {
        return Err(refuse(
            StatusCode::CONFLICT,
            "project_root_unindexable",
            serde_json::json!({
                "message": format!("{} is {why}, which names no project", root.display()),
                "requested_root_path": root.display().to_string(),
            }),
        ));
    }
    let handles = state.registry.list_handles();
    let cold = state.cold_store.snapshot();
    let found = find_root_path_collision(&handles, &cold, &root, None);
    let mut profile_note = None;
    let mut created_label: Option<String> = None;
    let (index_id, created) = match found {
        Some(existing) => (existing.0, false),
        None => {
            match find_root_overlap(&handles, &cold, &root, None) {
                Ok(None) => {}
                Ok(Some(conflict)) => {
                    tracing::warn!(
                        "project.ensure: refusing {} for '{}' — overlaps index '{}' at {} (#10531)",
                        root.display(),
                        req.project_key,
                        conflict.index_id,
                        conflict.root_path.display(),
                    );
                    let (status, mut body) = root_overlap_response(&conflict, &root);
                    body["message"] = body["error"].take();
                    body["error"] = "project_root_overlap".into();
                    body["overlapping_index_id"] = conflict.index_id.0.clone().into();
                    return Err((status, body));
                }
                Err(failure) => return Err(overlap_check_failed_response(&failure)),
            }
            let id = pick_id(&req, &root, &handles, &cold)?;
            // #10532: `.trusty-search.yaml`, then the registry override, then the
            // profile. A caller list reaches create as `exclude_globs`; with
            // none, create applies the profile itself.
            let (overrides, repo_config_error) = repo_config_excludes(&root);
            let resolved = crate::core::profile::resolve(
                overrides.as_deref(),
                req.exclude.as_deref(),
                req.profile.as_deref(),
            )
            .map_err(|e| {
                refuse(
                    StatusCode::BAD_REQUEST,
                    "unknown_profile",
                    serde_json::json!({ "message": e.to_string() }),
                )
            })?;
            let caller_globs = (resolved.source != crate::core::profile::Source::Profile)
                .then(|| resolved.globs.clone());
            profile_note = Some((resolved.source, repo_config_error));
            let create = serde_json::from_value(serde_json::json!({
                "id": id, "root_path": root,
                "exclude_globs": caller_globs, "profile": req.profile,
            }))
            .map_err(|e| {
                refuse(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ensure could not build a create request",
                    serde_json::json!({ "message": e.to_string() }),
                )
            })?;
            // #10531: a racer can register the same id and root after the snapshot
            // above; `created` is the daemon's word on who won, not this branch's.
            let registered = create_index_report(state, create).await?;
            created_label = registered
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let won = registered
                .get("created")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            (id, won)
        }
    };
    // Idempotent: pins only a group that has no pin yet.
    let project_map_error =
        crate::service::rpc::project_map::pin_after_create(state, &index_id).await;
    let mut body = serde_json::json!({
        "index_id": index_id,
        "project_key": req.project_key,
        "created": created,
        "root_path": root.display().to_string(),
        "reindex_queued": false,
        "profile": null,
        "profile_applied": false,
    });
    // The label the index carries (`std-v1@<hash>` or `custom@<hash>`). It is
    // "applied" only by the call that created the index.
    let label = created_label.or_else(|| {
        super::profile_apply::stored_label(state.registry_path_override.as_deref(), &index_id)
    });
    body["profile_applied"] = (created && label.is_some()).into();
    body["profile"] = label.into();
    if let Some((source, repo_config_error)) = profile_note {
        body["profile_source"] = format!("{source:?}").into();
        if let Some(e) = repo_config_error {
            body["repo_config_error"] = e.into();
        }
    }
    if let Some(e) = project_map_error {
        body["project_map_error"] = e.into();
    }
    if created {
        match reindex_report(state, &index_id, None).await {
            Ok(reindex) => {
                body["reindex_queued"] = true.into();
                body["reindex"] = reindex;
            }
            // The index exists and is pinned; the first reindex is reported, not fatal.
            Err((status, refusal)) => {
                tracing::warn!(
                    "project.ensure[{index_id}]: first reindex not queued ({status}): {refusal}"
                );
                body["reindex_error"] =
                    serde_json::json!({ "status": status.as_u16(), "body": refusal });
            }
        }
    }
    Ok(body)
}

#[cfg(test)]
#[path = "project_ensure_tests.rs"]
mod project_ensure_tests;
