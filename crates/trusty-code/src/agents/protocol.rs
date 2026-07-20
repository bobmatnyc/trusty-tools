//! `agents.*` JSON-RPC methods — the daemon's full-tier AGENT CATALOG
//! surface (issue #3449), backing the Foundry GUI's Agents management tab.
//!
//! Why: distinct from `session.get_agents` (`crate::serve::rest::agents` /
//! `crate::session::protocol_agents`), which answers "who is running (or
//! ran) inside one existing session" — this module answers "what agents
//! could I dispatch?", independent of any session. The GUI's Agents tab
//! needs to list every agent across BOTH tiers `resolve_agent` already
//! recognises (embedded ∪ disk — see [`super::resolve_agent`]'s docs) and
//! let an operator add/remove disk-tier ones; the embedded roster (32
//! agents incl. `pm`, #2958/#3437) is compiled in and therefore read-only.
//! There is exactly ONE disk tier active at a time — `<binding
//! root>/.claude/agents` when a project is bound, `~/.claude/agents`
//! (Claude-Code's user-level convention) when projectless (see
//! `crate::binding::ProjectBinding::agents_dir`) — so every disk entry this
//! module reports carries the SAME tier label for one daemon instance,
//! computed once at [`register`] time from how the daemon itself was
//! started, never re-derived per request.
//! What: [`register`] wires three methods onto a shared, immutable
//! [`AgentsCatalogState`] (`dir` + `tier`, both fixed for the daemon's
//! lifetime — no `Mutex` needed, every operation here is a stateless
//! filesystem call):
//!   - `agents.list` -> `{"agents": [{name, tier, description?, model?}]}`,
//!     the union of the embedded roster (`tier: "embedded"`) and the
//!     resolved disk tier (`tier: "project"` or `"user"`) — a disk agent
//!     overriding an embedded name WINS and suppresses the embedded copy of
//!     that name, mirroring [`super::resolve_agent`]'s disk-wins precedence
//!     exactly (so the catalog never shows the same name twice with two
//!     different tiers).
//!   - `agents.create{name, content}` -> writes
//!     `<dir>/<name>.md` from the caller-supplied Markdown+frontmatter body.
//!     Rejects (see [`validate_agent_name`]) a `name` that is empty, exceeds
//!     [`MAX_AGENT_NAME_LEN`], contains anything outside `[a-z0-9-]`, or
//!     collides with an EMBEDDED name (embedded is read-only — this endpoint
//!     never lets a disk file silently shadow it) with `-32001
//!     permission_denied` (403); an already-existing disk file of the same
//!     name with a `-32008`-shaped "already exists" conflict (409, use
//!     delete-then-create to replace).
//!   - `agents.delete{name}` -> removes `<dir>/<name>.md`. `-32002 not_found`
//!     (404) if no disk file exists under that name; `-32001
//!     permission_denied` (403) if `name` names an EMBEDDED agent (nothing
//!     to delete on disk — the embedded roster cannot be removed).
//!
//! Test: `tests::*`.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::jsonrpc::{ConnectionContext, Router, RpcError};

use super::{AgentConfig, discover_agents, load_embedded_default_agents, md_loader};

/// Maximum length for a caller-supplied agent `name` in [`agents_create`].
///
/// Why: a generous but finite bound — long enough for any real agent slug,
/// short enough to reject a pathological payload before it ever reaches the
/// filesystem.
const MAX_AGENT_NAME_LEN: usize = 64;

/// Shared, immutable state every `agents.*` handler in this module closes
/// over: the resolved disk agents directory and its tier label for this
/// daemon instance.
///
/// Why: `dir`/`tier` are fixed for the daemon's entire lifetime (derived
/// once from `ProjectBinding` at startup, see `crate::serve::build_router_at`)
/// — no shared mutable state, no `Mutex`, unlike `WorkstreamStore`.
/// What: `dir` is `ProjectBinding::agents_dir()`'s result; `tier` is
/// `"project"` when a project is bound, `"user"` when projectless.
#[derive(Clone)]
pub struct AgentsCatalogState {
    /// The resolved disk agents directory (project- or user-scoped).
    pub dir: PathBuf,
    /// `"project"` or `"user"` — which tier `dir` represents.
    pub tier: &'static str,
}

impl AgentsCatalogState {
    /// Build state from a resolved agents directory and whether a project
    /// is bound.
    ///
    /// Why: keeps the "project bound -> tier project, else user" rule in one
    /// place rather than re-deriving it at every call site that constructs
    /// this state (today: `crate::serve::build_router_at`).
    /// What: `tier = "project"` when `project_bound`, else `"user"`.
    pub fn new(dir: PathBuf, project_bound: bool) -> Self {
        Self {
            dir,
            tier: if project_bound { "project" } else { "user" },
        }
    }
}

/// Register `agents.list`, `agents.create`, `agents.delete` onto `router`.
///
/// Why: the one place listing this namespace's full surface, mirroring
/// every other `*::protocol::register` in this crate.
/// What: clones `state` once per method (cheap — `PathBuf` + `&'static
/// str`) into a small adapter closure that forwards to the corresponding
/// free function below.
/// Test: `tests::register_wires_all_three_methods`.
pub fn register(router: &mut Router, state: AgentsCatalogState) {
    let s = state.clone();
    router.register(
        "agents.list",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { agents_list(&s, params, ctx).await }
        },
    );

    let s = state.clone();
    router.register(
        "agents.create",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { agents_create(&s, params, ctx).await }
        },
    );

    let s = state;
    router.register(
        "agents.delete",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { agents_delete(&s, params, ctx).await }
        },
    );
}

/// Wire shape for one catalog entry — shared by `agents.list`'s embedded and
/// disk halves so the two are indistinguishable to a client beyond `tier`.
fn entry_json(cfg: &AgentConfig, tier: &str) -> Value {
    json!({
        "name": cfg.agent.name,
        "tier": tier,
        "description": cfg.agent.description,
        "model": cfg.agent.model,
    })
}

/// `agents.list` -> the full embedded ∪ disk catalog.
///
/// Why: the GUI's Agents tab roster fetch — see module docs for the
/// disk-wins-over-embedded union rule.
/// What: ignores `params` (no filters yet). Builds the embedded set keyed by
/// name, then overlays disk entries (skipping any disk `.md` that fails to
/// parse, logged at WARN — mirrors `load_all_agents`'s graceful-skip
/// convention), removing the embedded entry of the same name whenever a
/// disk override exists. Sorted by name.
/// Test: `tests::list_returns_embedded_when_disk_empty`,
/// `tests::list_disk_override_wins_and_suppresses_embedded`,
/// `tests::list_disk_only_entries_are_additive`.
async fn agents_list(
    state: &AgentsCatalogState,
    _params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let mut by_name: Vec<(String, Value)> = load_embedded_default_agents()
        .iter()
        .map(|cfg| (cfg.agent.name.clone(), entry_json(cfg, "embedded")))
        .collect();

    for (name, path) in discover_agents(&state.dir) {
        match md_loader::load_md_agent(&path) {
            Ok(cfg) => {
                let entry = entry_json(&cfg, state.tier);
                match by_name.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => slot.1 = entry,
                    None => by_name.push((name, entry)),
                }
            }
            Err(e) => {
                tracing::warn!("agents.list: skipping unparseable disk agent '{name}': {e}");
            }
        }
    }

    by_name.sort_by(|a, b| a.0.cmp(&b.0));
    let agents: Vec<Value> = by_name.into_iter().map(|(_, v)| v).collect();
    Ok(json!({ "agents": agents }))
}

/// `params` shape for `agents.create`.
#[derive(Deserialize)]
struct CreateParams {
    name: String,
    content: String,
}

/// Validate a caller-supplied agent/skill name against the shared naming
/// contract (issue #3449): non-empty, `[a-z0-9-]` only, bounded length.
///
/// Why: this is ALSO the path-traversal guard — a name matching this
/// pattern can never contain `/`, `..`, or any other path-breaking
/// character, so `dir.join(format!("{name}.md"))` is always confined to
/// `dir` by construction. Shared verbatim by `agents::protocol` and
/// `skills::protocol` (issue #3449's paired endpoints) so the two surfaces
/// can never drift into two different naming rules.
/// What: `Err(RpcError::invalid_argument(..))` on any violation.
/// Test: `tests::validate_name_rejects_empty`,
/// `tests::validate_name_rejects_uppercase`,
/// `tests::validate_name_rejects_path_traversal`,
/// `tests::validate_name_rejects_too_long`,
/// `tests::validate_name_accepts_lowercase_alnum_hyphen`.
pub(crate) fn validate_agent_name(name: &str) -> Result<(), RpcError> {
    if name.is_empty() {
        return Err(RpcError::invalid_argument("name must not be empty"));
    }
    if name.len() > MAX_AGENT_NAME_LEN {
        return Err(RpcError::invalid_argument(format!(
            "name must be at most {MAX_AGENT_NAME_LEN} characters"
        )));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(RpcError::invalid_argument(
            "name must match [a-z0-9-]+ (lowercase letters, digits, hyphens only)",
        ));
    }
    Ok(())
}

/// `agents.create` -> write `<dir>/<name>.md` from `content`.
///
/// Why: the GUI's Agents tab add-flow — creates a new disk-tier agent.
/// What: `201`-shaped success `{"name", "tier"}` (REST layer maps this to
/// `201 Created`, see `serve::rest::agent_catalog::create`). Errors: invalid
/// `name` (`-32602`/`-32003`), `name` collides with an embedded agent
/// (`-32001 permission_denied`, 403 — embedded is read-only), an existing
/// disk file of the same name (`-32008`-shaped conflict, 409), or a
/// write-side I/O failure (`-32603 internal`).
/// Test: `tests::create_writes_file_and_returns_tier`,
/// `tests::create_rejects_invalid_name`,
/// `tests::create_rejects_embedded_name_collision`,
/// `tests::create_rejects_existing_disk_file`.
async fn agents_create(
    state: &AgentsCatalogState,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: CreateParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("agents.create: {e}")))?;

    validate_agent_name(&p.name)?;

    if crate::assets::DEFAULT_AGENTS
        .iter()
        .any(|a| a.name() == p.name)
    {
        return Err(RpcError::permission_denied(format!(
            "'{}' is an embedded agent and cannot be overridden by name via this endpoint",
            p.name
        )));
    }

    let path = agent_path(&state.dir, &p.name)?;
    if path.exists() {
        return Err(
            RpcError::new(-32008, format!("agent '{}' already exists on disk", p.name))
                .with_data(json!({"error_type": "already_exists"})),
        );
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| RpcError::internal(format!("creating agents dir: {e}")))?;
    }
    std::fs::write(&path, &p.content)
        .map_err(|e| RpcError::internal(format!("writing agent file: {e}")))?;

    Ok(json!({ "name": p.name, "tier": state.tier }))
}

/// `params` shape for `agents.delete`.
#[derive(Deserialize)]
struct DeleteParams {
    name: String,
}

/// `agents.delete` -> remove `<dir>/<name>.md`.
///
/// Why: the GUI's Agents tab remove-flow (two-step inline confirm on the
/// client side — this endpoint itself is a single, non-idempotent DELETE).
/// What: `{}` on success. `-32002 not_found` (404) when no disk file exists
/// under `name` AND it is not embedded; `-32001 permission_denied` (403)
/// when `name` names an embedded agent (nothing to delete — the embedded
/// roster is compiled in, never on disk).
/// Test: `tests::delete_removes_disk_file`,
/// `tests::delete_missing_name_returns_not_found`,
/// `tests::delete_embedded_name_returns_permission_denied`.
async fn agents_delete(
    state: &AgentsCatalogState,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: DeleteParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("agents.delete: {e}")))?;
    validate_agent_name(&p.name)?;

    let path = agent_path(&state.dir, &p.name)?;
    if !path.exists() {
        if crate::assets::DEFAULT_AGENTS
            .iter()
            .any(|a| a.name() == p.name)
        {
            return Err(RpcError::permission_denied(format!(
                "'{}' is an embedded agent and cannot be deleted",
                p.name
            )));
        }
        return Err(RpcError::not_found(format!(
            "no disk agent named '{}'",
            p.name
        )));
    }

    std::fs::remove_file(&path).map_err(|e| RpcError::internal(format!("deleting agent: {e}")))?;
    Ok(json!({}))
}

/// Join a validated `name` onto `dir` as `<dir>/<name>.md`.
///
/// Why: one seam for the join so both write paths (`create`, `delete`)
/// build the path identically; `name` must already have passed
/// [`validate_agent_name`] before this is called (it re-validates as a
/// defense-in-depth belt-and-suspenders, never trust-by-construction alone).
/// What: `Err` if `name` fails validation; otherwise `dir.join("<name>.md")`.
fn agent_path(dir: &Path, name: &str) -> Result<PathBuf, RpcError> {
    validate_agent_name(name)?;
    Ok(dir.join(format!("{name}.md")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ConnectionContext {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    fn state(dir: &Path, project_bound: bool) -> AgentsCatalogState {
        AgentsCatalogState::new(dir.to_path_buf(), project_bound)
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(validate_agent_name("").is_err());
    }

    #[test]
    fn validate_name_rejects_uppercase() {
        assert!(validate_agent_name("Engineer").is_err());
    }

    #[test]
    fn validate_name_rejects_path_traversal() {
        assert!(validate_agent_name("../../etc/passwd").is_err());
        assert!(validate_agent_name("a/b").is_err());
        assert!(validate_agent_name("..").is_err());
    }

    #[test]
    fn validate_name_rejects_too_long() {
        let long = "a".repeat(MAX_AGENT_NAME_LEN + 1);
        assert!(validate_agent_name(&long).is_err());
    }

    #[test]
    fn validate_name_accepts_lowercase_alnum_hyphen() {
        assert!(validate_agent_name("my-custom-agent-2").is_ok());
    }

    #[tokio::test]
    async fn list_returns_embedded_when_disk_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = state(tmp.path(), false);
        let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
        let agents = result["agents"].as_array().expect("array");
        assert_eq!(agents.len(), crate::assets::DEFAULT_AGENTS.len());
        assert!(agents.iter().all(|a| a["tier"] == "embedded"));
    }

    #[tokio::test]
    async fn list_disk_override_wins_and_suppresses_embedded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("engineer.md"),
            "---\nname: engineer\ndescription: DISK OVERRIDE\n---\n\nBody.\n",
        )
        .expect("write");
        let s = state(tmp.path(), true);
        let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
        let agents = result["agents"].as_array().expect("array");
        let engineer_entries: Vec<_> = agents.iter().filter(|a| a["name"] == "engineer").collect();
        assert_eq!(engineer_entries.len(), 1, "must appear exactly once");
        assert_eq!(engineer_entries[0]["tier"], "project");
        assert_eq!(engineer_entries[0]["description"], "DISK OVERRIDE");
    }

    #[tokio::test]
    async fn list_disk_only_entries_are_additive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("my-custom.md"),
            "---\nname: my-custom\ndescription: Custom\n---\n\nBody.\n",
        )
        .expect("write");
        let s = state(tmp.path(), true);
        let result = agents_list(&s, Value::Null, ctx()).await.expect("list");
        let agents = result["agents"].as_array().expect("array");
        assert_eq!(agents.len(), crate::assets::DEFAULT_AGENTS.len() + 1);
        assert!(
            agents
                .iter()
                .any(|a| a["name"] == "my-custom" && a["tier"] == "project")
        );
    }

    #[tokio::test]
    async fn create_writes_file_and_returns_tier() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = state(tmp.path(), true);
        let result = agents_create(
            &s,
            json!({"name": "my-agent", "content": "---\nname: my-agent\n---\n\nBody.\n"}),
            ctx(),
        )
        .await
        .expect("create");
        assert_eq!(result["name"], "my-agent");
        assert_eq!(result["tier"], "project");
        assert!(tmp.path().join("my-agent.md").exists());
    }

    #[tokio::test]
    async fn create_rejects_invalid_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = state(tmp.path(), true);
        let err = agents_create(&s, json!({"name": "Bad Name!", "content": "x"}), ctx())
            .await
            .expect_err("must reject");
        assert_eq!(err.code, -32003);
    }

    #[tokio::test]
    async fn create_rejects_path_traversal_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = state(tmp.path(), true);
        let err = agents_create(&s, json!({"name": "../../evil", "content": "x"}), ctx())
            .await
            .expect_err("must reject");
        assert_eq!(err.code, -32003);
        assert!(!tmp.path().parent().unwrap().join("evil.md").exists());
    }

    #[tokio::test]
    async fn create_rejects_embedded_name_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = state(tmp.path(), true);
        let err = agents_create(
            &s,
            json!({"name": "engineer", "content": "---\nname: engineer\n---\n\nBody.\n"}),
            ctx(),
        )
        .await
        .expect_err("must reject embedded collision");
        assert_eq!(err.code, -32001);
        assert!(!tmp.path().join("engineer.md").exists());
    }

    #[tokio::test]
    async fn create_rejects_existing_disk_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("dup.md"), "---\nname: dup\n---\n\nBody.\n").expect("write");
        let s = state(tmp.path(), true);
        let err = agents_create(
            &s,
            json!({"name": "dup", "content": "---\nname: dup\n---\n\nNew.\n"}),
            ctx(),
        )
        .await
        .expect_err("must reject existing file");
        assert_eq!(err.code, -32008);
    }

    #[tokio::test]
    async fn delete_removes_disk_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("my-agent.md"),
            "---\nname: my-agent\n---\n\nBody.\n",
        )
        .expect("write");
        let s = state(tmp.path(), true);
        agents_delete(&s, json!({"name": "my-agent"}), ctx())
            .await
            .expect("delete");
        assert!(!tmp.path().join("my-agent.md").exists());
    }

    #[tokio::test]
    async fn delete_missing_name_returns_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = state(tmp.path(), true);
        let err = agents_delete(&s, json!({"name": "totally-bogus"}), ctx())
            .await
            .expect_err("must 404");
        assert_eq!(err.code, -32002);
    }

    #[tokio::test]
    async fn delete_embedded_name_returns_permission_denied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = state(tmp.path(), true);
        let err = agents_delete(&s, json!({"name": "engineer"}), ctx())
            .await
            .expect_err("must 403");
        assert_eq!(err.code, -32001);
    }

    #[tokio::test]
    async fn register_wires_all_three_methods() {
        use trusty_common::mcp::Request;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut router = Router::new();
        register(&mut router, state(tmp.path(), true));

        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(Value::from(1)),
            method: "agents.list".to_string(),
            params: None,
        };
        let resp = router.dispatch(req, &ctx()).await;
        assert!(resp.result.is_some(), "agents.list must be wired");
        assert!(resp.result.unwrap()["agents"].is_array());
    }
}
