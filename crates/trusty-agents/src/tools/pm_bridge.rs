//! `dispatch_task` — the opaque tm<->tcode "PM bridge" tool (epic #3052, PR
//! B, lane 3).
//!
//! Why: the assistant needs to hand off orchestration/project/session/
//! issue/PR/multi-agent work to `tm` and direct-coding-in-a-repo work to
//! `tcode` WITHOUT the caller — the user, or the LLM itself — ever learning
//! that either backend, or trusty-mpm/trusty-code by name, exists. Lane 2
//! (`delegate_to_agent`, `crate::tools::delegate`) already handles in-process
//! sub-agent delegation; this is the DISTINCT lane for routing to the two
//! external opaque backends. `route_task` (`crate::intent::route`) decides
//! WHICH backend deterministically and with no I/O so the routing decision
//! itself is unit-testable independent of any LLM or subprocess; this tool
//! is the thin glue that calls it, dispatches to a `PmBridgeBackend`, and
//! scrubs the backend's identity out of both the schema text and the result
//! before either can leak to a black-boxed persona.
//! What: `PmBridgeTool` implements `ToolExecutor` as `dispatch_task`. Its
//! schema never mentions `tm`/`tcode`/`trusty-mpm`/`trusty-code`/"routing".
//! `restricted_tiers()` denies `ServiceTier::ReadOnly` AND
//! `ServiceTier::Analytics` (owner-locked — see epic #3052's PR B decision
//! log) so those tiers never see the tool at all. `execute()` returns the
//! FULL backend transcript (not a summary) run through `scrub_branding`,
//! win or lose — errors are scrubbed too, since a raw backend error (e.g.
//! "failed to spawn tm serve --stdio") would otherwise leak identity through
//! the failure path the owner's spec text didn't explicitly call out.
//! Test: `pm_bridge_tests` — `RecordingBackend` proves routing reaches the
//! right side, `scrub_branding` strips every forbidden token from a sample
//! backend transcript, `name()`/`schema()` stay clean, and an RBAC test
//! proves both denied tiers never see the tool via `filter_tools_for_user`.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};

use crate::intent::route::route_task;
use crate::rbac::ServiceTier;
use crate::tools::pm_bridge_backend::PmBridgeBackend;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `dispatch_task` — hands a task to whichever backend `route_task` decides,
/// via an injected `PmBridgeBackend`.
pub struct PmBridgeTool {
    backend: Arc<dyn PmBridgeBackend>,
    restricted: Vec<ServiceTier>,
}

impl PmBridgeTool {
    /// Construct with an injected backend and no RBAC restriction.
    ///
    /// Why: mirrors `DelegateToAgentTool::new` — tests substitute a
    /// `RecordingBackend` without touching the production subprocess/MCP
    /// code; production call sites always chain `with_restricted_tiers`
    /// (see `ctrl::pm_task::dispatch::history` / `runtime::pm_mode`).
    /// What: stores `backend`; `restricted` starts empty (open access) until
    /// `with_restricted_tiers` narrows it.
    /// Test: `dispatch_task_routes_code_task_to_tcode` and siblings.
    pub fn new(backend: Arc<dyn PmBridgeBackend>) -> Self {
        Self {
            backend,
            restricted: Vec::new(),
        }
    }

    /// Attach the RBAC-denied tiers.
    ///
    /// Why: the owner-locked decision denies BOTH `ReadOnly` and
    /// `Analytics` (see the module docs) — a builder method keeps that
    /// list a single source of truth at the registration call site rather
    /// than hardcoded inside this file.
    /// What: replaces `restricted` with `tiers`.
    /// Test: `dispatch_task_denies_read_only_and_analytics_tiers`.
    pub fn with_restricted_tiers(mut self, tiers: Vec<ServiceTier>) -> Self {
        self.restricted = tiers;
        self
    }
}

#[async_trait]
impl ToolExecutor for PmBridgeTool {
    fn name(&self) -> &str {
        "dispatch_task"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "dispatch_task",
                "description": "Hand off a self-contained unit of work — a coding change in a repo, a multi-step coordination or planning task, a status check — so it actually gets done. The system inspects the task and automatically picks the right way to execute it; you never need to say how. Returns the full result once the work completes.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "A concrete, self-contained description of the work to hand off."
                        }
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(task) = args.get("task").and_then(Value::as_str) else {
            return ToolResult::err("dispatch_task: missing 'task'");
        };
        if task.trim().is_empty() {
            return ToolResult::err("dispatch_task: 'task' must not be empty");
        }

        let route = route_task(task);
        match self.backend.run(route, task).await {
            Ok(out) => ToolResult::ok(scrub_branding(&out)),
            // Scrubbed too: a raw backend error can name the process it
            // tried to spawn (see `ProcessPmBridge::run_tcode`/`run_tm`),
            // which would defeat the whole point of a black-boxed tool.
            Err(e) => ToolResult::err(scrub_branding(&format!("dispatch_task failed: {e:#}"))),
        }
    }

    fn restricted_tiers(&self) -> &[ServiceTier] {
        &self.restricted
    }
}

/// Regex matching a tm-style ephemeral tmux session name (`tm-<word>-<word>`)
/// or a UUIDv4-shaped session id — both are backend-identifying artifacts a
/// raw transcript can carry (see `ProcessPmBridge::run_tm`'s pane content /
/// `session_new`'s returned id).
fn session_id_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)\b(tm-[a-z0-9]+-[a-z0-9]+|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\b",
        )
        .expect("session id pattern is a valid static regex")
    })
}

/// Regex matching a standalone backend-identity token: `tm`, `tcode`,
/// `trusty-mpm`/`Trusty MPM`, `trusty-code`/`Trusty Code`. Word-bounded so
/// it does NOT match inside unrelated words (`tmux`, `atm`, `item`,
/// `system`).
///
/// Why (code-critic BLOCK, finding 1): the original pattern required the
/// literal hyphen (`trusty-mpm`), but `tm`'s own launch banner prints the
/// space-separated title-case wordmark `Trusty MPM v{VERSION}` (see
/// `crates/trusty-mpm/src/bin/tm/formatters/banner/mod.rs` and
/// `.../banner/two_panel/mod.rs`'s `render_title_bar`) — and that banner is
/// the FIRST thing `run_tm` captures via `session_activity`, so it leaked
/// straight through `ToolResult::ok` untouched by the hyphen-only pattern.
/// `[\s_-]*` accepts a hyphen, underscore, any run of whitespace, or no
/// separator at all between `trusty` and `mpm`/`code`, so `trusty-mpm`,
/// `trusty mpm`, `Trusty MPM`, and `trustympm` all match the same way.
/// What: see `scrub_branding`'s docs for the two-pass replacement order.
/// Test: `scrub_branding_removes_the_real_tm_launch_banner`,
/// `scrub_branding_removes_every_forbidden_token`.
fn branded_token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(trusty[\s_-]*mpm|trusty[\s_-]*code|tcode|tm)\b")
            .expect("branded token pattern is a valid static regex")
    })
}

/// Strip backend-identity tokens and session-id-shaped artifacts out of a
/// raw backend transcript before it can reach a black-boxed persona.
///
/// Why: `dispatch_task`'s entire premise (owner-locked, epic #3052 PR B) is
/// that the caller never learns which backend ran its task. The router
/// (`route_task`) and the backend (`ProcessPmBridge`) both necessarily know
/// — they spawn processes literally named `tm`/`tcode` — so the boundary
/// where that knowledge must stop is exactly here, right before the tool
/// returns anything to the LLM loop.
/// What: replaces `tm-<word>-<word>` tmux session names and UUID-shaped
/// session ids with `[session]`, THEN replaces standalone (word-bounded,
/// case-insensitive) `tm`/`tcode`/`trusty-mpm`/`trusty-code` tokens with
/// "the system". Session-id substitution runs first so a name like
/// `tm-quiet-falcon` is replaced as one unit rather than leaving a
/// dangling `-quiet-falcon` behind after the shorter `tm` token match.
/// Test: `scrub_branding_removes_every_forbidden_token`,
/// `scrub_branding_redacts_session_identifiers`,
/// `scrub_branding_leaves_unrelated_words_alone`.
pub fn scrub_branding(input: &str) -> String {
    let redacted_sessions = session_id_pattern().replace_all(input, "[session]");
    branded_token_pattern()
        .replace_all(&redacted_sessions, "the system")
        .into_owned()
}

#[cfg(test)]
#[path = "pm_bridge_tests.rs"]
mod pm_bridge_tests;
