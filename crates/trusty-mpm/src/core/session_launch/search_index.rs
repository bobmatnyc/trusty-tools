//! trusty-search MCP stub + project-index registration/reindex helpers.
//!
//! Why: split out of `settings.rs` (issue #610 SLOC cap) — the trusty-search
//! index lifecycle (build the pinned MCP stub, find-or-create the index, then
//! best-effort trigger a reindex so it is actually populated) is a cohesive
//! concern distinct from the output-style/hook/trust-preseed helpers that
//! remain in `settings.rs`.
//! What: [`trusty_search_mcp_value`] / [`inject_trusty_search_mcp`] build and
//! write the `.mcp.json` stub (issue #1373); [`register_project_index`] is now a
//! thin wrapper over the shared [`trusty_common::search_index::ensure_project_indexed`]
//! entry point, which find-or-creates the daemon-side index and ensures it is
//! populated (issue #1908). That register+reindex logic was PROMOTED into
//! trusty-common so trusty-code can reuse the ONE implementation (common
//! entry-point rule) — this module keeps only the MCP-stub concerns plus the
//! wrapper.
//! Test: each `pub(super)` function has a dedicated test in `tests.rs`.

use std::path::Path;

use super::PrepError;
use super::settings::inject_mcp_server;

/// Build the `trusty-search` MCP server definition injected into a project's
/// `.mcp.json`, optionally pinned to a project index (issue #1373).
///
/// Why (issue #1270 / step 4): trusty-mpm-spawned sessions need the code-search
/// tools (`search`, `grep`, `get_call_chain`, …). Issue #1373: without pinning,
/// the contextless `trusty-search serve` stub left index selection to the LLM,
/// which routinely queried the WRONG index (usually the persistent `claude-mpm`
/// one) instead of the session's own project. Passing `--index <derived-id>`
/// pins the session to its project index so a bare `search` always resolves
/// correctly and fan-out never sweeps every index. The canonical stdio MCP
/// invocation is bare `serve` (stdio is the default transport; HTTP is off
/// unless `--with-http`).
/// What: returns the JSON `Value` for a stdio MCP server running
/// `trusty-search serve` — with `["serve", "--index", "<id>"]` when `index_id`
/// is `Some` (non-empty), else the unpinned `["serve"]`. Index *creation* is
/// handled separately by [`register_project_index`].
/// Test: `trusty_search_mcp_value_pins_index`, `trusty_search_mcp_value_unpinned`.
pub(super) fn trusty_search_mcp_value(index_id: Option<&str>) -> serde_json::Value {
    let args: Vec<serde_json::Value> = match index_id {
        Some(id) if !id.trim().is_empty() => vec![
            serde_json::Value::String("serve".to_string()),
            serde_json::Value::String("--index".to_string()),
            serde_json::Value::String(id.to_string()),
        ],
        _ => vec![serde_json::Value::String("serve".to_string())],
    };
    serde_json::json!({
        "type": "stdio",
        "command": "trusty-search",
        "args": args,
    })
}

/// Inject the `trusty-search` MCP server into the project's `.mcp.json`,
/// pinned to the project's index when one is known (issue #1373).
///
/// Why (issue #1270 / step 4): spawned sessions must reach the code-search
/// tools, but `trusty-search` was never registered alongside `trusty-memory`.
/// Issue #1373: the stub must additionally PIN the session to the project's own
/// index (`--index <id>`) so queries never resolve to the wrong index. When
/// `index_id` is `None` (derivation failed) the unpinned stub is written so the
/// session still gets the tools — backward-compatible with the pre-#1373 stub.
/// What: builds the (optionally pinned) server value via
/// [`trusty_search_mcp_value`] and registers it under the key `trusty-search`.
/// Test: `inject_trusty_search_mcp_adds_server`,
/// `inject_trusty_search_mcp_preserves_existing`,
/// `inject_trusty_search_mcp_is_idempotent`,
/// `inject_trusty_search_mcp_pins_index`,
/// `inject_both_mcp_servers_coexist`.
pub(super) fn inject_trusty_search_mcp(
    project_path: &Path,
    index_id: Option<&str>,
) -> Result<(), PrepError> {
    inject_mcp_server(
        project_path,
        "trusty-search",
        trusty_search_mcp_value(index_id),
    )
}

/// Find-or-create the trusty-search index for `project_root`, best-effort
/// trigger a reindex so it is actually populated, and return its id (issues
/// #1373, #1908).
///
/// Why: pinning the serve stub to an index id is only useful if that index
/// actually exists in the daemon — otherwise a query against it returns nothing
/// and the LLM falls back to guessing (the very bug #1373 fixes). The
/// register-and-populate logic was PROMOTED into
/// [`trusty_common::search_index::ensure_project_indexed`] so trusty-code can
/// reuse the ONE implementation (common entry-point rule); this wrapper keeps
/// the session-launch call site and name stable. Behaviour is unchanged: derive
/// the canonical index id, best-effort `POST /indexes` + freshness-gated reindex
/// when the daemon is reachable, always return the derived id so the stub is
/// pinned even when the daemon is down, and never propagate an error (the
/// session must still launch).
/// What: delegates to [`trusty_common::search_index::ensure_project_indexed`]
/// with `allow_sensitive_path: false` (issue #2914). A trusty-mpm session
/// workspace is always either the user's checked-out repository or a
/// `.worktrees/<uuid>` leaf inside it — never a legitimate OS-temp path — so
/// this caller, unlike trusty-code's task-start caller, has no reason to
/// bypass the daemon's `SENSITIVE_PATH_PREFIXES` denylist. Before this fix the
/// bypass was unconditional, so a session-launch test standing in a `tempfile`
/// fixture for `project_root` (e.g. a workspace under `/var/folders/…`)
/// silently registered that throwaway directory against whatever trusty-search
/// daemon happened to be running on the developer/CI machine — the root cause
/// of the ephemeral-index leak this issue reports.
/// Test: `register_project_index_returns_derived_id` (derivation + daemon-down
/// graceful path) and `register_project_index_never_bypasses_sensitive_path_denylist`
/// (issue #2914 regression) in `tests.rs`; the promoted logic is unit-tested in
/// `trusty_common::search_index::tests`.
pub(super) fn register_project_index(project_root: &Path) -> Option<String> {
    trusty_common::search_index::ensure_project_indexed(project_root, false)
}
