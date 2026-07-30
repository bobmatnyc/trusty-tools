//! Native git tool surface for the LLM (#247).
//!
//! Why: Coordinating agents (ctrl, pm, research, observe) need first-class
//! git tools instead of relying on `shell_exec`. Twelve narrow tools each
//! with a typed schema let the LLM call the right operation without
//! constructing shell commands.
//! What: `git_tools(root)` returns `Vec<Arc<dyn ToolExecutor>>` for the
//! twelve operations: status, log, branches, create_branch, checkout,
//! stage, commit, push, pull, fetch, stash, search_commits.
//! Test: See unit tests below — count, schemas, and required-argument
//! validation.

use std::path::PathBuf;
use std::sync::Arc;

use crate::agents::CrossProjectScope;
use crate::tools::traits::ToolExecutor;

mod branch;
mod helpers;
mod inspect;
mod remote;
mod write;

use branch::{GitBranchesTool, GitCheckoutTool, GitCreateBranchTool};
use inspect::{GitLogTool, GitSearchCommitsTool, GitStatusTool};
use remote::{GitFetchTool, GitPullTool, GitPushTool};
use write::{GitCommitTool, GitStageTool, GitStashTool};

/// Build the 12 git tools bound to the given working-tree `root`.
///
/// Why: A single factory keeps registration sites compact and ensures
/// every tool is constructed with the same project root, so an
/// LLM-instructed `git_status` and a follow-up `git_commit` both target
/// the same repo.
/// What: Returns an `Arc<dyn ToolExecutor>` for each of the 12 tools, bound
/// to the SINGLE-TENANT scope over `root` (#4172) — no `repo` argument in
/// any schema, and every call targets `root`, exactly as before #4172. Call
/// sites that want an L0 persona's cross-project reach use
/// [`git_tools_scoped`] instead.
/// Test: `git_tools_count_is_12`,
/// `git_schemas_omit_repo_arg_for_single_tenant_scope`.
pub fn git_tools(root: PathBuf) -> Vec<Arc<dyn ToolExecutor>> {
    git_tools_scoped(Arc::new(CrossProjectScope::single_tenant(root)))
}

/// Build the 12 git tools bound to a resolved cross-project scope (#4172,
/// epic #4167).
///
/// Why: An L0 orchestration persona has to reconcile state across several
/// repos in one turn (#4172's "clone/pull repos outside the current
/// project"). Handing every tool the SAME [`CrossProjectScope`] means the
/// widening is decided once, by the tier resolver, rather than re-derived
/// per tool — and means an L1 persona's twelve tools are provably identical
/// to what they were, because [`CrossProjectScope::single_tenant`] is the
/// shape `git_tools` still builds.
/// What: Returns an `Arc<dyn ToolExecutor>` for each of the 12 tools, all
/// sharing one `Arc<CrossProjectScope>`. When the scope reaches more than
/// one project, each schema additionally advertises an optional absolute
/// `repo` path; every call resolves it through
/// [`CrossProjectScope::resolve_repo_root`], which refuses anything outside
/// the allow-set.
/// Test: `git_schemas_offer_repo_arg_for_cross_project_scope`,
/// `git_status_refuses_repo_outside_allow_set`,
/// `git_status_accepts_repo_inside_allow_set`.
pub fn git_tools_scoped(scope: Arc<CrossProjectScope>) -> Vec<Arc<dyn ToolExecutor>> {
    vec![
        Arc::new(GitStatusTool {
            scope: scope.clone(),
        }),
        Arc::new(GitLogTool {
            scope: scope.clone(),
        }),
        Arc::new(GitBranchesTool {
            scope: scope.clone(),
        }),
        Arc::new(GitCreateBranchTool {
            scope: scope.clone(),
        }),
        Arc::new(GitCheckoutTool {
            scope: scope.clone(),
        }),
        Arc::new(GitStageTool {
            scope: scope.clone(),
        }),
        Arc::new(GitCommitTool {
            scope: scope.clone(),
        }),
        Arc::new(GitPushTool {
            scope: scope.clone(),
        }),
        Arc::new(GitPullTool {
            scope: scope.clone(),
        }),
        Arc::new(GitFetchTool {
            scope: scope.clone(),
        }),
        Arc::new(GitStashTool {
            scope: scope.clone(),
        }),
        Arc::new(GitSearchCommitsTool { scope }),
    ]
}

/// Resolve a persona's cross-project scope and register its git tools
/// (#4172, epic #4167).
///
/// Why: The persona-chat dispatch path needs BOTH halves of #4172 from one
/// resolution — the git surface and `vector_search`'s tier-2 index list —
/// and keeping the four-step sequence (resolve cwd's repo, resolve the
/// scope, register, hand the scope back) here rather than inline at the call
/// site keeps the boundary logic in the module that owns it.
/// What: Discovers the repository containing the process cwd — the SAME
/// seed the pre-#4172 call site used; the scope's home root is that
/// repository's work tree when one is found (so every tool defaults to
/// exactly the root it was bound to pre-#4172) and the cwd otherwise. Git
/// tools are registered ONLY when a repository was found — unchanged from
/// the pre-#4172 call site. Returns the scope so the caller can widen its
/// search-index list with the same resolution.
/// Test: `git_status_accepts_repo_inside_allow_set`,
/// `single_tenant_git_tools_refuse_any_repo_argument`.
pub async fn register_scoped_git_tools(
    registry: &mut crate::tools::ToolRegistry,
    agent: &crate::agents::AgentInfo,
) -> Arc<CrossProjectScope> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let discovered = crate::git::GitRepo::open(&cwd).ok();
    let home = discovered.as_ref().map(|r| r.root.clone()).unwrap_or(cwd);
    let scope = Arc::new(CrossProjectScope::from_registry(agent, &home).await);
    if discovered.is_some() {
        for tool in git_tools_scoped(scope.clone()) {
            registry.register(tool);
        }
    }
    scope
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cwd_root() -> PathBuf {
        std::env::current_dir().unwrap()
    }

    #[test]
    fn git_tools_count_is_12() {
        let tools = git_tools(cwd_root());
        assert_eq!(tools.len(), 12);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for expected in [
            "git_status",
            "git_log",
            "git_branches",
            "git_create_branch",
            "git_checkout",
            "git_stage",
            "git_commit",
            "git_push",
            "git_pull",
            "git_fetch",
            "git_stash",
            "git_search_commits",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool '{expected}' in {names:?}"
            );
        }
    }

    #[test]
    fn git_status_tool_has_no_required_params() {
        let tools = git_tools(cwd_root());
        let s = tools
            .iter()
            .find(|t| t.name() == "git_status")
            .unwrap()
            .schema();
        let required = s["function"]["parameters"]["required"]
            .as_array()
            .expect("required is array");
        assert!(required.is_empty());
    }

    #[test]
    fn git_commit_tool_requires_message() {
        let tools = git_tools(cwd_root());
        let s = tools
            .iter()
            .find(|t| t.name() == "git_commit")
            .unwrap()
            .schema();
        let required: Vec<String> = s["function"]["parameters"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(required.contains(&"message".to_string()));
    }

    #[test]
    fn git_log_tool_schema_valid() {
        let tools = git_tools(cwd_root());
        let s = tools
            .iter()
            .find(|t| t.name() == "git_log")
            .unwrap()
            .schema();
        assert_eq!(s["type"], "function");
        assert_eq!(s["function"]["name"], "git_log");
        let props = &s["function"]["parameters"]["properties"];
        assert!(props.get("limit").is_some());
        assert!(props.get("search").is_some());
    }

    #[tokio::test]
    async fn git_status_executes_against_trusty_agents_repo() {
        let tools = git_tools(cwd_root());
        let status_tool = tools.iter().find(|t| t.name() == "git_status").unwrap();
        let out = status_tool.execute(json!({})).await;
        // We don't assert specific content; just that the tool ran without
        // failing at the open()/get_status() level.
        assert!(
            !out.is_error(),
            "git_status returned error: {}",
            out.content()
        );
    }

    #[tokio::test]
    async fn git_commit_rejects_missing_message() {
        let tools = git_tools(cwd_root());
        let tool = tools.iter().find(|t| t.name() == "git_commit").unwrap();
        let out = tool.execute(json!({})).await;
        assert!(out.is_error());
        assert!(out.content().contains("message"));
    }

    #[tokio::test]
    async fn git_stage_rejects_empty_files() {
        let tools = git_tools(cwd_root());
        let tool = tools.iter().find(|t| t.name() == "git_stage").unwrap();
        let out = tool.execute(json!({"files": []})).await;
        assert!(out.is_error());
    }

    #[tokio::test]
    async fn git_stash_rejects_unknown_action() {
        let tools = git_tools(cwd_root());
        let tool = tools.iter().find(|t| t.name() == "git_stash").unwrap();
        let out = tool.execute(json!({"action": "bogus"})).await;
        assert!(out.is_error());
        assert!(out.content().contains("bogus"));
    }

    // --- #4172 (epic #4167): tier-gated cross-project reach ---

    /// Two real git repos plus the L0 scope that reaches both — built the
    /// PRODUCTION way.
    ///
    /// Why: The cross-project cases need an actual repository to open (the
    /// tools discover through libgit2 and shell out to `git -C`), and the
    /// scope must come from the REAL resolution path, not a test-only
    /// constructor: a real `agent.toml` parsed by `AgentConfig::by_name_in`
    /// supplies the tier (via #4200's fail-closed `AgentInfo::tier`), and a
    /// real `projects.json` read by `ProjectRegistry::list_active` supplies
    /// the allow-set. A boundary proved against hand-built arguments proves
    /// nothing about the boundary production uses.
    /// What: Returns `(tempdir, home_root, second_root, scope)` where `scope`
    /// permits both roots.
    /// Test: used by the `git_*` cross-project cases below.
    async fn two_repo_scope() -> (tempfile::TempDir, PathBuf, PathBuf, Arc<CrossProjectScope>) {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("t4172-git");
        std::fs::create_dir_all(&base).unwrap();
        let tmp = tempfile::Builder::new()
            .prefix("x4172-")
            .tempdir_in(&base)
            .unwrap();
        let home = init_repo(tmp.path(), "home-project");
        let second = init_repo(tmp.path(), "second-project");

        let registry = tmp.path().join("projects.json");
        std::fs::write(
            &registry,
            json!({
                second.display().to_string(): {
                    "path": second.display().to_string(),
                    "name": "second-project",
                    "last_run": null,
                    "status": "active"
                }
            })
            .to_string(),
        )
        .unwrap();

        let agents_dir = tmp.path().join("agents").join("l0-git");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("agent.toml"),
            "[agent]\nname = \"l0-git\"\nrole = \"assistant\"\n\
             description = \"l0 fixture\"\n\
             model = \"claude-sonnet-4-5\"\ntier = \"l0\"\n\n\
             [llm]\ntemperature = 0.2\nmax_tokens = 4096\n",
        )
        .unwrap();
        std::fs::write(agents_dir.join("persona.md"), "Orchestrator persona.\n").unwrap();
        let cfg = crate::agents::AgentConfig::by_name_in(&[tmp.path().join("agents")], "l0-git")
            .expect("load l0 agent config");

        let scope =
            Arc::new(CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await);
        assert!(scope.is_cross_project(), "fixture must resolve to L0 reach");
        (tmp, home, second, scope)
    }

    /// `git init` a real repository under `parent`.
    ///
    /// Test: used by `two_repo_scope`.
    fn init_repo(parent: &std::path::Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        std::fs::create_dir_all(&root).unwrap();
        git2::Repository::init(&root).expect("git init");
        std::fs::canonicalize(&root).unwrap()
    }

    #[test]
    fn git_schemas_omit_repo_arg_for_single_tenant_scope() {
        for tool in git_tools(cwd_root()) {
            let s = tool.schema();
            let props = &s["function"]["parameters"]["properties"];
            assert!(
                props.get("repo").is_none(),
                "{} must not advertise `repo` on a single-tenant scope",
                tool.name()
            );
        }
    }

    #[tokio::test]
    async fn git_schemas_offer_repo_arg_for_cross_project_scope() {
        let (_tmp, _home, second, scope) = two_repo_scope().await;
        for tool in git_tools_scoped(scope) {
            let s = tool.schema();
            let desc = s["function"]["parameters"]["properties"]["repo"]["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{} must advertise `repo`", tool.name()));
            assert!(
                desc.contains(&second.display().to_string()),
                "{} must name the allow-set: {desc}",
                tool.name()
            );
        }
    }

    #[tokio::test]
    async fn git_status_accepts_repo_inside_allow_set() {
        let (_tmp, _home, second, scope) = two_repo_scope().await;
        let tools = git_tools_scoped(scope);
        let tool = tools.iter().find(|t| t.name() == "git_status").unwrap();
        let out = tool
            .execute(json!({ "repo": second.to_str().unwrap() }))
            .await;
        assert!(!out.is_error(), "{}", out.content());
    }

    #[tokio::test]
    async fn git_status_refuses_repo_outside_allow_set() {
        let (tmp, _home, _second, scope) = two_repo_scope().await;
        // A real repo that the scope was never granted.
        let rogue = init_repo(tmp.path(), "rogue-project");
        let tools = git_tools_scoped(scope);
        for name in [
            "git_status",
            "git_log",
            "git_branches",
            "git_search_commits",
        ] {
            let tool = tools.iter().find(|t| t.name() == name).unwrap();
            let out = tool
                .execute(json!({ "repo": rogue.to_str().unwrap(), "query": "x" }))
                .await;
            assert!(out.is_error(), "{name} must refuse an unlisted repo");
            assert!(
                out.content()
                    .contains("outside this agent's project allow-set"),
                "{name}: {}",
                out.content()
            );
        }
    }

    #[tokio::test]
    async fn git_write_tools_refuse_repo_outside_allow_set() {
        let (tmp, _home, _second, scope) = two_repo_scope().await;
        let rogue = init_repo(tmp.path(), "rogue-write");
        let tools = git_tools_scoped(scope);
        let repo = rogue.to_str().unwrap();
        for (name, args) in [
            ("git_create_branch", json!({"repo": repo, "name": "x"})),
            ("git_checkout", json!({"repo": repo, "target": "main"})),
            ("git_stage", json!({"repo": repo, "files": ["a.txt"]})),
            ("git_commit", json!({"repo": repo, "message": "m"})),
            ("git_stash", json!({"repo": repo, "action": "list"})),
            ("git_push", json!({"repo": repo})),
            ("git_pull", json!({"repo": repo})),
            ("git_fetch", json!({"repo": repo})),
        ] {
            let tool = tools.iter().find(|t| t.name() == name).unwrap();
            let out = tool.execute(args).await;
            assert!(out.is_error(), "{name} must refuse an unlisted repo");
            assert!(
                out.content()
                    .contains("outside this agent's project allow-set"),
                "{name}: {}",
                out.content()
            );
        }
    }

    #[tokio::test]
    async fn single_tenant_git_tools_refuse_any_repo_argument() {
        let (tmp, home, second, _scope) = two_repo_scope().await;
        // The L1 shape: `git_tools(root)` — the second repo exists and is a
        // perfectly good repository, but this scope was never widened.
        let tools = git_tools(home);
        let tool = tools.iter().find(|t| t.name() == "git_status").unwrap();
        let out = tool
            .execute(json!({ "repo": second.to_str().unwrap() }))
            .await;
        assert!(out.is_error(), "L1 must not act on a second project");
        drop(tmp);
    }
}
