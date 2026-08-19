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

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::core::gh_account::{GH_DOCTOR_TIMEOUT, GhAuthProbe, probe_gh_auth};
use crate::core::trusty_tools_config::GithubConfig;
use crate::daemon::state::DaemonState;
use crate::project::{Project, ProjectStoreError};

/// JSON body for `POST /api/v1/projects` — an idempotent project upsert.
///
/// Why: registering a project from the deterministic CLI (#2115) mirrors the
/// `project_register` MCP tool's arguments so the two surfaces never drift. Only
/// `name` and `repo_url` are required; the rest are optional descriptive config.
/// A re-register PRESERVES any existing `github`/commit binding rather than
/// wiping it. `gh_config_dir` (#5851) is the one field that WRITES into that
/// binding, and only when present: `configured_account_pair` needs
/// `github.config_dir` and `github.account` set together, and before #5851
/// neither could be set from `register` at all.
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
    /// GitHub account login pinned for this project's spawned sessions,
    /// resolved into `GH_TOKEN`/`GH_USER` at spawn time (#3025).
    #[serde(default)]
    pub gh_account: Option<String>,
    /// Scoped `gh` config home for this project (#5851), merged into
    /// `github.config_dir` and injected as `GH_CONFIG_DIR` at spawn time.
    #[serde(default)]
    pub gh_config_dir: Option<PathBuf>,
    /// "Launch on main" opt-out (#3455): `None`/`Some(true)` → default
    /// worktree isolation; `Some(false)` → sessions run directly in the
    /// resolved local checkout, no worktree.
    #[serde(default)]
    pub worktree: Option<bool>,
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
///
/// #3025 review follow-up (item 4, MEDIUM — "no safe update path"): before this
/// fix, `stack_hint`/`tags`/`description`/`gh_user`/`gh_account` were REPLACED
/// wholesale from the body on every register call — an operator running
/// `tm projects register <name> --gh-account <login>` (the only way to pin an
/// account without knowing the PATCH API) on an ALREADY-configured project
/// silently wiped its description/tags/stack_hint/gh_user. Chosen fix (of the
/// two the review offered): merge-with-existing for these five optional fields
/// — an ABSENT body field now falls back to the CURRENT persisted value rather
/// than resetting it, matching the `github`/`commit_name`/`commit_email`
/// preservation this route already did (those simply have no body
/// representation at all). `default_branch` is deliberately UNCHANGED (still
/// defaults to `"main"` when absent) — it was not named in the review finding
/// and register's positional/required framing around it differs from the other
/// five. Explicit clearing of any of these five still works via PATCH's
/// double-Option `null` semantics (`ConfigField`/`ClearableField` remain
/// intentionally un-extended — see `project_config.rs`'s module doc for why
/// adding a `GhAccount` variant there would require matching TUI-form changes
/// out of scope for this fix).
/// What: rejects a blank `name`/`repo_url` with 400; builds a [`Project`]
/// (defaulting `default_branch` to `"main"`), preserving the CURRENT value of
/// `stack_hint`/`tags`/`description`/`gh_user`/`gh_account`/`github`/
/// `commit_name`/`commit_email` for every field the body omits, persists via
/// [`ProjectRegistry::register`](crate::project::ProjectRegistry::register),
/// and returns 201 with the stored record.
/// Test: `tests/project_registry_routes.rs::register_get_list_status_round_trip`,
/// `register_is_idempotent_upsert`, `register_preserves_identity_binding_not_expressible_in_body`,
/// `register_preserves_unspecified_optional_fields_on_existing_project`,
/// `register_explicit_fields_still_override_existing`.
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
    let existing = registry.get(&body.name).await.ok();
    let existing_github = existing.as_ref().and_then(|p| p.github.clone());
    // #5851: `--gh-config-dir` is the one register-time flag that writes INTO
    // the `github` binding, so both keys `configured_account_pair` requires can
    // be set in one command.
    let github = merge_gh_config_dir(
        existing_github,
        body.gh_config_dir.clone(),
        body.gh_account.as_deref(),
    );
    let project = Project {
        name: body.name,
        repo_url: body.repo_url,
        default_branch: body.default_branch.unwrap_or_else(|| "main".to_string()),
        stack_hint: body
            .stack_hint
            .or_else(|| existing.as_ref().and_then(|p| p.stack_hint.clone())),
        tags: body
            .tags
            .or_else(|| existing.as_ref().map(|p| p.tags.clone()))
            .unwrap_or_default(),
        description: body
            .description
            .or_else(|| existing.as_ref().and_then(|p| p.description.clone())),
        gh_user: body
            .gh_user
            .or_else(|| existing.as_ref().and_then(|p| p.gh_user.clone())),
        gh_account: body
            .gh_account
            .or_else(|| existing.as_ref().and_then(|p| p.gh_account.clone())),
        github,
        commit_name: existing.as_ref().and_then(|p| p.commit_name.clone()),
        commit_email: existing.as_ref().and_then(|p| p.commit_email.clone()),
        // #3455: mirrors the other optional fields' preserve-on-re-register
        // fix (#3025 review item 4) — an absent body field never resets an
        // existing worktree opt-out back to the default.
        worktree: body
            .worktree
            .or_else(|| existing.as_ref().and_then(|p| p.worktree)),
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

/// Fold a register-time `--gh-config-dir` into a project's `github` binding
/// (#5851).
///
/// Why: `configured_account_pair` needs `github.config_dir` AND
/// `github.account` together before any per-project `gh` enforcement or
/// selection happens, and before #5851 neither key had a `register` flag —
/// they could only be set by hand-editing config. Folding the flag in here
/// lets `tm projects register <name> --gh-account <login> --gh-config-dir
/// <dir>` arm both in one command.
/// What: `None` config dir → the existing binding, untouched (the
/// preserve-on-re-register contract every other optional field here follows).
/// `Some(dir)` → the existing binding with `config_dir` replaced, and
/// `account` filled from `gh_account` only when the binding does not already
/// name one — an explicitly configured `github.account` is never overwritten
/// by the coarser `gh_account` flag.
/// Test: `merge_gh_config_dir_absent_preserves`,
/// `merge_gh_config_dir_sets_both_keys`,
/// `merge_gh_config_dir_keeps_existing_account`.
fn merge_gh_config_dir(
    existing: Option<GithubConfig>,
    gh_config_dir: Option<PathBuf>,
    gh_account: Option<&str>,
) -> Option<GithubConfig> {
    let Some(dir) = gh_config_dir else {
        return existing;
    };
    let mut cfg = existing.unwrap_or_default();
    cfg.config_dir = Some(dir);
    if cfg
        .account
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
        && let Some(account) = gh_account.map(str::trim).filter(|s| !s.is_empty())
    {
        cfg.account = Some(account.to_string());
    }
    Some(cfg)
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
/// mutation goes through `tags_add`/`tags_remove` instead; setting `gh_user` is
/// additionally checked against `gh auth status` by
/// [`validate_gh_user_login`] (#2081/#2121); `jira_config` is NOT a field on
/// this body (#2082/#2122 territory).
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
    /// string=set. A `set` is validated against `gh auth status` before it is
    /// stored and rejected if `gh` does not report the login as logged in
    /// (#2121) — see [`validate_gh_user_login`].
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub gh_user: Option<Option<String>>,
    /// New pinned spawn-time `gh` account (#3025); absent=unchanged,
    /// `null`=clear, string=set. Resolved into `GH_TOKEN`/`GH_USER` at
    /// session spawn/relaunch — see [`crate::core::gh_account::resolve_gh_account_env`].
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub gh_account: Option<Option<String>>,
    /// "Launch on main" opt-out (#3455): absent=unchanged, `true`/`false`=set.
    /// Unlike the three `Option<String>` fields above this is a PLAIN
    /// `Option<bool>` (single, not double) — there is no separate "clear"
    /// story to express: the default state (`true`, worktree isolation ON)
    /// is reachable by simply setting the value back to `true`, mirroring
    /// `default_branch`'s "no clear story" precedent rather than adding an
    /// `Unset` variant for a field with only two meaningful states.
    #[serde(default)]
    pub worktree: Option<bool>,
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
/// `patch_rejects_blank_tags_add`, `patch_rejects_blank_tags_remove`; the
/// `gh_user` accept/reject arms are proved through
/// [`patch_project_registry_with_probe`] by
/// `patch_persists_a_gh_user_gh_reports_as_logged_in` and
/// `patch_rejects_an_unknown_gh_user_and_leaves_the_store_untouched`; and in
/// `crate::project::registry::tests`:
/// `update_with_serializes_concurrent_field_edits`,
/// `get_then_register_pattern_reproduces_lost_update_pre_fix`.
pub async fn patch_project_registry_route(
    State(state): State<Arc<DaemonState>>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<PatchProjectBody>,
) -> impl IntoResponse {
    // #2121: only a request that actually SETS `gh_user` pays for the `gh auth
    // status` subprocess — absent and `null` (clear) never run it. The probe
    // blocks its thread for up to GH_DOCTOR_TIMEOUT, so it runs off the async
    // executor.
    let probe = match body.gh_user.as_ref().and_then(|v| v.as_deref()) {
        Some(login) if !login.trim().is_empty() => Some(
            tokio::task::spawn_blocking(|| probe_gh_auth(GH_DOCTOR_TIMEOUT))
                .await
                .unwrap_or_else(|e| {
                    GhAuthProbe::Inconclusive(format!("the `gh auth status` probe failed: {e}"))
                }),
        ),
        _ => None,
    };
    patch_project_registry_with_probe(state, name, body, probe).await
}

/// [`patch_project_registry_route`]'s body, with the `gh auth status` answer
/// passed in rather than taken (#2121).
///
/// Why: the route's own validation must be testable without a live `gh` on the
/// machine running the suite, and the accept/reject arms are exactly the ones
/// worth proving end-to-end (does a rejected login really leave the store
/// untouched?). Taking [`GhAuthProbe`] as a parameter is the seam that makes
/// both arms hermetic, mirroring [`crate::core::gh_account::probe_gh_auth_with`].
/// What: `probe` is `None` when the request does not set `gh_user` (nothing to
/// verify); `Some(..)` carries whatever `gh` reported. Everything else is the
/// validate-then-touch-the-store sequence described on the route.
/// Test: `patch_persists_a_gh_user_gh_reports_as_logged_in`,
/// `patch_rejects_an_unknown_gh_user_and_leaves_the_store_untouched`.
pub(crate) async fn patch_project_registry_with_probe(
    state: Arc<DaemonState>,
    name: String,
    body: PatchProjectBody,
    probe: Option<GhAuthProbe>,
) -> Response {
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
    // #2121: verify a `gh_user` SET before anything touches the store; absent
    // and `null` (clear) pass straight through.
    let gh_user = match body.gh_user {
        Some(Some(login)) => match validate_gh_user_login(&login, probe.as_ref()) {
            Ok(canonical) => Some(Some(canonical)),
            Err((status, message)) => return (status, message).into_response(),
        },
        other => other,
    };

    let repo_url = body.repo_url;
    let default_branch = body.default_branch;
    let description = body.description;
    let stack_hint = body.stack_hint;
    let gh_account = body.gh_account;
    let worktree = body.worktree;

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
            if let Some(gh_account) = gh_account {
                project.gh_account = gh_account;
            }
            if let Some(worktree) = worktree {
                project.worktree = Some(worktree);
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

/// Accept a `gh_user` only when `gh auth status` reports it as logged in
/// (#2121), returning `gh`'s own spelling of the login.
///
/// Why: #2081 gave `Project::gh_user` a fail-loud contract at `gh`-call time,
/// but this endpoint stored whatever string it was handed. A typo'd or
/// not-yet-authenticated login therefore persisted with a 200 and surfaced
/// much later as a delegated-agent failure, far from the edit that caused it.
/// Checking at config-WRITE time moves that failure to the request that
/// introduces it. The `gh auth status` account list is the right authority
/// because it is the only source that sees every auth mode — keyring,
/// `hosts.yml`, and an env token, which writes no config file at all (#5032).
/// What: rejects a blank login with 400 (`null` is the documented way to clear
/// the field, so blank is a mistake rather than an intent). With a definitive
/// answer from `gh`, returns the canonical spelling of a logged-in login, or
/// 400 naming the field, the rejected login, and the logins that would be
/// accepted. An INCONCLUSIVE probe is 503, never 400 and never a silent
/// accept: "`gh` says no" and "I could not ask `gh`" are different facts
/// (#5032), and only the first is the caller's mistake — but neither is
/// grounds to store an unverified login.
/// Test: `validate_gh_user_accepts_a_logged_in_login`,
/// `validate_gh_user_rejects_an_unknown_login`,
/// `validate_gh_user_rejects_a_blank_login`,
/// `validate_gh_user_rejects_a_host_with_no_accounts`,
/// `validate_gh_user_is_503_when_gh_could_not_answer`.
fn validate_gh_user_login(
    login: &str,
    probe: Option<&GhAuthProbe>,
) -> Result<String, (StatusCode, String)> {
    let login = login.trim();
    if login.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "gh_user must not be blank — send null to clear it".to_string(),
        ));
    }
    match probe {
        Some(GhAuthProbe::Answered(status)) => match status.canonical_logged_in_login(login) {
            Some(canonical) => Ok(canonical),
            None if status.logged_in.is_empty() => Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "gh_user must name an account `gh auth status` reports as logged in, but it \
                     reports none — run `gh auth login` before setting gh_user to '{login}'"
                ),
            )),
            None => Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "gh_user must name an account `gh auth status` reports as logged in; \
                     '{login}' is not one of: {}",
                    status.logged_in.join(", ")
                ),
            )),
        },
        Some(GhAuthProbe::Inconclusive(why)) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("gh_user '{login}' could not be verified against `gh auth status`: {why}"),
        )),
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "gh_user '{login}' could not be verified: no `gh auth status` answer was \
                 available for this request"
            ),
        )),
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
    use std::path::Path;

    use super::*;
    use crate::core::gh_account::GhAccountStatus;

    /// The two-account answer a real `gh auth status` gives on this machine.
    fn two_accounts() -> GhAuthProbe {
        GhAuthProbe::Answered(GhAccountStatus {
            active: Some("bob-duetto".to_string()),
            logged_in: vec!["bob-duetto".to_string(), "bobmatnyc".to_string()],
        })
    }

    /// Why (#2121): the accepted arm — a login `gh` reports as logged in is
    /// stored, in `gh`'s spelling rather than the caller's casing.
    /// Test: itself.
    #[test]
    fn validate_gh_user_accepts_a_logged_in_login() {
        assert_eq!(
            validate_gh_user_login("BobMatNyc", Some(&two_accounts())).unwrap(),
            "bobmatnyc"
        );
        // A logged-in but non-active account is a legitimate choice.
        assert_eq!(
            validate_gh_user_login(" bob-duetto ", Some(&two_accounts())).unwrap(),
            "bob-duetto"
        );
    }

    /// Why (#2121): the rejected arm — the 400 must name the field, the
    /// rejected login, and what would have been accepted, so the operator can
    /// fix the typo without reading the daemon's source.
    /// Test: itself.
    #[test]
    fn validate_gh_user_rejects_an_unknown_login() {
        let (status, message) =
            validate_gh_user_login("typo-bot", Some(&two_accounts())).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(message.contains("gh_user"), "{message}");
        assert!(message.contains("typo-bot"), "{message}");
        assert!(message.contains("bobmatnyc"), "{message}");
    }

    /// Why (#2121): `null` is the documented way to clear `gh_user`, so an
    /// empty or whitespace-only string is a client mistake, not an unset.
    /// Test: itself.
    #[test]
    fn validate_gh_user_rejects_a_blank_login() {
        for blank in ["", "   "] {
            let (status, message) =
                validate_gh_user_login(blank, Some(&two_accounts())).unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(message.contains("null"), "{message}");
        }
    }

    /// Why (#2121): on a host where `gh` is installed but nobody is logged in,
    /// "not one of: " with an empty list would be a useless message.
    /// Test: itself.
    #[test]
    fn validate_gh_user_rejects_a_host_with_no_accounts() {
        let probe = GhAuthProbe::Answered(GhAccountStatus::default());
        let (status, message) = validate_gh_user_login("bobmatnyc", Some(&probe)).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(message.contains("gh auth login"), "{message}");
    }

    /// Why (#5032/#2121): "`gh` says no" and "I could not ask `gh`" are
    /// different facts. An unanswerable probe must not be reported as a bad
    /// login (400) and must not be stored either.
    /// Test: itself.
    #[test]
    fn validate_gh_user_is_503_when_gh_could_not_answer() {
        let probe = GhAuthProbe::Inconclusive("`gh` could not be run".to_string());
        let (status, message) = validate_gh_user_login("bobmatnyc", Some(&probe)).unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(message.contains("could not be verified"), "{message}");

        let (status, _) = validate_gh_user_login("bobmatnyc", None).unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Seed an isolated daemon holding one registered project.
    async fn state_with_widget() -> (Arc<DaemonState>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state = Arc::new(DaemonState::with_root_isolated_managed(tmp.path().to_owned()).await);
        state
            .project_registry()
            .await
            .register(Project {
                name: "widget".into(),
                repo_url: "https://github.com/acme/widget".into(),
                default_branch: "main".into(),
                stack_hint: None,
                tags: vec![],
                description: None,
                gh_user: Some("bob-duetto".into()),
                gh_account: None,
                github: None,
                commit_name: None,
                commit_email: None,
                worktree: None,
            })
            .await
            .expect("seed register");
        (state, tmp)
    }

    /// Why (#2121): the accepted arm end-to-end — a verified login reaches the
    /// store, in `gh`'s spelling.
    /// Test: itself.
    #[tokio::test]
    async fn patch_persists_a_gh_user_gh_reports_as_logged_in() {
        let (state, _tmp) = state_with_widget().await;
        let resp = patch_project_registry_with_probe(
            Arc::clone(&state),
            "widget".into(),
            PatchProjectBody {
                gh_user: Some(Some("BobMatNyc".into())),
                ..Default::default()
            },
            Some(two_accounts()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let stored = state
            .project_registry()
            .await
            .get("widget")
            .await
            .expect("stored");
        assert_eq!(stored.gh_user.as_deref(), Some("bobmatnyc"));
    }

    /// Why (#2121): the rejected arm end-to-end — the defect this fixes was
    /// store-and-continue, so the assertion that matters is that the persisted
    /// record is byte-identical to what it was before the request.
    /// Test: itself.
    #[tokio::test]
    async fn patch_rejects_an_unknown_gh_user_and_leaves_the_store_untouched() {
        let (state, _tmp) = state_with_widget().await;
        let before = state
            .project_registry()
            .await
            .get("widget")
            .await
            .expect("seeded");

        let resp = patch_project_registry_with_probe(
            Arc::clone(&state),
            "widget".into(),
            PatchProjectBody {
                // A description edit rides along to prove the WHOLE request is
                // rejected, not just the offending field.
                description: Some(Some("should not persist".into())),
                gh_user: Some(Some("typo-bot".into())),
                ..Default::default()
            },
            Some(two_accounts()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let after = state
            .project_registry()
            .await
            .get("widget")
            .await
            .expect("still there");
        assert_eq!(after, before);
    }

    /// Why (#2121): clearing with `null`, and every request that does not
    /// mention `gh_user`, must stay free of any `gh auth status` requirement —
    /// so no probe is taken and none is needed.
    /// Test: itself.
    #[tokio::test]
    async fn patch_clears_gh_user_without_a_probe() {
        let (state, _tmp) = state_with_widget().await;
        let resp = patch_project_registry_with_probe(
            Arc::clone(&state),
            "widget".into(),
            PatchProjectBody {
                gh_user: Some(None),
                ..Default::default()
            },
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state
                .project_registry()
                .await
                .get("widget")
                .await
                .expect("stored")
                .gh_user,
            None
        );
    }

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
            "gh_user": "acme-bot",
            "gh_account": "acme-bot",
            "gh_config_dir": "/home/bob/.config/gh-acme"
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
        assert_eq!(body.gh_account.as_deref(), Some("acme-bot"));
        assert_eq!(
            body.gh_config_dir.as_deref(),
            Some(Path::new("/home/bob/.config/gh-acme"))
        );
    }

    /// Why: an absent `--gh-config-dir` must leave an existing `github`
    /// binding exactly as it was — the preserve-on-re-register contract every
    /// other optional field on this route follows (#3025 review item 4).
    /// Test: itself.
    #[test]
    fn merge_gh_config_dir_absent_preserves() {
        let existing = GithubConfig {
            config_dir: Some(PathBuf::from("/keep/me")),
            account: Some("keeper".to_string()),
            ..GithubConfig::default()
        };
        let merged = merge_gh_config_dir(Some(existing.clone()), None, Some("other"));
        assert_eq!(merged, Some(existing));
        assert_eq!(merge_gh_config_dir(None, None, Some("other")), None);
    }

    /// Why (#5851): `configured_account_pair` only arms when BOTH
    /// `github.config_dir` and `github.account` are set, and neither had a
    /// `register` flag before this change. One command must set both.
    /// Test: itself.
    #[test]
    fn merge_gh_config_dir_sets_both_keys() {
        let merged = merge_gh_config_dir(None, Some(PathBuf::from("/gh/acme")), Some("acme-bot"))
            .expect("some");
        assert_eq!(merged.config_dir.as_deref(), Some(Path::new("/gh/acme")));
        assert_eq!(merged.account.as_deref(), Some("acme-bot"));
    }

    /// Why: an explicitly configured `github.account` is the more specific
    /// statement of intent; the coarser `gh_account` flag must never overwrite
    /// it, only fill the gap when it is absent or blank.
    /// Test: itself.
    #[test]
    fn merge_gh_config_dir_keeps_existing_account() {
        let existing = GithubConfig {
            account: Some("configured".to_string()),
            ..GithubConfig::default()
        };
        let merged = merge_gh_config_dir(
            Some(existing),
            Some(PathBuf::from("/gh/acme")),
            Some("flag-account"),
        )
        .expect("some");
        assert_eq!(merged.account.as_deref(), Some("configured"));
        assert_eq!(merged.config_dir.as_deref(), Some(Path::new("/gh/acme")));
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
        assert!(body.gh_account.is_none());
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
        assert_eq!(absent.gh_account, None);

        // Key present, value null → Some(None) (clear).
        let cleared: PatchProjectBody = serde_json::from_value(serde_json::json!({
            "description": null,
            "stack_hint": null,
            "gh_user": null,
            "gh_account": null
        }))
        .unwrap();
        assert_eq!(cleared.description, Some(None));
        assert_eq!(cleared.stack_hint, Some(None));
        assert_eq!(cleared.gh_user, Some(None));
        assert_eq!(cleared.gh_account, Some(None));

        // Key present, value a string → Some(Some(v)) (set).
        let set: PatchProjectBody = serde_json::from_value(serde_json::json!({
            "description": "new desc",
            "stack_hint": "python",
            "gh_user": "acme-bot",
            "gh_account": "acme-bot"
        }))
        .unwrap();
        assert_eq!(set.description, Some(Some("new desc".to_string())));
        assert_eq!(set.stack_hint, Some(Some("python".to_string())));
        assert_eq!(set.gh_user, Some(Some("acme-bot".to_string())));
        assert_eq!(set.gh_account, Some(Some("acme-bot".to_string())));
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
        assert!(body.gh_account.is_none());
        assert!(body.tags_add.is_none());
        assert!(body.tags_remove.is_none());
    }
}
