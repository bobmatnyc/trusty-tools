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
//!
//! ## Cross-product specialists (epic #4021, #4026/#4028)
//!
//! The tool now also accepts an OPTIONAL `specialist` name, routing the task
//! to that named external non-coding agent instead of the default opaque
//! target. Two rules govern it, both enforced here rather than at the caller:
//!
//! - **#4026, fail-closed allow-set.** A named specialist is resolved through
//!   `cross_product::SubagentAllowSet` — the intersection of the bridge's own
//!   `NON_CODING_TARGETS` floor with the calling agent's `[subagents].allowed`
//!   config. The default is EMPTY: omitting `with_allow_set` disables named
//!   targeting entirely. A denial returns before `backend.run`, so nothing is
//!   dispatched. Per the owner's OQ-7 ruling this is never caller-trusted;
//!   #4030's domain authority will later feed the same seam.
//! - **#4028, propose-not-authorize.** A named specialist's result is wrapped
//!   in `cross_product::ProposalEnvelope` (origin agent, target agent, the
//!   CALLER's authority tier, and a `Disposition` marker that is always
//!   `Proposal`) per DOC-41 §5.5 line 1398 and #3078's AUTH-5 pattern. Holding
//!   `user_authority` is recorded, never an upgrade: the caller acts on the
//!   proposal in its own turn, under its own identity.
//!
//! Omitting `specialist` is byte-identical to pre-#4026 behaviour: same route
//! derivation, same default target, same bare scrubbed transcript back.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};

use crate::intent::route::{BridgeRoute, route_task};
use crate::rbac::ServiceTier;
use crate::tools::cross_product::{
    CallerAuthority, HandoffContext, ProposalEnvelope, SubagentAllowSet,
};
use crate::tools::pm_bridge_backend::PmBridgeBackend;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Envelope `origin_agent` used when a registration site names none.
///
/// Why: `ProposalEnvelope` must always identify an origin, but the older
/// registration sites have no agent identity to hand in. A generic, non-
/// branded placeholder keeps the envelope honest without leaking anything.
/// What: the literal used unless `with_origin` supplies a real name.
/// Test: `envelope_carries_origin_target_and_authority`.
const DEFAULT_ORIGIN_AGENT: &str = "assistant";

/// `dispatch_task` — hands a task to whichever backend `route_task` decides,
/// via an injected `PmBridgeBackend`.
pub struct PmBridgeTool {
    backend: Arc<dyn PmBridgeBackend>,
    restricted: Vec<ServiceTier>,
    // #4026: the fail-closed, bridge-owned allow-set for named cross-product
    // specialists. EMPTY by default — see `with_allow_set`.
    allow_set: SubagentAllowSet,
    // #4028: envelope provenance/authority for cross-product results.
    origin_agent: String,
    caller_authority: CallerAuthority,
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
            // #4026: EMPTY allow-set by default — constructing the tool grants
            // no cross-product reach whatsoever.
            allow_set: SubagentAllowSet::empty(),
            origin_agent: DEFAULT_ORIGIN_AGENT.to_string(),
            caller_authority: CallerAuthority::Standard,
        }
    }

    /// Attach the calling agent's cross-product allow-set (#4026).
    ///
    /// Why: OQ-7 puts enforcement at the bridge, fail-closed. Injecting the
    /// resolved set at REGISTRATION time (from the caller's
    /// `[subagents].allowed`, see `agents::config::SubagentsConfig`) means
    /// `execute` never consults anything the LLM can influence mid-turn, and
    /// #4030's domain authority can later feed the same seam without touching
    /// this tool.
    /// What: replaces the default EMPTY set. Omitting this call leaves named
    /// specialist targeting fully disabled — the no-silent-grant default.
    /// Test: `named_specialist_is_denied_when_no_allow_set_is_configured`,
    /// `named_specialist_reaches_the_backend_when_allowed`.
    pub fn with_allow_set(mut self, allow_set: SubagentAllowSet) -> Self {
        self.allow_set = allow_set;
        self
    }

    /// Attach the envelope's origin identity and the CALLER's authority tier
    /// (#4028).
    ///
    /// Why: `ProposalEnvelope` records who dispatched and at what tier so an
    /// authority-holder can recognise that IT, in its own turn, is the party
    /// that may act on a returned proposal (DOC-41 §5.5). The tier is supplied
    /// by the registration site rather than read here because `user_authority`
    /// has no `AgentConfig` field yet (reserved for #3074/AUTH-1) — this is
    /// the seam that will read it when it lands, with no new tier invented.
    /// What: sets `origin_agent` (blank falls back to the generic default) and
    /// `caller_authority`.
    /// Test: `envelope_carries_origin_target_and_authority`.
    pub fn with_origin(
        mut self,
        origin_agent: impl Into<String>,
        caller_authority: CallerAuthority,
    ) -> Self {
        let origin = origin_agent.into();
        if !origin.trim().is_empty() {
            self.origin_agent = origin;
        }
        self.caller_authority = caller_authority;
        self
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
                        },
                        // #4026: optional named specialist. Described without
                        // naming any backend or enumerating the roster (the
                        // #3052 PR A CRITICAL-2 rule) — the caller's own
                        // instructions are the source of legitimate names.
                        "specialist": {
                            "type": "string",
                            "description": "Optional name of a non-coding specialist to hand this to, when your own instructions name one. Omit to let the system choose how to execute the task."
                        },
                        // #4028: optional HandoffContext-shaped payload.
                        "handoff": {
                            "type": "object",
                            "description": "Optional context to hand along with the task. Keep it small; it is capped.",
                            "properties": {
                                "summary": { "type": "string" },
                                "relevant_state": { "type": "string" },
                                "constraints": { "type": "array", "items": { "type": "string" } }
                            },
                            "additionalProperties": false
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

        // #4028: validate the handoff BEFORE anything is dispatched, so an
        // over-cap payload is a recoverable caller error and the target is
        // never invoked.
        let handoff = match args.get("handoff") {
            None | Some(Value::Null) => HandoffContext::default(),
            Some(raw) => match serde_json::from_value::<HandoffContext>(raw.clone()) {
                Ok(h) => h,
                Err(e) => return ToolResult::err(format!("dispatch_task: invalid 'handoff': {e}")),
            },
        };
        if let Err(size) = handoff.validate() {
            return ToolResult::err(format!(
                "dispatch_task: 'handoff' is too large ({size} bytes; limit {}) — nothing was dispatched",
                crate::tools::cross_product::HANDOFF_MAX_BYTES
            ));
        }

        // #4026: resolve the named specialist against the fail-closed
        // bridge-layer allow-set. A denial returns BEFORE `backend.run`, so
        // nothing is dispatched.
        let requested = args
            .get("specialist")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let target = match requested {
            Some(name) => match self.allow_set.resolve(name) {
                Ok(resolved) => Some(resolved),
                Err(denied) => return ToolResult::err(format!("dispatch_task: {denied}")),
            },
            None => None,
        };

        let body = match handoff.render_preamble() {
            Some(preamble) => format!("{preamble}\n{task}"),
            None => task.to_string(),
        };
        // #4026: a named specialist always takes the external-roster leg —
        // that roster is where the non-coding specialists live (#4027), and
        // re-deriving the route would let an unrelated phrase in `task` send
        // a named target somewhere it does not exist.
        let route = match target {
            Some(_) => BridgeRoute::Tcode,
            None => route_task(task),
        };

        match self.backend.run(route, target.as_deref(), &body).await {
            Ok(out) => {
                let scrubbed = scrub_branding(&out);
                match target {
                    // #4028: a cross-product specialist's result is wrapped in
                    // the propose-only envelope — DOC-41 §5.5 line 1398.
                    Some(name) => ToolResult::ok(
                        ProposalEnvelope::for_cross_product(
                            self.origin_agent.clone(),
                            name,
                            self.caller_authority,
                            scrubbed,
                        )
                        .render(),
                    ),
                    // Unnamed dispatch is the pre-#4026 opaque path, returned
                    // byte-identically to before this change.
                    None => ToolResult::ok(scrubbed),
                }
            }
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
/// Test: `scrub_branding_removes_the_real_tm_launch_banner_plain_fallback`,
/// `scrub_branding_removes_the_real_tm_launch_banner_box_form`,
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
