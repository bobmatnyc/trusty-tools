//! Shared schema and repo-opening helpers for the git tool surface.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agents::CrossProjectScope;
use crate::git::GitRepo;

/// The optional per-call repo selector added to every git tool schema when —
/// and only when — the bound scope reaches more than one project (#4172).
pub(super) const REPO_ARG: &str = "repo";

/// Build an OpenAI-style function schema envelope.
///
/// Why: Every git tool wraps its parameters in the same `type:function` shell;
/// centralising it keeps each `schema()` body to a single call.
/// What: Returns the `{type, function:{name, description, parameters}}` JSON.
/// Test: `git_log_tool_schema_valid` asserts the envelope shape.
pub(super) fn fn_schema(name: &str, description: &str, params: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": params
        }
    })
}

/// [`fn_schema`] plus the tier-gated `repo` selector (#4172, epic #4167).
///
/// Why: An L1 persona's git surface must be unchanged down to the JSON
/// schema — showing it a `repo` knob it can never turn would both waste
/// tokens and invite the model to keep retrying a call the gate will keep
/// refusing. So the argument is advertised only when
/// [`CrossProjectScope::is_cross_project`] is true, which by construction
/// only an L0 persona with at least one admitted registry project can be.
/// What: When the scope is single-tenant, returns [`fn_schema`] verbatim —
/// byte-identical to pre-#4172. Otherwise injects an optional `repo` string
/// property (documented with the allow-set) into `params.properties`.
/// `additionalProperties: false` on every git schema means an L1 persona
/// passing `repo` is a schema violation on top of the gate's refusal.
/// Test: `git_schemas_omit_repo_arg_for_single_tenant_scope`,
/// `git_schemas_offer_repo_arg_for_cross_project_scope`.
pub(super) fn scoped_schema(
    scope: &CrossProjectScope,
    name: &str,
    description: &str,
    mut params: Value,
) -> Value {
    if scope.is_cross_project()
        && let Some(props) = params.get_mut("properties").and_then(Value::as_object_mut)
    {
        let roots = scope
            .allowed_roots()
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        props.insert(
            REPO_ARG.to_string(),
            json!({
                "type": "string",
                "description": format!(
                    "Absolute path of the project to act on. Defaults to this agent's own \
                     project. Permitted roots: {roots}"
                )
            }),
        );
    }
    fn_schema(name, description, params)
}

/// Resolve the repository root ONE git tool call should act on (#4172).
///
/// Why: The single enforcement point shared by all twelve tools, so
/// "bounded to the allow-set" is a property of one reviewable call rather
/// than of twelve tool bodies.
/// What: Delegates to [`CrossProjectScope::resolve_repo_root`] with the
/// call's `repo` argument (absent → the agent's own root, exactly as
/// pre-#4172), prefixing the tool name onto any refusal so the model can see
/// which call was denied.
/// Test: `git_status_refuses_repo_outside_allow_set`,
/// `git_status_accepts_repo_inside_allow_set`.
pub(super) fn scoped_root(
    scope: &CrossProjectScope,
    args: &Value,
    tool: &str,
) -> std::result::Result<PathBuf, String> {
    scope
        .resolve_repo_root(args.get(REPO_ARG).and_then(Value::as_str))
        .map_err(|e| format!("{tool}: {e}"))
}

/// Open the git repository rooted at `root`, mapping errors to tool-friendly
/// strings.
///
/// Why: Read-only git tools (`status`, `log`, `branches`, `search_commits`)
/// open the repo the same way and surface the same failure message.
/// What: Returns `Ok(GitRepo)` or an `Err(String)` describing the open failure.
/// Test: Exercised indirectly by `git_status_executes_against_trusty_agents_repo`.
pub(super) fn open_repo(root: &Path) -> std::result::Result<GitRepo, String> {
    GitRepo::open(root).map_err(|e| format!("failed to open git repo at {}: {e}", root.display()))
}

/// [`scoped_root`] + [`open_repo`] + a post-discovery containment re-check
/// (#4172).
///
/// Why: `GitRepo::open` is libgit2's `Repository::discover`, which walks
/// UPWARD from its seed. A permitted seed can therefore still resolve to a
/// repository whose work tree is an ANCESTOR of the allow-set — a real
/// escape hop that resolving the seed alone does not close. Re-checking the
/// DISCOVERED work tree through [`CrossProjectScope::permits`] closes it.
/// What: The re-check runs only when the caller named an explicit `repo`.
/// Without one the tool targets the root its registration site bound it to,
/// and upward discovery from that root is the pre-#4172 contract every
/// existing call site relies on (`build_assistant_tier_registry` seeds the
/// process cwd, which is routinely a subdirectory of the repo) — tightening
/// THAT is a separate change, not #4172's cross-project widening.
/// Test: `git_status_refuses_repo_outside_allow_set`,
/// `git_status_accepts_repo_inside_allow_set`.
pub(super) fn open_scoped_repo(
    scope: &CrossProjectScope,
    args: &Value,
    tool: &str,
) -> std::result::Result<GitRepo, String> {
    let root = scoped_root(scope, args, tool)?;
    let repo = open_repo(&root)?;
    if args.get(REPO_ARG).and_then(Value::as_str).is_some() && !scope.permits(&repo.root) {
        return Err(format!(
            "{tool}: discovered work tree {} is outside this agent's project allow-set. {}",
            repo.root.display(),
            scope.audit_summary()
        ));
    }
    Ok(repo)
}
