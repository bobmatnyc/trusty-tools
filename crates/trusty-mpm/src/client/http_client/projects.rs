//! Registry-B project methods for [`DaemonClient`] (DOC-35 §3.1/§4/§6, #2115/#2114).
//!
//! Why: the `tm projects list/register/show/status/config` CLI verbs are thin
//! HTTP clients (DOC-35 §1.3). Wiring each endpoint once here — as typed
//! [`DaemonClient`] methods — keeps every consumer (the CLI today, the TUI next)
//! off hand-rolled `reqwest` calls, exactly as the managed-session methods in the
//! sibling `managed.rs` do. This file lives beside `mod.rs` (not in it) so the
//! near-cap client `mod.rs` stays under the 500-SLOC bar; it reaches the
//! `base`/`http` fields because they are `pub(in crate::client::http_client)`.
//! What: the registry-B response DTOs the client owns ([`ProjectStatusWire`] and
//! its parts, plus [`RegisterProjectArgs`]/[`PatchProjectArgs`] for the
//! POST/PATCH bodies) and five methods: [`DaemonClient::registry_list_projects`],
//! [`DaemonClient::registry_register_project`], [`DaemonClient::registry_get_project`],
//! [`DaemonClient::registry_patch_project`], and [`DaemonClient::project_status`].
//! The status DTO deliberately does NOT `deny_unknown_fields` so the additive
//! Deliverable/Milestone histogram fields #2382 adds to the endpoint deserialize
//! cleanly against an older client (wire-compat, forward-tolerant).
//! Test: `project_status_wire_tolerates_additive_fields`,
//! `register_args_serializes_to_wire_shape`,
//! `patch_args_serializes_double_option_semantics`, `projects_list_deserializes`
//! in the `tests` submodule; live HTTP via the daemon's own route tests.

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::DaemonClient;
use super::error::response_or_body_error;
use crate::project::Project;

/// Serializable body for `POST /api/v1/projects` (registry-B register).
///
/// Why: `tm projects register` builds this from its flags; a typed struct keeps
/// the wire body checkable in a unit test rather than a hand-built `json!`.
/// What: mirrors the daemon's `RegisterProjectBody` — `name`/`repo_url` required,
/// the rest optional and omitted from the JSON when `None`.
/// Test: `register_args_serializes_to_wire_shape`.
#[derive(Debug, Clone, Serialize)]
pub struct RegisterProjectArgs {
    /// Registry key.
    pub name: String,
    /// Full repository URL.
    pub repo_url: String,
    /// Default branch; the daemon defaults to `"main"` when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// Free-form description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Classification tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Technology-stack hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_hint: Option<String>,
    /// Preferred `gh` login (#2081).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gh_user: Option<String>,
    /// GitHub account login pinned for this project's spawned sessions (#3025).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gh_account: Option<String>,
}

/// Serializable body for `PATCH /api/v1/projects/{name}` (registry-B partial
/// update, #2114, DOC-35 §6).
///
/// Why: `tm projects config <name> set/unset` and the TUI config form (#2120)
/// build this from their flags; mirrors the daemon's `PatchProjectBody` wire
/// shape exactly, including the double-`Option` unset story for
/// `description`/`stack_hint`/`gh_user` and the `tags_add`/`tags_remove`
/// add/remove pair (§6: "no free-text replace-whole-list footgun" — there is
/// deliberately no plain `tags` replacement field).
/// What: every field optional and omitted from the JSON when the OUTER
/// `Option` is `None` (`skip_serializing_if`); for the three double-`Option`
/// fields, `Some(None)` serializes as JSON `null` (serde's stock `Option<T>`
/// `Serialize` impl already does this — no custom serializer is needed on the
/// SEND side, only on the daemon's receive side), clearing the field, while
/// `Some(Some(v))` serializes as the value. `name` is intentionally NOT a
/// field here: the identity key is always the `{name}` path segment
/// [`DaemonClient::registry_patch_project`] takes separately, so this DTO's
/// shape cannot even express the (always-rejected) name-change attempt.
/// Test: `patch_args_serializes_double_option_semantics`,
/// `patch_args_omits_absent_fields`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PatchProjectArgs {
    /// New repository URL, if changing (rejected by the daemon when blank).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    /// New default branch, if changing (rejected by the daemon when blank).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// `None` = unchanged; `Some(None)` = clear; `Some(Some(v))` = set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    /// Same three-state semantics as `description`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_hint: Option<Option<String>>,
    /// Same three-state semantics as `description` (#2081; `gh auth status`
    /// validation is deferred to #2121).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gh_user: Option<Option<String>>,
    /// Same three-state semantics as `description`, for the pinned spawn-time
    /// `gh` account (#3025).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gh_account: Option<Option<String>>,
    /// Tags to add, deduplicated server-side against the current set. A
    /// blank/whitespace-only entry rejects the whole PATCH with 400.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags_add: Option<Vec<String>>,
    /// Tags to remove; applied AFTER `tags_add` server-side. Same
    /// blank-rejection rule as `tags_add`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags_remove: Option<Vec<String>>,
}

/// Per-state session histogram (client mirror of the daemon `SessionStateCounts`).
///
/// Why: the `tm projects status` rollup needs the per-state counts; every field
/// is `#[serde(default)]` so a daemon that renames or drops a state (unlikely,
/// but forward-safe) still deserializes.
/// What: one count per lifecycle state plus the total.
/// Test: `project_status_wire_tolerates_additive_fields`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionStateCountsWire {
    /// Sessions in `Provisioning`.
    #[serde(default)]
    pub provisioning: usize,
    /// Sessions in `Active`.
    #[serde(default)]
    pub active: usize,
    /// Sessions in `Stopped`.
    #[serde(default)]
    pub stopped: usize,
    /// Sessions in `Errored`.
    #[serde(default)]
    pub errored: usize,
    /// Sessions in `Decommissioned` (tombstones).
    #[serde(default)]
    pub decommissioned: usize,
    /// Total bound session records.
    #[serde(default)]
    pub total: usize,
}

/// Config-completeness flags (client mirror of the daemon `ProjectConfigFlags`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectConfigFlagsWire {
    /// Whether `gh_user` is set.
    #[serde(default)]
    pub gh_user_set: bool,
    /// Whether the per-project `gh` identity binding is set.
    #[serde(default)]
    pub github_binding_set: bool,
    /// Whether the pinned spawn-time `gh` account (#3025) is set.
    #[serde(default)]
    pub gh_account_set: bool,
}

/// Deserialized `GET /api/v1/projects/{name}/status` rollup.
///
/// Why: the daemon's `ProjectStatusResponse` is `Serialize`-only and lives in the
/// daemon module; the client needs its own `Deserialize` mirror. It is
/// intentionally forward-tolerant — no `deny_unknown_fields` — so the additive
/// `deliverables`/`milestones` histogram fields #2382 adds land in an older client
/// without error (the client simply ignores fields it does not model).
/// What: the project identity, the session histogram, the newest activity
/// timestamp, and the config flags.
/// Test: `project_status_wire_tolerates_additive_fields`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectStatusWire {
    /// Registered project name.
    #[serde(default)]
    pub project_name: String,
    /// Repository URL.
    #[serde(default)]
    pub repo_url: String,
    /// Per-state session histogram.
    #[serde(default)]
    pub sessions: SessionStateCountsWire,
    /// Most recent activity across the project's sessions, if any.
    #[serde(default)]
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Config-completeness flags.
    #[serde(default)]
    pub config: ProjectConfigFlagsWire,
}

/// Wire wrapper for `GET /api/v1/projects` (`{ projects, count }`).
#[derive(Debug, Deserialize)]
struct ProjectsListWire {
    #[serde(default)]
    projects: Vec<Project>,
}

impl DaemonClient {
    /// List registry-B projects via `GET /api/v1/projects`, optional tag filter.
    ///
    /// Why: backs `tm projects list [--tag <t>]` (#2115). Tag filtering is applied
    /// client-side (a pure `retain`) so the endpoint stays a plain list and the
    /// filter rule lives in one place.
    /// What: GETs the collection, returns the `projects` array; when `tag` is
    /// `Some`, keeps only projects whose `tags` contain it.
    /// Test: `projects_list_deserializes`; live HTTP via the daemon route tests.
    pub async fn registry_list_projects(&self, tag: Option<&str>) -> anyhow::Result<Vec<Project>> {
        let url = format!("{}/api/v1/projects", self.base);
        let resp = self.http.get(&url).send().await?;
        let body: ProjectsListWire = response_or_body_error(resp)
            .await?
            .json()
            .await
            .context("deserialize project list")?;
        let mut projects = body.projects;
        if let Some(tag) = tag {
            projects.retain(|p| p.tags.iter().any(|t| t == tag));
        }
        Ok(projects)
    }

    /// Register (idempotent upsert) a project via `POST /api/v1/projects`.
    ///
    /// Why: backs `tm projects register` (#2115). The daemon upserts on `name` and
    /// preserves any existing `gh`/commit identity binding.
    /// What: POSTs `args`, returns the stored [`Project`].
    /// Test: `register_args_serializes_to_wire_shape`; live HTTP via the daemon
    /// route tests.
    pub async fn registry_register_project(
        &self,
        args: &RegisterProjectArgs,
    ) -> anyhow::Result<Project> {
        let url = format!("{}/api/v1/projects", self.base);
        let resp = self.http.post(&url).json(args).send().await?;
        let project: Project = response_or_body_error(resp)
            .await?
            .json()
            .await
            .context("deserialize registered project")?;
        Ok(project)
    }

    /// Fetch one project via `GET /api/v1/projects/{name}`.
    ///
    /// Why: backs the config half of `tm projects show` (#2115).
    /// What: GETs the point-lookup, returns the [`Project`]; a 404 surfaces as an
    /// error via [`response_or_body_error`], carrying the daemon's body text.
    /// Test: live HTTP via the daemon route tests.
    pub async fn registry_get_project(&self, name: &str) -> anyhow::Result<Project> {
        let url = format!("{}/api/v1/projects/{name}", self.base);
        let resp = self.http.get(&url).send().await?;
        let project: Project = response_or_body_error(resp)
            .await?
            .json()
            .await
            .context("deserialize project")?;
        Ok(project)
    }

    /// Partially update a project via `PATCH /api/v1/projects/{name}`.
    ///
    /// Why: backs `tm projects config <name> set/unset` and the TUI config form
    /// (#2120) — both are thin clients over the daemon's single PATCH
    /// implementation (§6). See [`PatchProjectArgs`] for the field-by-field
    /// unset/tags contract.
    /// What: PATCHes `args` to the named project, returns the updated
    /// [`Project`]; a 404 (unknown project) or 400 (blank required field, or a
    /// body that tried to change `name`) surfaces as an [`anyhow::Error`] via
    /// [`response_or_body_error`] carrying the daemon's actual rejection
    /// message (#2485) — e.g. `"400 Bad Request: repo_url must not be
    /// empty"` — rather than a bare status line.
    /// Test: `patch_rejects_blank_repo_url_surfaces_server_message` in
    /// `tests/project_registry_routes.rs`; live HTTP via the other daemon
    /// route tests there.
    pub async fn registry_patch_project(
        &self,
        name: &str,
        args: &PatchProjectArgs,
    ) -> anyhow::Result<Project> {
        let url = format!("{}/api/v1/projects/{name}", self.base);
        let resp = self.http.patch(&url).json(args).send().await?;
        let project: Project = response_or_body_error(resp)
            .await?
            .json()
            .await
            .context("deserialize patched project")?;
        Ok(project)
    }

    /// Fetch the deterministic status rollup via `GET .../{name}/status`.
    ///
    /// Why: backs `tm projects status` (#2115). The [`ProjectStatusWire`] target is
    /// forward-tolerant of #2382's additive histogram fields.
    /// What: GETs the rollup, returns the parsed [`ProjectStatusWire`].
    /// Test: `project_status_wire_tolerates_additive_fields`; live HTTP via the
    /// daemon route tests.
    pub async fn project_status(&self, name: &str) -> anyhow::Result<ProjectStatusWire> {
        let url = format!("{}/api/v1/projects/{name}/status", self.base);
        let resp = self.http.get(&url).send().await?;
        let status: ProjectStatusWire = response_or_body_error(resp)
            .await?
            .json()
            .await
            .context("deserialize project status")?;
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status DTO ignores unknown fields, so #2382's additive
    /// `deliverables`/`milestones` histograms deserialize against this client.
    #[test]
    fn project_status_wire_tolerates_additive_fields() {
        let json = serde_json::json!({
            "project_name": "widget",
            "repo_url": "https://github.com/acme/widget",
            "sessions": { "provisioning": 1, "active": 2, "stopped": 0,
                          "errored": 0, "decommissioned": 1, "total": 4 },
            "last_activity_at": "2026-07-10T12:00:00Z",
            "config": { "gh_user_set": true, "github_binding_set": false },
            // Fields #2382 (Wave 2) adds — an older client must ignore them.
            "deliverables": { "proposed": 3, "complete": 1 },
            "milestones": { "in-progress": 1 }
        });
        let status: ProjectStatusWire = serde_json::from_value(json).unwrap();
        assert_eq!(status.project_name, "widget");
        assert_eq!(status.sessions.active, 2);
        assert_eq!(status.sessions.total, 4);
        assert!(status.config.gh_user_set);
        assert!(status.last_activity_at.is_some());
    }

    /// A status body missing the newer `config`/`last_activity_at` still parses
    /// (backward tolerance): every field defaults.
    #[test]
    fn project_status_wire_tolerates_missing_fields() {
        let json = serde_json::json!({
            "project_name": "widget",
            "repo_url": "https://github.com/acme/widget",
            "sessions": { "total": 0 }
        });
        let status: ProjectStatusWire = serde_json::from_value(json).unwrap();
        assert_eq!(status.sessions.total, 0);
        assert!(status.last_activity_at.is_none());
        assert!(!status.config.gh_user_set);
    }

    /// The register body omits absent optionals and keeps the required pair.
    #[test]
    fn register_args_serializes_to_wire_shape() {
        let args = RegisterProjectArgs {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: Some("develop".into()),
            description: None,
            tags: Some(vec!["backend".into()]),
            stack_hint: None,
            gh_user: None,
            gh_account: None,
        };
        let v = serde_json::to_value(&args).unwrap();
        assert_eq!(v["name"], "widget");
        assert_eq!(v["repo_url"], "https://github.com/acme/widget");
        assert_eq!(v["default_branch"], "develop");
        assert_eq!(v["tags"][0], "backend");
        // Absent optionals must not appear in the body.
        assert!(v.get("description").is_none());
        assert!(v.get("stack_hint").is_none());
        assert!(v.get("gh_user").is_none());
        assert!(v.get("gh_account").is_none());
    }

    /// The list wrapper deserializes the `{ projects, count }` shape and drops the
    /// extra `count` field the client does not model.
    #[test]
    fn projects_list_deserializes() {
        let json = serde_json::json!({
            "projects": [{
                "name": "widget",
                "repo_url": "https://github.com/acme/widget",
                "default_branch": "main"
            }],
            "count": 1
        });
        let body: ProjectsListWire = serde_json::from_value(json).unwrap();
        assert_eq!(body.projects.len(), 1);
        assert_eq!(body.projects[0].name, "widget");
    }

    /// `Some(None)` (clear) serializes as JSON `null`; `Some(Some(v))` (set)
    /// serializes as the value — the double-`Option` unset story the daemon's
    /// `deserialize_double_option` expects on the receiving end.
    #[test]
    fn patch_args_serializes_double_option_semantics() {
        let args = PatchProjectArgs {
            description: Some(None),
            stack_hint: Some(Some("rust".into())),
            ..Default::default()
        };
        let v = serde_json::to_value(&args).unwrap();
        assert!(v["description"].is_null(), "{v}");
        assert_eq!(v["stack_hint"], "rust");
    }

    /// `gh_account`'s double-`Option` set/clear semantics mirror `gh_user`'s
    /// (#3025).
    #[test]
    fn patch_args_serializes_gh_account_double_option() {
        let cleared = PatchProjectArgs {
            gh_account: Some(None),
            ..Default::default()
        };
        assert!(serde_json::to_value(&cleared).unwrap()["gh_account"].is_null());

        let set = PatchProjectArgs {
            gh_account: Some(Some("bobmatnyc".into())),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&set).unwrap()["gh_account"],
            "bobmatnyc"
        );
    }

    /// Every field defaults to `None` (outer), so a default `PatchProjectArgs`
    /// serializes to an empty object — a true "change nothing" PATCH body.
    #[test]
    fn patch_args_omits_absent_fields() {
        let args = PatchProjectArgs::default();
        let v = serde_json::to_value(&args).unwrap();
        assert_eq!(v, serde_json::json!({}));
    }

    /// `tags_add`/`tags_remove` and the two required-string fields serialize
    /// plainly when present.
    #[test]
    fn patch_args_serializes_tags_and_required_fields() {
        let args = PatchProjectArgs {
            repo_url: Some("https://github.com/acme/widget2".into()),
            default_branch: Some("release".into()),
            tags_add: Some(vec!["ml".into()]),
            tags_remove: Some(vec!["oss".into()]),
            ..Default::default()
        };
        let v = serde_json::to_value(&args).unwrap();
        assert_eq!(v["repo_url"], "https://github.com/acme/widget2");
        assert_eq!(v["default_branch"], "release");
        assert_eq!(v["tags_add"][0], "ml");
        assert_eq!(v["tags_remove"][0], "oss");
    }
}
