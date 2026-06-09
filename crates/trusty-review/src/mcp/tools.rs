//! MCP tool definitions and `tools/call` router.
//!
//! Why: Claude Code communicates with MCP servers using JSON-RPC 2.0 over
//! stdio.  This module provides the three trusty-review tools:
//!   - `review_pr`     — review a GitHub PR by owner/repo/number
//!   - `review_diff`   — review a raw unified diff string
//!   - `review_health` — probe service liveness and configuration
//!
//! What: `tool_descriptors` returns the `tools/list` payload; `call_tool`
//! dispatches a `tools/call` request to the appropriate handler.  Results are
//! wrapped in the MCP content envelope `{content:[{type:"text",text:<json>}]}`.
//!
//! Test: `tools_list_has_three_tools`, `review_health_does_not_require_creds`,
//! and `call_unknown_tool_returns_error`.

use std::io::Write as _;
use std::sync::Arc;

use serde_json::Value;
use tempfile::NamedTempFile;
use tracing::info;

use crate::{
    integrations::github::{AuthStrategy, GithubClient, RunMode},
    models::ReviewResult,
    pipeline::{DiffSource, ReviewDeps, ReviewInput, TriggerDecision, run_review},
    service::{
        AppState,
        handlers::{DepInfo, DepStatus, compute_status},
    },
};

// ─── Tool definitions ────────────────────────────────────────────────────────

/// Return the `tools/list` payload — one descriptor per exposed tool.
///
/// Why: Claude Code calls `tools/list` at startup to discover what the server
/// can do.  Accurate `inputSchema` JSON Schema lets the LLM construct correct
/// tool calls without guessing.
/// What: returns a serde_json `Value` array with three tool objects.
/// Test: `tools_list_has_three_tools`.
pub fn tool_descriptors() -> Value {
    serde_json::json!([
        {
            "name": "review_pr",
            "description": "Review a GitHub pull request. Fetches the PR diff, retrieves \
                           code context from trusty-search, and returns a structured verdict \
                           (APPROVE / APPROVE* / REQUEST_CHANGES / BLOCK / UNKNOWN) with \
                           actionable findings.  Requires GITHUB_TOKEN and AWS Bedrock \
                           credentials (or OPENROUTER_API_KEY for OpenRouter provider). \
                           Dry-run by default (PR_INTELLIGENCE_DRY_RUN=true — no GitHub \
                           comments posted).  trusty-search must be running on :7878.",
            "inputSchema": {
                "type": "object",
                "required": ["owner", "repo", "pr"],
                "properties": {
                    "owner": {
                        "type": "string",
                        "description": "GitHub organisation or user that owns the repository"
                    },
                    "repo": {
                        "type": "string",
                        "description": "GitHub repository name"
                    },
                    "pr": {
                        "type": "integer",
                        "description": "Pull request number"
                    },
                    "reviewer_model": {
                        "type": "string",
                        "description": "Override the reviewer model slug. \
                                       Use a `bedrock/<id>` prefix to force AWS Bedrock, \
                                       `openrouter/<id>` for OpenRouter. \
                                       Default: us.anthropic.claude-sonnet-4-6 on Bedrock.",
                        "examples": [
                            "bedrock/us.anthropic.claude-sonnet-4-6",
                            "bedrock/us.anthropic.claude-haiku-4-5",
                            "openrouter/openai/gpt-5.4-mini-20260317"
                        ]
                    }
                }
            }
        },
        {
            "name": "review_diff",
            "description": "Review a raw unified diff string without fetching from GitHub. \
                           Useful for reviewing local changes, staged diffs, or patches. \
                           No GitHub credentials required. \
                           Requires AWS Bedrock credentials (or OPENROUTER_API_KEY). \
                           trusty-search on :7878 is used for code-context retrieval when available.",
            "inputSchema": {
                "type": "object",
                "required": ["diff"],
                "properties": {
                    "diff": {
                        "type": "string",
                        "description": "Unified diff string (output of `git diff` or similar)"
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional human-readable context — e.g. PR title/description, \
                                       ticket number, or a note about what changed and why. \
                                       Appended to the diff file so the reviewer model sees it."
                    },
                    "reviewer_model": {
                        "type": "string",
                        "description": "Override the reviewer model slug (same format as review_pr)."
                    }
                }
            }
        },
        {
            "name": "review_health",
            "description": "Probe trusty-review service liveness and configuration. \
                           Returns the current configuration (dry_run mode, reviewer model) \
                           and dependency reachability. Safe to call without any credentials.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }
    ])
}

// ─── Tool errors ─────────────────────────────────────────────────────────────

/// Internal dispatch error for the MCP tool router.
///
/// Why: differentiates protocol-level errors (unknown tool, malformed params —
/// reported as JSON-RPC errors) from tool-execution errors (reported in-band
/// per MCP spec).
/// What: three variants covering the two protocol cases and the catch-all.
/// Test: `call_unknown_tool_returns_error`.
#[derive(Debug)]
pub enum ToolError {
    /// The tool name is not registered.
    UnknownTool,
    /// A required parameter is missing or has the wrong type.
    InvalidParams(String),
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// Dispatch a `tools/call` request to the appropriate handler.
///
/// Why: centralises the tool routing logic so `mod.rs`'s dispatch function
/// stays thin and each tool handler can be read independently.
/// What: matches on `tool` name, extracts arguments from `args`, calls the
/// appropriate async handler, and wraps the result in the MCP content envelope.
/// Returns `Err(ToolError)` only for protocol-level errors (unknown tool,
/// missing params); tool-execution failures are returned as `Ok(error_envelope)`.
/// Test: `call_unknown_tool_returns_error`, `review_health_does_not_require_creds`.
pub async fn call_tool(tool: &str, args: &Value, state: &AppState) -> Result<Value, ToolError> {
    match tool {
        "review_pr" => call_review_pr(args, state).await,
        "review_diff" => call_review_diff(args, state).await,
        "review_health" => Ok(call_review_health(state).await),
        _ => Err(ToolError::UnknownTool),
    }
}

// ─── review_pr ───────────────────────────────────────────────────────────────

/// Execute the `review_pr` tool.
///
/// Why: lets Claude Code trigger a full GitHub PR review via MCP without
/// requiring the user to invoke the CLI manually.
/// What: resolves the GitHub token, builds a `DiffSource::Github`, constructs
/// `ReviewDeps` from the shared `AppState`, runs the pipeline, and returns the
/// `ReviewResult` as a JSON string in the MCP content envelope.
/// Test: `review_pr_returns_review_result_envelope`.
async fn call_review_pr(args: &Value, state: &AppState) -> Result<Value, ToolError> {
    let owner = require_str(args, "owner")?;
    let repo = require_str(args, "repo")?;
    let pr = args
        .get("pr")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::InvalidParams("missing or non-integer 'pr'".into()))?;

    let reviewer_model = args
        .get("reviewer_model")
        .and_then(Value::as_str)
        .unwrap_or(&state.config.role_models.reviewer.model)
        .to_string();

    // Resolve GitHub token.
    let client = GithubClient::new()
        .map_err(|e| ToolError::InvalidParams(format!("failed to build HTTP client: {e}")))?;
    let token = AuthStrategy::select(RunMode::Serve, None)
        .resolve_token(&client, &state.config, owner)
        .await
        .map_err(|e| ToolError::InvalidParams(format!("GitHub auth failed: {e}")))?;

    let diff_source = DiffSource::Github {
        owner: owner.to_string(),
        repo: repo.to_string(),
        pr,
        token,
    };

    let deps = deps_from_state(state, &reviewer_model);
    let input = ReviewInput {
        diff_source,
        reviewer_model: reviewer_model.clone(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::ForceDryRun,
        run_mode: RunMode::Serve,
        allow_posting: false,
    };

    info!(owner, repo, pr, reviewer_model, "mcp: review_pr");
    let result = run_review(&state.config, input, deps).await;
    Ok(wrap_result(&result))
}

// ─── review_diff ─────────────────────────────────────────────────────────────

/// Execute the `review_diff` tool.
///
/// Why: lets Claude Code pass a raw diff (e.g. from `git diff`) directly to the
/// review pipeline without requiring a GitHub PR.
/// What: writes the diff (plus optional context header) to a named temp file,
/// then runs the pipeline with `DiffSource::LocalFile`.  The temp file is
/// cleaned up when it is dropped (via `NamedTempFile`'s `Drop`).
/// Test: `review_diff_returns_review_result_envelope`.
async fn call_review_diff(args: &Value, state: &AppState) -> Result<Value, ToolError> {
    let diff = require_str(args, "diff")?;
    let context = args.get("context").and_then(Value::as_str).unwrap_or("");
    let reviewer_model = args
        .get("reviewer_model")
        .and_then(Value::as_str)
        .unwrap_or(&state.config.role_models.reviewer.model)
        .to_string();

    // Write diff to a temp file so DiffSource::LocalFile can read it.
    let mut tmp = NamedTempFile::new()
        .map_err(|e| ToolError::InvalidParams(format!("failed to create temp file: {e}")))?;

    if !context.is_empty() {
        writeln!(tmp, "# Context: {context}")
            .map_err(|e| ToolError::InvalidParams(format!("temp file write error: {e}")))?;
    }
    tmp.write_all(diff.as_bytes())
        .map_err(|e| ToolError::InvalidParams(format!("temp file write error: {e}")))?;
    tmp.flush()
        .map_err(|e| ToolError::InvalidParams(format!("temp file flush error: {e}")))?;

    let path = tmp.path().to_path_buf();
    let diff_source = DiffSource::LocalFile { path };

    let deps = deps_from_state(state, &reviewer_model);
    let input = ReviewInput {
        diff_source,
        reviewer_model: reviewer_model.clone(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::ForceDryRun,
        run_mode: RunMode::Serve,
        allow_posting: false,
    };

    info!(bytes = diff.len(), reviewer_model, "mcp: review_diff");
    let result = run_review(&state.config, input, deps).await;
    // `tmp` is dropped here — temp file cleaned up automatically.
    Ok(wrap_result(&result))
}

// ─── review_health ────────────────────────────────────────────────────────────

/// Execute the `review_health` tool.
///
/// Why: gives Claude Code a quick way to verify that the service is reachable
/// AND that inference is working before issuing a real review (closes #719).
/// MPM uses this to gate `review_pr` calls so it never attempts a full review
/// when the LLM endpoint is down or credentials are expired.  #722 extends the
/// status decision to factor in required-dep reachability so callers that gate
/// on the top-level `status` field get an accurate signal even when only the
/// search dep is down.
/// What: probes the search dep (non-blocking health call) and the inference
/// endpoint (via the cached `InferenceProbe`); computes `status` via the shared
/// `compute_status` helper so the HTTP and MCP paths are always consistent;
/// returns a JSON health snapshot with `status` (`"ok"` or `"degraded"`),
/// `inference`, `dry_run`, `reviewer_model`, and a `deps` object with
/// `reachable` flags for each dep.  When inference is not `"ok"` OR a required
/// dep is unreachable, `status` becomes `"degraded"`.
/// Test: `review_health_inference_ok`, `review_health_inference_auth_error_degraded`,
/// `review_health_required_dep_down_degraded`, `review_health_optional_dep_down_ok`.
async fn call_review_health(state: &AppState) -> Value {
    let reviewer_model = state.config.role_models.reviewer.model.clone();

    // Non-blocking dep probes — same logic as the HTTP /health handler.
    let search_reachable = state.search.health().await.is_ok_and(|r| r.is_healthy());
    let analyze_reachable = match &state.analyze {
        Some(a) => a.health().await.is_ok(),
        None => false,
    };

    // Cached inference-reachability probe (#719).
    let inference = state
        .inference_probe
        .probe(&state.llm, &reviewer_model)
        .await;

    // Build the deps struct so compute_status can inspect required flags (#722).
    let deps = DepStatus {
        trusty_search: DepInfo {
            required: true,
            reachable: search_reachable,
        },
        trusty_analyze: DepInfo {
            required: false,
            reachable: analyze_reachable,
        },
    };

    // #722: status is "degraded" when inference fails OR any required dep is down.
    let status = compute_status(inference, &deps);

    let result = serde_json::json!({
        "status": status,
        "version": env!("CARGO_PKG_VERSION"),
        "dry_run": state.config.dry_run,
        "reviewer_model": reviewer_model,
        "inference": inference,
        "deps": {
            "trusty_search": {
                "required": deps.trusty_search.required,
                "reachable": deps.trusty_search.reachable,
            },
            "trusty_analyze": {
                "required": deps.trusty_analyze.required,
                "reachable": deps.trusty_analyze.reachable,
            },
        },
    });
    wrap_value(&result)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Build `ReviewDeps` from the shared `AppState`, substituting the reviewer
/// model from the tool arguments when provided.
///
/// Why: all three tools need the same deps structure; factoring it out avoids
/// repetition across the three handlers.
/// What: clones `Arc` handles from `state`; does not allocate new providers.
/// Test: covered transitively by tool handler tests.
fn deps_from_state(state: &AppState, _reviewer_model: &str) -> ReviewDeps {
    ReviewDeps {
        llm: Arc::clone(&state.llm),
        verifier: state.verifier.clone(),
        search: Arc::clone(&state.search),
        analyze: state.analyze.clone(),
        dedup: state.dedup.clone(),
    }
}

/// Extract a required string field from the tool arguments.
///
/// Why: avoids boilerplate `ok_or_else` chains in every tool handler.
/// What: returns `&str` on success; `ToolError::InvalidParams` on missing/wrong type.
/// Test: `missing_field_returns_invalid_params`.
fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidParams(format!("missing or non-string '{key}'")))
}

/// Wrap a `ReviewResult` in the MCP content envelope.
///
/// Why: MCP `tools/call` responses must carry results inside a `content[]` array
/// (per MCP spec) so the LLM can render them correctly.
/// What: serialises `ReviewResult` to a pretty JSON string, wraps it in a text
/// content block.
/// Test: result shape verified by `review_health_does_not_require_creds`.
fn wrap_result(result: &ReviewResult) -> Value {
    let text = serde_json::to_string_pretty(result)
        .unwrap_or_else(|_| serde_json::to_string(result).unwrap_or_default());
    serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    })
}

/// Wrap an arbitrary JSON value in the MCP content envelope.
///
/// Why: `review_health` returns a free-form JSON object; the same envelope
/// format applies.
/// What: serialises to pretty JSON string inside a text content block.
/// Test: used by `review_health_does_not_require_creds`.
fn wrap_value(value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    })
}

/// Wrap a tool execution error in the MCP in-band error envelope.
///
/// Why: per MCP spec, tool execution failures use `isError: true` with a text
/// content block rather than a JSON-RPC error object — the protocol error space
/// is reserved for malformed requests / unknown tools.
/// What: wraps the error message in the standard MCP error envelope.
/// Test: `call_unknown_tool_returns_error`.
pub fn wrap_tool_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": format!("Error: {msg}") }],
        "isError": true,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────
// Split across two test modules to keep each file under the 500-line cap.
//  - `tools_tests.rs`          — descriptors, helpers, review_health (#719/#722)
//  - `tools_dispatch_tests.rs` — call_tool dispatch: review_diff / review_pr (#949)

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tools_dispatch_tests.rs"]
mod dispatch_tests;
