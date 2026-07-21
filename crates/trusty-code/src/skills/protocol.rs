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
//!   - `skills.list` -> `{"skills": [{name, tier, description}]}` — WHOLE-
//!     CATALOG REPLACEMENT, mirroring `discover_skill_metadata`'s own
//!     `if !skills.is_empty() { return skills }` threshold (see that
//!     function's docs) exactly, at the catalog level rather than per-name:
//!     when a project is bound and its disk skills directory has at least
//!     one entry, the response is ONLY those disk skills (`tier:
//!     "project"`) — the bundled catalog is entirely absent, because that
//!     is what actually resolves at `task.run` time (`FsSkillResolver`
//!     discards the whole bundled set the instant disk has anything).
//!     Otherwise (projectless, or a bound project with no/empty disk
//!     skills) the response is the full bundled catalog (`tier: "bundled"`).
//!     A per-name bundled-∪-disk overlay would be WRONG here — it would
//!     report bundled skills as available for a project whose real
//!     `use_skill` catalog resolves only its own custom entries
//!     (code-critic PR #3465 re-review, MEDIUM).
//!   - `skills.create{name, content}` -> writes
//!     `<skills_dir>/<name>/SKILL.md` from the caller-supplied
//!     Markdown+frontmatter body. Same name-validation contract as
//!     `agents::protocol::validate_agent_name` (shared, not reimplemented —
//!     see that function's docs): rejects a name colliding with a bundled
//!     skill (`-32001 permission_denied`, 403) or an already-existing disk
//!     skill (`-32009 already_exists`, 409). `-32003
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

/// `skills.list` -> the catalog that will ACTUALLY resolve for this project
/// at `task.run` time.
///
/// Why: the GUI's Skills tab roster fetch. The runtime resolver
/// (`discover_skill_metadata`, wired through `FsSkillResolver` at
/// `task::executor::daily_driver_skills_catalog`) does WHOLE-CATALOG
/// REPLACEMENT, not a per-name overlay: the instant a project's
/// `.claude/skills/` has even one entry, the ENTIRE bundled catalog is
/// discarded — every bundled name becomes unresolvable
/// (`skills::SkillError::Unknown` on `use_skill`), disk or not. An earlier
/// version of this handler built a bundled ∪ disk union with per-name
/// override (disk winning only for names it actually shadowed), which made
/// `skills.list` report every bundled skill as available for a project that
/// had, say, one custom skill and nothing else — the catalog lied about
/// what would actually resolve (code-critic PR #3465 re-review, MEDIUM:
/// "half-fixed" — the agents side already mirrored its resolver correctly,
/// this side did not). Fixed by mirroring `discover_skill_metadata`'s own
/// `if !skills.is_empty() { return skills }` threshold exactly, at the
/// catalog level rather than per-name.
/// What: ignores `params`. When `state.dir` is `Some` and
/// `discover_disk_skill_metadata` returns anything non-empty, the response
/// is EXACTLY those disk entries (`tier: "project"`) — no bundled entries
/// at all. Otherwise (`state.dir` is `None`, i.e. projectless, OR the
/// project's skills dir is missing/empty) the response is the full bundled
/// catalog (`tier: "bundled"`). Sorted by name either way.
///
/// A FOURTH tier, `"plugin"`, is layered on top when `state.dir` is `Some`
/// and a project root can be recovered from it (issue #3539 — Phase 1
/// Claude Code plugin support): every skill discovered under
/// `<project_root>/.claude/plugins/*/skills/` is added, namespaced
/// `<plugin>:<name>` by `plugins::skills::discover_plugin_skills`. This
/// layer is DELIBERATELY INDEPENDENT of the bundled-vs-project threshold
/// above — it is added identically whether the response above was the
/// bundled catalog or the project's own disk skills, so a project with one
/// custom skill AND a plugin still sees both its custom skill and every
/// plugin skill (#3539's locked precedence-interaction requirement; see
/// `tests::list_plugin_skills_are_additive_alongside_project_custom_skill`).
/// Test: `tests::list_returns_bundled_when_projectless`,
/// `tests::list_returns_bundled_when_disk_empty`,
/// `tests::list_returns_disk_only_when_disk_non_empty_no_bundled_entries`,
/// `tests::list_plugin_skills_are_additive_alongside_project_custom_skill`.
async fn skills_list(
    state: &SkillsCatalogState,
    _params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let disk = state
        .dir
        .as_ref()
        .map(|dir| super::discover_disk_skill_metadata(dir))
        .unwrap_or_default();

    let mut entries: Vec<(String, Value)> = if disk.is_empty() {
        super::embedded_skill_metadata()
            .iter()
            .map(|m| (m.name.clone(), entry_json(m, "bundled")))
            .collect()
    } else {
        disk.iter()
            .map(|m| (m.name.clone(), entry_json(m, "project")))
            .collect()
    };

    if let Some(dir) = &state.dir
        && let Some(project_root) = crate::plugins::project_root_two_levels_up(dir)
    {
        for m in crate::plugins::skills::discover_plugin_skills(&project_root) {
            entries.push((m.name.clone(), entry_json(&m, "plugin")));
        }
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let skills: Vec<Value> = entries.into_iter().map(|(_, v)| v).collect();
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
/// name collision (`-32001`, 403); existing disk skill (`-32009
/// already_exists`, 409). The existence check is NOT a separate
/// `skill_md.exists()` pre-check — the same TOCTOU hole
/// `agents::protocol::agents_create` had (code-critic PR #3465 review,
/// HIGH 2). Instead: `create_dir_all` the skill directory unconditionally
/// (idempotent — an existing dir is fine, only the `SKILL.md` inside it is
/// the conflict unit), then `agents::protocol::write_new_file`'s atomic
/// `O_CREAT|O_EXCL` create on `SKILL.md` itself.
/// Test: `tests::create_writes_file_and_returns_project_tier`,
/// `tests::create_rejects_when_projectless`,
/// `tests::create_rejects_bundled_name_collision`,
/// `tests::create_rejects_existing_disk_skill`,
/// `tests::create_conflict_does_not_clobber_existing_skill`.
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
    std::fs::create_dir_all(&skill_dir)
        .map_err(|e| RpcError::internal(format!("creating skill dir: {e}")))?;
    crate::agents::protocol::write_new_file(&skill_md, &p.content, || {
        format!("skill '{}' already exists on disk", p.name)
    })?;

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

    /// A bound project with an EXISTING BUT EMPTY skills directory still
    /// falls back to the bundled catalog — the same threshold
    /// `discover_skill_metadata`'s own `if !skills.is_empty()` uses, pinned
    /// here at the `skills.list` handler level too (code-critic PR #3465
    /// re-review, MEDIUM).
    #[tokio::test]
    async fn list_returns_bundled_when_disk_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let result = skills_list(&s, Value::Null, ctx()).await.expect("list");
        let skills = result["skills"].as_array().expect("array");
        assert_eq!(skills.len(), crate::assets::DEFAULT_SKILLS.len());
        assert!(skills.iter().all(|sk| sk["tier"] == "bundled"));
    }

    /// The core fix under test (code-critic PR #3465 re-review, MEDIUM): a
    /// project with even ONE custom disk skill must see ONLY that skill —
    /// zero bundled entries — because that is exactly what
    /// `FsSkillResolver`/`discover_skill_metadata` will actually resolve at
    /// `task.run` time (whole-catalog replacement, not a per-name overlay).
    /// A prior version of `skills_list` built a bundled ∪ disk union here,
    /// which reported every bundled skill as available even though none of
    /// them would actually dispatch — this test pins the corrected,
    /// resolver-matching behavior.
    #[tokio::test]
    async fn list_returns_disk_only_when_disk_non_empty_no_bundled_entries() {
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
        assert_eq!(
            skills.len(),
            1,
            "must be ONLY the one disk skill — no bundled entries alongside it"
        );
        assert_eq!(skills[0]["name"], "my-custom-skill");
        assert_eq!(skills[0]["tier"], "project");
        assert!(
            skills.iter().all(|sk| sk["tier"] != "bundled"),
            "the bundled catalog must be entirely absent once disk is non-empty"
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
        // `-32009 already_exists` — NOT `-32008 active_conflict` (code-critic
        // PR #3465 review, LOW).
        assert_eq!(err.code, -32009);
        assert_eq!(err.data.as_ref().unwrap()["error_type"], "already_exists");
    }

    /// The conflict path must go through `write_new_file`'s atomic
    /// `O_CREAT|O_EXCL` create — a losing `skills.create` can NEVER have
    /// truncated or overwritten the existing `SKILL.md`, even transiently
    /// (code-critic PR #3465 review, HIGH 2).
    #[tokio::test]
    async fn create_conflict_does_not_clobber_existing_skill() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("dup-skill");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let original = "---\nname: dup-skill\ndescription: ORIGINAL\n---\n\nBody.\n";
        std::fs::write(dir.join("SKILL.md"), original).expect("write");

        let s = SkillsCatalogState {
            dir: Some(tmp.path().to_path_buf()),
        };
        let err = skills_create(
            &s,
            json!({"name": "dup-skill", "content": "CLOBBER"}),
            ctx(),
        )
        .await
        .expect_err("must conflict");
        assert_eq!(err.code, -32009);

        let on_disk = std::fs::read_to_string(dir.join("SKILL.md")).expect("read");
        assert_eq!(on_disk, original, "existing SKILL.md must be untouched");
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

    /// `skills.list`'s `plugin` tier is independent of the bundled-vs-project
    /// whole-catalog-replacement threshold (PR #3465): a project with ONE
    /// custom skill (which alone would suppress the bundled catalog, see
    /// `list_returns_disk_only_when_disk_non_empty_no_bundled_entries`)
    /// PLUS a plugin still shows both the project's custom skill and every
    /// namespaced plugin skill (issue #3539's locked precedence-interaction
    /// requirement).
    ///
    /// Why: this is the exact test #3539 calls for — proof that neither
    /// side of the interaction suppresses the other.
    /// Test: this test.
    #[tokio::test]
    async fn list_plugin_skills_are_additive_alongside_project_custom_skill() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");
        let custom_dir = skills_dir.join("my-custom-skill");
        std::fs::create_dir_all(&custom_dir).expect("mkdir");
        std::fs::write(
            custom_dir.join("SKILL.md"),
            "---\nname: my-custom-skill\ndescription: Custom\n---\n\nBody.\n",
        )
        .expect("write");

        let plugin_skill_dir = tmp
            .path()
            .join(".claude")
            .join("plugins")
            .join("my-plugin")
            .join("skills")
            .join("plugin-skill");
        std::fs::create_dir_all(&plugin_skill_dir).expect("mkdir");
        std::fs::write(
            plugin_skill_dir.join("SKILL.md"),
            "---\nname: plugin-skill\ndescription: From a plugin\n---\n\nBody.\n",
        )
        .expect("write");

        let s = SkillsCatalogState {
            dir: Some(skills_dir),
        };
        let result = skills_list(&s, Value::Null, ctx()).await.expect("list");
        let skills = result["skills"].as_array().expect("array");

        let project_entry = skills
            .iter()
            .find(|sk| sk["name"] == "my-custom-skill")
            .expect("the project's own custom skill must still be listed");
        assert_eq!(project_entry["tier"], "project");

        let plugin_entry = skills
            .iter()
            .find(|sk| sk["name"] == "my-plugin:plugin-skill")
            .expect("the namespaced plugin skill must be listed too");
        assert_eq!(plugin_entry["tier"], "plugin");

        assert!(
            skills.iter().all(|sk| sk["tier"] != "bundled"),
            "the bundled catalog must still be entirely absent (unrelated to the plugin tier)"
        );
    }
}
