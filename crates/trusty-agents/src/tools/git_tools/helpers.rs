//! Shared schema and repo-opening helpers for the git tool surface.

use std::path::Path;

use serde_json::{Value, json};

use crate::git::GitRepo;

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
