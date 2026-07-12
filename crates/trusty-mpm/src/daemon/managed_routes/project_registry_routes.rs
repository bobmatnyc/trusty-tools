//! Registry-B project HTTP surface (DOC-35 §4, #2114) — list / register / get /
//! patch.
//!
//! Why: `tm projects list/register/show` (#2115) and the `tm projects config
//! set/unset` field editor + TUI config form (#2120) are thin deterministic HTTP
//! clients (DOC-35 §1.3/§6) and need `GET/POST /api/v1/projects` and
//! `GET/PATCH /api/v1/projects/{name}` to call. Registry B (`crate::project`) was
//! until now reachable only via the MCP tools (`mcp_project.rs`) and the
//! `/status` rollup route; the plain-HTTP surface those verbs require did not
//! exist, so this module adds it. Each handler is a thin, deterministic
//! composition over [`crate::project::ProjectRegistry`] — no LLM, no cross-store
//! reasoning (§11) — mirroring the existing `mcp_project` bodies so the HTTP and
//! MCP surfaces stay behaviourally identical (idempotent upsert keyed on `name`,
//! preserving the per-project `gh`/commit identity binding across a re-register).
//! What: [`RegisterProjectBody`], [`PatchProjectBody`], [`ProjectsListResponse`],
//! and the four axum handlers [`list_projects_registry_route`],
//! [`register_project_registry_route`], [`get_project_registry_route`], and
//! [`patch_project_registry_route`] — the single validation/persistence
//! implementation §6 requires both the `tm projects config` CLI (#2120) and its
//! TUI form to share, so the two front ends can never drift on what a valid
//! field edit looks like.
//! Test: `register_body_deserializes`, `register_body_minimal`,
//! `patch_body_distinguishes_absent_null_and_value`, and the HTTP handler tests
//! in `tests/project_registry_routes.rs`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::project::{Project, ProjectStoreError};

/// JSON body for `POST /api/v1/projects` — an idempotent project upsert.
///
/// Why: registering a project from the deterministic CLI (#2115) mirrors the
/// `project_register` MCP tool's arguments so the two surfaces never drift. Only
/// `name` and `repo_url` are required; the rest are optional descriptive config.
/// The per-project `gh`/commit identity binding (#2184) is deliberately absent —
/// like `project_register`, a re-register PRESERVES any existing binding rather
/// than wiping it (operators set those via the config path).
/// What: the register arguments, all optional beyond `name`/`repo_url`, with
/// `default_branch` defaulting to `"main"` when omitted.
/// Test: `register_body_deserializes`, `register_body_minimal`.
#[derive(Debug, Deserialize)]
pub struct RegisterProjectBody {
    /// Registry key (non-empty).
    pub name: String,
    /// Full repository URL (non-empty).
    pub repo_url: String,
    /// Default branch; defaults to `"main"` when omitted.
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Free-form description.
    #[serde(default)]
    pub description: Option<String>,
    /// Classification tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Technology-stack hint (e.g. `rust`).
    #[serde(default)]
    pub stack_hint: Option<String>,
    /// Preferred `gh` login for this project (#2081).
    #[serde(default)]
    pub gh_user: Option<String>,
}

/// Response body for `GET /api/v1/projects`.
///
/// Why: mirrors the `project_list` MCP tool's `{ projects, count }` shape so a
/// consumer can read the inventory and its size in one call.
/// What: the registered projects plus their count.
/// Test: `tests/project_registry_routes.rs::register_get_list_status_round_trip`.
#[derive(Debug, Serialize)]
pub struct ProjectsListResponse {
    /// Every registered project.
    pub projects: Vec<Project>,
    /// `projects.len()`, surfaced so scripts need not count client-side.
    pub count: usize,
}

/// `GET /api/v1/projects` — list every registered registry-B project.
///
/// Why: backs `tm projects list` (#2115). A pure read over
/// [`ProjectRegistry::list`](crate::project::ProjectRegistry::list).
/// What: returns `{ projects, count }`; a store read failure degrades to 500 with
/// a logged warning rather than a panic (the library never `unwrap`s a store
/// result).
/// Test: `tests/project_registry_routes.rs::register_get_list_status_round_trip`.
pub async fn list_projects_registry_route(
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    let registry = state.project_registry().await;
    match registry.list().await {
        Ok(projects) => {
            let count = projects.len();
            Json(ProjectsListResponse { projects, count }).into_response()
        }
        Err(e) => {
            warn!(error = %e, "list_projects_registry_route: registry read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "project registry read failed".to_string(),
            )
                .into_response()
        }
    }
}

/// `POST /api/v1/projects` — register (idempotent upsert) a registry-B project.
///
/// Why: backs `tm projects register` (#2115). Mirrors the `project_register` MCP
/// tool: upsert keyed on `name`, preserving any existing `gh`/commit identity
/// binding so a plain re-register never silently wipes config set elsewhere.
/// What: rejects a blank `name`/`repo_url` with 400; builds a [`Project`]
/// (defaulting `default_branch` to `"main"`), preserves the existing binding,
/// persists via [`ProjectRegistry::register`](crate::project::ProjectRegistry::register),
/// and returns 201 with the stored record.
/// Test: `tests/project_registry_routes.rs::register_get_list_status_round_trip`,
/// `register_is_idempotent_upsert`, `register_preserves_identity_binding_not_expressible_in_body`.
pub async fn register_project_registry_route(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<RegisterProjectBody>,
) -> impl IntoResponse {
    if body.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "project name must not be empty").into_response();
    }
    if body.repo_url.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "repo_url must not be empty").into_response();
    }

    let registry = state.project_registry().await;
    // #2184 parity with `project_register`: a re-register REPLACES the record, so
    // carry any existing per-project `gh`/commit identity binding forward rather
    // than clobbering it (this route's body cannot express those fields).
    let existing = registry.get(&body.name).await.ok();
    let project = Project {
        name: body.name,
        repo_url: body.repo_url,
        default_branch: body.default_branch.unwrap_or_else(|| "main".to_string()),
        stack_hint: body.stack_hint,
        tags: body.tags.unwrap_or_default(),
        description: body.description,
        gh_user: body.gh_user,
        github: existing.as_ref().and_then(|p| p.github.clone()),
        commit_name: existing.as_ref().and_then(|p| p.commit_name.clone()),
        commit_email: existing.as_ref().and_then(|p| p.commit_email.clone()),
    };
    if let Err(e) = registry.register(project.clone()).await {
        warn!(error = %e, project = %project.name, "register_project_registry_route: register failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "project registry write failed".to_string(),
        )
            .into_response();
    }
    (StatusCode::CREATED, Json(project)).into_response()
}

/// `GET /api/v1/projects/{name}` — fetch one registry-B project by name.
///
/// Why: backs `tm projects show` (#2115) — the config half of the read-only
/// project view (the nested sessions half comes from the fleet endpoint, §3.1).
/// What: returns the project (200) or a 404 when unregistered; any other store
/// error degrades to 500 with a logged warning.
/// Test: `tests/project_registry_routes.rs::register_get_list_status_round_trip`,
/// `get_unknown_project_errors`.
pub async fn get_project_registry_route(
    State(state): State<Arc<DaemonState>>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let registry = state.project_registry().await;
    match registry.get(&name).await {
        Ok(project) => Json(project).into_response(),
        Err(ProjectStoreError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("project {name} not found")).into_response()
        }
        Err(e) => {
            warn!(error = %e, project = %name, "get_project_registry_route: registry read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "project registry read failed".to_string(),
            )
                .into_response()
        }
    }
}

/// Deserialize a `PATCH` field so "key absent" (leave unchanged, outer `None`)
/// is distinguishable from "key present with value `null`" (clear the field,
/// `Some(None)`) and "key present with a value" (`Some(Some(v))`, set it).
///
/// Why: serde's default `Option<Option<T>>` handling collapses JSON `null`
/// into the OUTER `None` — the same value `#[serde(default)]` produces for a
/// fully absent key — so a naive derive cannot express "explicitly clear this
/// field" separately from "don't touch it". This is the `double_option` idiom
/// `serde_with::rust::double_option` documents; trusty-tools already uses it
/// (without the `serde_with` dependency) in trusty-search's `PATCH /config`
/// (`crates/trusty-search/src/service/server/admin.rs`,
/// `deserialize_optional_option_u64`) — mirrored here for `Option<String>`
/// fields rather than adding a new dependency for one helper.
/// What: only invoked by serde when the JSON key IS present (the
/// `#[serde(default)]` on each field covers "key absent" by leaving the outer
/// `Option` at its `Default::default()`, i.e. `None`); wraps whatever
/// `Option<T>` the value deserializes to (`None` for `null`, `Some(v)`
/// otherwise) in an outer `Some`.
/// Test: `patch_body_distinguishes_absent_null_and_value`.
fn deserialize_double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// JSON body for `PATCH /api/v1/projects/{name}` — a field-level partial update
/// of a registry-B project (#2114, DOC-35 §6).
///
/// Why: `tm projects config <name> set/unset` and the TUI config form (#2120)
/// are both thin clients over this one endpoint (§6 RESOLVED), so every field
/// validation rule lives here exactly once. `name` is carried in the body only
/// so a client that echoes the full record back can be rejected with a clear
/// message rather than a confusing "field not recognised" — the identity key
/// comes from the URL path, never the body.
/// What: `name` — present and different from the path `{name}` is rejected
/// (400) by [`patch_project_registry_route`]; present and equal is a harmless
/// no-op. `repo_url`/`default_branch` are the two REQUIRED `String` fields on
/// [`Project`]: absent leaves them unchanged, present-and-blank is rejected
/// (400), present-and-non-blank replaces the value — there is deliberately no
/// "clear" story for either (§6: both are effectively non-empty-or-absent).
/// `description`/`stack_hint`/`gh_user` are the three `Option<String>` fields
/// on [`Project`]: they use [`deserialize_double_option`] so absent=unchanged,
/// JSON `null`=clear, a string=set — giving `tm projects config <name> unset
/// <field>` (#2120) a real wire mechanism (send `null`) rather than the
/// "clearing is unsupported in v1" limitation `PatchDeliverable`
/// (`daemon/api/deliverable_routes.rs`) documents for its single-`Option`
/// fields. `tags` deliberately does NOT accept a full-replacement array — §6
/// requires "`--add`/`--remove`, no free-text replace-whole-list footgun" — so
/// mutation goes through `tags_add`/`tags_remove` instead; `gh_user`'s `gh auth
/// status` validation is out of scope here (#2081/#2121, accepted as-is);
/// `jira_config` is NOT a field on this body (#2082/#2122 territory).
/// Test: `patch_body_distinguishes_absent_null_and_value`, and the HTTP tests
/// in `tests/project_registry_routes.rs`.
#[derive(Debug, Default, Deserialize)]
pub struct PatchProjectBody {
    /// Echoed identity key; must equal the path `{name}` if present at all.
    #[serde(default)]
    pub name: Option<String>,
    /// New repository URL, if changing (non-blank when present).
    #[serde(default)]
    pub repo_url: Option<String>,
    /// New default branch, if changing (non-blank when present).
    #[serde(default)]
    pub default_branch: Option<String>,
    /// New description; absent=unchanged, `null`=clear, string=set.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub description: Option<Option<String>>,
    /// New stack hint; absent=unchanged, `null`=clear, string=set.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub stack_hint: Option<Option<String>>,
    /// New preferred `gh` login (#2081); absent=unchanged, `null`=clear,
    /// string=set. Deep validation against `gh auth status` is deferred to
    /// #2121 — this endpoint accepts and stores the string as-is.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub gh_user: Option<Option<String>>,
    /// Tags to add, deduplicated against the current set. Each entry is
    /// trimmed; a blank/whitespace-only entry rejects the WHOLE request with
    /// 400 (see [`trim_and_reject_blank_tags`]).
    #[serde(default)]
    pub tags_add: Option<Vec<String>>,
    /// Tags to remove. Applied AFTER `tags_add`, so a tag present in both
    /// lists in the same request ends up removed. Same trim/blank-rejection
    /// rule as `tags_add`.
    #[serde(default)]
    pub tags_remove: Option<Vec<String>>,
}

/// `PATCH /api/v1/projects/{name}` — field-level partial update of a
/// registry-B project (#2114, DOC-35 §4/§6).
///
/// Why: backs `tm projects config <name> set/unset` and the TUI config form
/// (#2120) — both are thin clients over this ONE endpoint (§6 RESOLVED) so
/// validation and persistence never drift between the two front ends. See
/// [`PatchProjectBody`] for the full field-by-field unset/tags contract.
/// What: validates the body BEFORE touching the store (name-change rejection,
/// blank-value rejection on the two required string fields, and blank/
/// whitespace-only-tag rejection on `tags_add`/`tags_remove` — matching
/// [`register_project_registry_route`]'s "validate then touch the store"
/// order), then applies the merge under
/// [`ProjectRegistry::update_with`](crate::project::ProjectRegistry::update_with) — ONE
/// write-lock guard held across the whole fetch→mutate→persist sequence
/// (#2481 review HIGH fix: a `get` followed by a LATER, separate `register`
/// call left a window where a concurrent PATCH to a different field could be
/// silently discarded — see `update_with`'s doc comment for the full
/// mechanism). 404s an unknown project (same shape as
/// [`get_project_registry_route`]) and returns 200 with the updated record on
/// success. Re-issuing an identical PATCH is idempotent: the second call
/// applies the same (already-applied) values and tag adds/removes are
/// membership-checked, so it is a true no-op.
/// Test: `patch_body_distinguishes_absent_null_and_value` (in-module), and in
/// `tests/project_registry_routes.rs`: `patch_round_trip_updates_fields`,
/// `patch_absent_fields_untouched`, `patch_unknown_project_is_404`,
/// `patch_rejects_name_change`, `patch_is_idempotent`,
/// `patch_rejects_blank_tags_add`, `patch_rejects_blank_tags_remove`; and in
/// `crate::project::registry::tests`:
/// `update_with_serializes_concurrent_field_edits`,
/// `get_then_register_pattern_reproduces_lost_update_pre_fix`.
pub async fn patch_project_registry_route(
    State(state): State<Arc<DaemonState>>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<PatchProjectBody>,
) -> impl IntoResponse {
    if let Some(new_name) = &body.name
        && new_name.trim() != name
    {
        return (
            StatusCode::BAD_REQUEST,
            "project name is the identity key and cannot be changed via PATCH",
        )
            .into_response();
    }
    if let Some(repo_url) = &body.repo_url
        && repo_url.trim().is_empty()
    {
        return (StatusCode::BAD_REQUEST, "repo_url must not be empty").into_response();
    }
    if let Some(default_branch) = &body.default_branch
        && default_branch.trim().is_empty()
    {
        return (StatusCode::BAD_REQUEST, "default_branch must not be empty").into_response();
    }

    let tags_add = match body
        .tags_add
        .map(|tags| trim_and_reject_blank_tags(tags, "tags_add"))
    {
        Some(Ok(tags)) => Some(tags),
        Some(Err(msg)) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        None => None,
    };
    let tags_remove = match body
        .tags_remove
        .map(|tags| trim_and_reject_blank_tags(tags, "tags_remove"))
    {
        Some(Ok(tags)) => Some(tags),
        Some(Err(msg)) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        None => None,
    };
    let repo_url = body.repo_url;
    let default_branch = body.default_branch;
    let description = body.description;
    let stack_hint = body.stack_hint;
    let gh_user = body.gh_user;

    let registry = state.project_registry().await;
    let result = registry
        .update_with(&name, move |mut project| {
            if let Some(repo_url) = repo_url {
                project.repo_url = repo_url;
            }
            if let Some(default_branch) = default_branch {
                project.default_branch = default_branch;
            }
            if let Some(description) = description {
                project.description = description;
            }
            if let Some(stack_hint) = stack_hint {
                project.stack_hint = stack_hint;
            }
            if let Some(gh_user) = gh_user {
                project.gh_user = gh_user;
            }
            if let Some(add) = tags_add {
                for tag in add {
                    if !project.tags.contains(&tag) {
                        project.tags.push(tag);
                    }
                }
            }
            if let Some(remove) = tags_remove {
                project.tags.retain(|t| !remove.contains(t));
            }
            project
        })
        .await;

    match result {
        Ok(project) => Json(project).into_response(),
        Err(ProjectStoreError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("project {name} not found")).into_response()
        }
        Err(e) => {
            warn!(error = %e, project = %name, "patch_project_registry_route: registry write failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "project registry write failed".to_string(),
            )
                .into_response()
        }
    }
}

/// Trim every tag in a `tags_add`/`tags_remove` list and reject the whole
/// request (400) if any entry is blank/whitespace-only after trimming.
///
/// Why (#2481 review MEDIUM): `repo_url`/`default_branch` already reject a
/// blank value; `tags_add` previously pushed entries as-is with no trim/blank
/// check, so a blank tag would persist silently with a 200. Since this
/// endpoint is THE server-side validation surface #2120's CLI/TUI depend on,
/// silently accepting a blank tag would let a client-side typo (e.g. a
/// trailing comma in `--add a,b,`) persist unnoticed. Rejecting — rather than
/// silently dropping the blank entry — surfaces the mistake to the caller
/// immediately, matching the `repo_url`/`default_branch` precedent.
/// What: returns the trimmed tags on success, or an `Err(message)` naming
/// which field (`tags_add`/`tags_remove`) failed, suitable for a 400 body.
/// Test: `patch_rejects_blank_tags_add`, `patch_rejects_blank_tags_remove`
/// (`tests/project_registry_routes.rs`).
fn trim_and_reject_blank_tags(tags: Vec<String>, field: &str) -> Result<Vec<String>, String> {
    let mut trimmed = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim().to_string();
        if tag.is_empty() {
            return Err(format!(
                "{field} must not contain blank/whitespace-only tags"
            ));
        }
        trimmed.push(tag);
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full register body deserializes with every field populated.
    #[test]
    fn register_body_deserializes() {
        let json = serde_json::json!({
            "name": "widget",
            "repo_url": "https://github.com/acme/widget",
            "default_branch": "develop",
            "description": "the widget",
            "tags": ["backend", "oss"],
            "stack_hint": "rust",
            "gh_user": "acme-bot"
        });
        let body: RegisterProjectBody = serde_json::from_value(json).unwrap();
        assert_eq!(body.name, "widget");
        assert_eq!(body.repo_url, "https://github.com/acme/widget");
        assert_eq!(body.default_branch.as_deref(), Some("develop"));
        assert_eq!(
            body.tags.as_deref(),
            Some(&["backend".to_string(), "oss".to_string()][..])
        );
        assert_eq!(body.gh_user.as_deref(), Some("acme-bot"));
    }

    /// Only `name`/`repo_url` are required; the rest default to absent.
    #[test]
    fn register_body_minimal() {
        let json = serde_json::json!({
            "name": "minimal",
            "repo_url": "https://github.com/acme/minimal"
        });
        let body: RegisterProjectBody = serde_json::from_value(json).unwrap();
        assert_eq!(body.name, "minimal");
        assert!(body.default_branch.is_none());
        assert!(body.tags.is_none());
        assert!(body.stack_hint.is_none());
        assert!(body.gh_user.is_none());
    }

    /// The PATCH body's three-state fields (`description`/`stack_hint`/
    /// `gh_user`) distinguish "key absent" from "key present with `null`" from
    /// "key present with a value" — the double-`Option` wire idiom this
    /// endpoint's unset story depends on.
    #[test]
    fn patch_body_distinguishes_absent_null_and_value() {
        // Key entirely absent → outer None (leave unchanged).
        let absent: PatchProjectBody = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(absent.description, None);
        assert_eq!(absent.stack_hint, None);
        assert_eq!(absent.gh_user, None);

        // Key present, value null → Some(None) (clear).
        let cleared: PatchProjectBody = serde_json::from_value(serde_json::json!({
            "description": null,
            "stack_hint": null,
            "gh_user": null
        }))
        .unwrap();
        assert_eq!(cleared.description, Some(None));
        assert_eq!(cleared.stack_hint, Some(None));
        assert_eq!(cleared.gh_user, Some(None));

        // Key present, value a string → Some(Some(v)) (set).
        let set: PatchProjectBody = serde_json::from_value(serde_json::json!({
            "description": "new desc",
            "stack_hint": "python",
            "gh_user": "acme-bot"
        }))
        .unwrap();
        assert_eq!(set.description, Some(Some("new desc".to_string())));
        assert_eq!(set.stack_hint, Some(Some("python".to_string())));
        assert_eq!(set.gh_user, Some(Some("acme-bot".to_string())));
    }

    /// A `name` field identical to no path context still deserializes fine —
    /// the equality check against the path `{name}` happens in the handler,
    /// not in deserialization. `repo_url`/`default_branch`/`tags_add`/
    /// `tags_remove` all default to absent when omitted.
    #[test]
    fn patch_body_minimal_defaults_to_all_absent() {
        let body: PatchProjectBody = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(body.name.is_none());
        assert!(body.repo_url.is_none());
        assert!(body.default_branch.is_none());
        assert!(body.description.is_none());
        assert!(body.stack_hint.is_none());
        assert!(body.gh_user.is_none());
        assert!(body.tags_add.is_none());
        assert!(body.tags_remove.is_none());
    }
}
