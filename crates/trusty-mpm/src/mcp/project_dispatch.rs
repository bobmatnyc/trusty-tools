//! Argument parsing + routing for the project-registry MCP tools
//! (WI-2 + WI-5, #1519 / #1517).
//!
//! Why: mirroring `session_dispatch.rs`, pulling the project-tool parse-and-call
//! arms into a sibling module keeps `mcp/mod.rs` under the 500-SLOC production
//! cap while giving the project tools an auditable home.
//! What: [`try_dispatch`] matches a tool name against the four project tools
//! (`project_list`, `project_register`, `project_get`, `project_resolve`); when
//! it matches, parses arguments and calls the corresponding
//! [`super::OrchestratorBackend`] method, returning `Some(result)`. A
//! non-project tool name returns `None`.
//! Test: `super::tests::dispatch_project_*` cases drive this module through
//! the public `dispatch` entry point with a mock backend.

use serde_json::Value;

use super::{OrchestratorBackend, required_str};

/// Route a project-registry tool call to the backend.
///
/// Why: a single entry point lets `dispatch_tool_call` delegate every
/// project-tool name in one arm without widening the core dispatch match.
/// What: returns `Some(Result)` for the three project tool names, parsing
/// args and calling the matching backend method, or `None` for any other name.
/// Test: exercised by every `dispatch_project_*` test in `super::tests`.
pub async fn try_dispatch<B: OrchestratorBackend>(
    backend: &B,
    name: &str,
    args: &Value,
) -> Option<Result<Value, String>> {
    let result = match name {
        "project_list" => backend.project_list().await,
        "project_register" => project_register(backend, args).await,
        "project_get" => match required_str(args, "name") {
            Ok(n) => backend.project_get(&n).await,
            Err(e) => Err(e),
        },
        "project_resolve" => match required_str(args, "query") {
            Ok(q) => backend.project_resolve(&q).await,
            Err(e) => Err(e),
        },
        _ => return None,
    };
    Some(result)
}

/// Parse `project_register` arguments and call the backend.
///
/// Why: `project_register` has the widest argument surface (name, repo_url
/// required + five optionals); a helper keeps [`try_dispatch`] readable.
/// What: requires `name` and `repo_url`; reads optional `default_branch`,
/// `stack_hint`, `tags`, `description`, `gh_user` (#2081), and `gh_account`
/// (#3025); calls
/// [`OrchestratorBackend::project_register`]. Missing required fields yield a
/// descriptive error string.
/// Test: `super::tests::dispatch_project_register_tool`,
/// `super::tests::dispatch_project_register_requires_name`.
async fn project_register<B: OrchestratorBackend>(
    backend: &B,
    args: &Value,
) -> Result<Value, String> {
    let name = required_str(args, "name")?;
    let repo_url = required_str(args, "repo_url")?;
    let default_branch = args.get("default_branch").and_then(Value::as_str);
    let stack_hint = args.get("stack_hint").and_then(Value::as_str);
    let tags: Option<Vec<String>> = args.get("tags").and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    });
    let description = args.get("description").and_then(Value::as_str);
    let gh_user = args.get("gh_user").and_then(Value::as_str);
    let gh_account = args.get("gh_account").and_then(Value::as_str);
    backend
        .project_register(
            &name,
            &repo_url,
            default_branch,
            stack_hint,
            tags,
            description,
            gh_user,
            gh_account,
        )
        .await
}
