//! `skills.*` JSON-RPC methods — the daemon's SKILL CATALOG management
//! surface (issue #3449), backing the Foundry GUI's Skills management tab.
//!
//! Why: the GUI's Agents tab (`crate::agents::protocol`) and Skills tab are
//! siblings in the same issue, but tcode's skill model is genuinely
//! narrower than its agent model — worth stating explicitly rather than
//! papering over with a symmetric-looking API that implies a tier that
//! doesn't exist. `crate::skills::discover_skill_metadata` (the ONLY
//! consumer of a project's `.claude/skills/`, wired at `run_task`/
//! `task::executor` time) recognises exactly TWO tiers: the embedded/bundled
//! catalog (`crate::assets::DEFAULT_SKILLS`, trusty-mpm's universal set
//! minus `tm-*` orchestration skills) and ONE project-scoped disk directory
//! (`<project_root>/.claude/skills/<name>/SKILL.md`) — there is NO
//! user-level `~/.claude/skills` tier the way `crate::agents::agents_dir`
//! has for agents (`skills::locate_skills_dir` takes a `project_root`
//! directly; every call site passes the bound project's root, never
//! `$HOME`). This module's `tier` values are therefore `"bundled"` and
//! `"project"` ONLY — never `"user"` — and `skills.create`/`skills.delete`
//! require an actually-bound project (there is nowhere else on disk for a
//! projectless daemon to persist a user-added skill); a projectless caller
//! gets a clear `-32003 invalid_argument`, not a silent no-op or an
//! invented user-level path nothing else in this crate would ever read.
//! What: [`register`] wires three methods onto a shared, immutable
//! [`SkillsCatalogState`] (an `Option<PathBuf>` project skills dir — `None`
//! when projectless):
//!   - `skills.list` -> `{"skills": [{name, tier, description}]}`, the union
//!     of the bundled catalog (`tier: "bundled"`) and, when a project is
//!     bound, its disk skills (`tier: "project"`) — a disk skill overriding
//!     a bundled name WINS, mirroring `discover_skill_metadata`'s disk-wins
//!     precedence (see that function's docs) exactly.
//!   - `skills.create{name, content}` -> writes
//!     `<skills_dir>/<name>/SKILL.md` from the caller-supplied
//!     Markdown+frontmatter body. Same name-validation contract as
//!     `agents::protocol::validate_agent_name` (shared, not reimplemented —
//!     see that function's docs): rejects a name colliding with a bundled
//!     skill (`-32001 permission_denied`, 403) or an already-existing disk
//!     skill directory (`-32008`-shaped conflict, 409). `-32003
//!     invalid_argument` (400) when projectless.
//!   - `skills.delete{name}` -> removes `<skills_dir>/<name>/` recursively.
//!     `-32002 not_found` (404) absent; `-32001 permission_denied` (403) for
//!     a bundled name; `-32003 invalid_argument` (400) when projectless.
//!
//! Test: `tests::*`.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::agents::protocol::validate_agent_name as validate_skill_name;
use crate::jsonrpc::{ConnectionContext, Router, RpcError};

use super::SkillMetadata;

/// Shared, immutable state every `skills.*` handler in this module closes
/// over: the project-scoped skills directory, or `None` when projectless.
///
/// Why: unlike [`crate::agents::protocol::AgentsCatalogState`], there is no
/// user-level fallback directory for skills (see module docs) — `None` is a
/// real, valid state (a projectless daemon can still LIST the bundled
/// catalog), not a placeholder for "not wired up yet".
#[derive(Clone)]
pub struct SkillsCatalogState {
    /// `<project_root>/.claude/skills`, or `None` when no project is bound.
    pub dir: Option<PathBuf>,
}

impl SkillsCatalogState {
    /// Build state from an optional bound project root.
    ///
    /// Why: keeps "resolve the skills dir from the binding, or None" in one
    /// place (today: `crate::serve::build_router_at`).
    /// What: `Some(super::locate_skills_dir(root))` when `project_root` is
    /// `Some`, else `None`.
    pub fn new(project_root: Option<&std::path::Path>) -> Self {
        Self {
            dir: project_root.map(super::locate_skills_dir),
        }
    }
}

/// Register `skills.list`, `skills.create`, `skills.delete` onto `router`.
///
/// Why: mirrors `crate::agents::protocol::register`'s shape for this
/// namespace.
/// Test: `tests::register_wires_all_three_methods`.
pub fn register(router: &mut Router, state: SkillsCatalogState) {
    let s = state.clone();
    router.register(
        "skills.list",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { skills_list(&s, params, ctx).await }
        },
    );

    let s = state.clone();
    router.register(
        "skills.create",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { skills_create(&s, params, ctx).await }
        },
    );

    let s = state;
    router.register(
        "skills.delete",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { skills_delete(&s, params, ctx).await }
        },
    );
}

/// Wire shape for one catalog entry.
fn entry_json(m: &SkillMetadata, tier: &str) -> Value {
    json!({ "name": m.name, "tier": tier, "description": m.description })
}

/// `skills.list` -> the full bundled ∪ project-disk catalog.
///
/// Why: the GUI's Skills tab roster fetch.
/// What: ignores `params`. Builds the bundled set keyed by name (always
/// present, even projectless), then overlays project-disk entries when
/// `state.dir` is `Some` — a disk override suppresses the bundled entry of
/// the same name (mirrors `discover_skill_metadata`'s disk-wins rule).
/// Sorted by name.
/// Test: `tests::list_returns_bundled_when_projectless`,
/// `tests::list_disk_override_wins_and_suppresses_bundled`,
/// `tests::list_disk_only_entries_are_additive`.
async fn skills_list(
    state: &SkillsCatalogState,
    _params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let mut by_name: Vec<(String, Value)> = super::embedded_skill_metadata()
        .iter()
        .map(|m| (m.name.clone(), entry_json(m, "bundled")))
        .collect();

    if let Some(dir) = &state.dir {
        for m in super::discover_disk_skill_metadata(dir) {
            let entry = entry_json(&m, "project");
            match by_name.iter_mut().find(|(n, _)| *n == m.name) {
                Some(slot) => slot.1 = entry,
                None => by_name.push((m.name.clone(), entry)),
            }
        }
    }

    by_name.sort_by(|a, b| a.0.cmp(&b.0));
    let skills: Vec<Value> = by_name.into_iter().map(|(_, v)| v).collect();
    Ok(json!({ "skills": skills }))
}

/// `params` shape for `skills.create`.
#[derive(Deserialize)]
struct CreateParams {
    name: String,
    content: String,
}

/// `skills.create` -> write `<skills_dir>/<name>/SKILL.md` from `content`.
///
/// Why: the GUI's Skills tab add-flow.
/// What: `{"name", "tier": "project"}` on success. `-32003 invalid_argument`
/// when projectless (nowhere to write); invalid `name`; embedded/bundled
/// name collision (`-32001`, 403); existing disk skill directory (`-32008`
/// shaped conflict, 409).
/// Test: `tests::create_writes_file_and_returns_project_tier`,
/// `tests::create_rejects_when_projectless`,
/// `tests::create_rejects_bundled_name_collision`,
/// `tests::create_rejects_existing_disk_skill`.
async fn skills_create(
    state: &SkillsCatalogState,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: CreateParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("skills.create: {e}")))?;
    validate_skill_name(&p.name)?;

    let Some(dir) = &state.dir else {
        return Err(RpcError::invalid_argument(
            "skills are project-scoped; bind a project before adding one",
        ));
    };

    if super::embedded_skill_metadata()
        .iter()
        .any(|m| m.name == p.name)
    {
        return Err(RpcError::permission_denied(format!(
            "'{}' is a bundled skill and cannot be overridden by name via this endpoint",
            p.name
        )));
    }

    let skill_dir = dir.join(&p.name);
    let skill_md = skill_dir.join("SKILL.md");
    if skill_md.exists() {
        return Err(
            RpcError::new(-32008, format!("skill '{}' already exists on disk", p.name))
                .with_data(json!({"error_type": "already_exists"})),
        );
    }

    std::fs::create_dir_all(&skill_dir)
        .map_err(|e| RpcError::internal(format!("creating skill dir: {e}")))?;
    std::fs::write(&skill_md, &p.content)
        .map_err(|e| RpcError::internal(format!("writing SKILL.md: {e}")))?;

    Ok(json!({ "name": p.name, "tier": "project" }))
}

/// `params` shape for `skills.delete`.
#[derive(Deserialize)]
struct DeleteParams {
    name: String,
}

/// `skills.delete` -> remove `<skills_dir>/<name>/` recursively.
///
/// Why: the GUI's Skills tab remove-flow.
/// What: `{}` on success. `-32003 invalid_argument` when projectless;
/// `-32002 not_found` (404) when no disk skill directory exists under
/// `name` and it is not bundled; `-32001 permission_denied` (403) for a
/// bundled name.
/// Test: `tests::delete_removes_disk_skill_dir`,
/// `tests::delete_rejects_when_projectless`,
/// `tests::delete_missing_name_returns_not_found`,
/// `tests::delete_bundled_name_returns_permission_denied`.
async fn skills_delete(
    state: &SkillsCatalogState,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: DeleteParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("skills.delete: {e}")))?;
    validate_skill_name(&p.name)?;

    let Some(dir) = &state.dir else {
        return Err(RpcError::invalid_argument(
            "skills are project-scoped; bind a project before deleting one",
        ));
    };

    let skill_dir = dir.join(&p.name);
    if !skill_dir.exists() {
        if super::embedded_skill_metadata()
            .iter()
            .any(|m| m.name == p.name)
        {
            return Err(RpcError::permission_denied(format!(
                "'{}' is a bundled skill and cannot be deleted",
                p.name
            )));
        }
        return Err(RpcError::not_found(format!(
            "no disk skill named '{}'",
            p.name
        )));
    }

    std::fs::remove_dir_all(&skill_dir)
        .map_err(|e| RpcError::internal(format!("deleting skill dir: {e}")))?;
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ConnectionContext {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    #[test]
    fn locate_skills_dir_matches_project_only_tier() {
        // Pins the module doc's central claim: no user-level fallback.
        let root = std::path::Path::new("/fake/project");
        let s = SkillsCatalogState::new(Some(root));
        assert_eq!(s.dir, Some(root.join(".claude").join("skills")));
        let projectless = SkillsCatalogState::new(None);
        assert_eq!(projectless.dir, None);
    }

    #[tokio::test]
    async fn list_returns_bundled_when_projectless() {
        let s = SkillsCatalogState::new(None);
        let result = skills_list(&s, Value::Null, ctx()).await.expect("list");
        let skills = result["skills"].as_array().expect("array");
        assert_eq!(skills.len(), crate::assets::DEFAULT_SKILLS.len());
        assert!(skills.iter().all(|sk| sk["tier"] == "bundled"));
    }

    #[tokio::test]
    async fn list_disk_override_wins_and_suppresses_bundled() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bundled_name = crate::assets::DEFAULT_SKILLS[0].name;
        let dir = tmp.path().join(bundled_name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {bundled_name}\ndescription: DISK OVERRIDE\n---\n\nBody.\n"),
        )
        .expect("write");

        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let result = skills_list(&s, Value::Null, ctx()).await.expect("list");
        let skills = result["skills"].as_array().expect("array");
        let matches: Vec<_> = skills
            .iter()
            .filter(|sk| sk["name"] == bundled_name)
            .collect();
        assert_eq!(matches.len(), 1, "must appear exactly once");
        assert_eq!(matches[0]["tier"], "project");
        assert_eq!(matches[0]["description"], "DISK OVERRIDE");
    }

    #[tokio::test]
    async fn list_disk_only_entries_are_additive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("my-custom-skill");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: my-custom-skill\ndescription: Custom\n---\n\nBody.\n",
        )
        .expect("write");

        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let result = skills_list(&s, Value::Null, ctx()).await.expect("list");
        let skills = result["skills"].as_array().expect("array");
        assert_eq!(skills.len(), crate::assets::DEFAULT_SKILLS.len() + 1);
        assert!(
            skills
                .iter()
                .any(|sk| sk["name"] == "my-custom-skill" && sk["tier"] == "project")
        );
    }

    #[tokio::test]
    async fn create_writes_file_and_returns_project_tier() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let result = skills_create(
            &s,
            json!({"name": "my-skill", "content": "---\nname: my-skill\n---\n\nBody.\n"}),
            ctx(),
        )
        .await
        .expect("create");
        assert_eq!(result["name"], "my-skill");
        assert_eq!(result["tier"], "project");
        assert!(tmp.path().join("my-skill").join("SKILL.md").exists());
    }

    #[tokio::test]
    async fn create_rejects_when_projectless() {
        let s = SkillsCatalogState::new(None);
        let err = skills_create(&s, json!({"name": "my-skill", "content": "x"}), ctx())
            .await
            .expect_err("must reject projectless create");
        assert_eq!(err.code, -32003);
    }

    #[tokio::test]
    async fn create_rejects_bundled_name_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bundled_name = crate::assets::DEFAULT_SKILLS[0].name;
        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let err = skills_create(&s, json!({"name": bundled_name, "content": "x"}), ctx())
            .await
            .expect_err("must reject bundled collision");
        assert_eq!(err.code, -32001);
    }

    #[tokio::test]
    async fn create_rejects_existing_disk_skill() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("dup-skill");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("SKILL.md"), "---\nname: dup-skill\n---\n\nBody.\n")
            .expect("write");

        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let err = skills_create(&s, json!({"name": "dup-skill", "content": "new"}), ctx())
            .await
            .expect_err("must reject existing disk skill");
        assert_eq!(err.code, -32008);
    }

    #[tokio::test]
    async fn delete_removes_disk_skill_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("SKILL.md"), "---\nname: my-skill\n---\n\nBody.\n").expect("write");

        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        skills_delete(&s, json!({"name": "my-skill"}), ctx())
            .await
            .expect("delete");
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn delete_rejects_when_projectless() {
        let s = SkillsCatalogState::new(None);
        let err = skills_delete(&s, json!({"name": "my-skill"}), ctx())
            .await
            .expect_err("must reject projectless delete");
        assert_eq!(err.code, -32003);
    }

    #[tokio::test]
    async fn delete_missing_name_returns_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let err = skills_delete(&s, json!({"name": "totally-bogus"}), ctx())
            .await
            .expect_err("must 404");
        assert_eq!(err.code, -32002);
    }

    #[tokio::test]
    async fn delete_bundled_name_returns_permission_denied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bundled_name = crate::assets::DEFAULT_SKILLS[0].name;
        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let err = skills_delete(&s, json!({"name": bundled_name}), ctx())
            .await
            .expect_err("must 403");
        assert_eq!(err.code, -32001);
    }

    #[tokio::test]
    async fn register_wires_all_three_methods() {
        use trusty_common::mcp::Request;

        let mut router = Router::new();
        register(&mut router, SkillsCatalogState::new(None));

        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(Value::from(1)),
            method: "skills.list".to_string(),
            params: None,
        };
        let resp = router.dispatch(req, &ctx()).await;
        assert!(resp.result.is_some(), "skills.list must be wired");
        assert!(resp.result.unwrap()["skills"].is_array());
    }
}
