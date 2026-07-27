//! `delegate_to_agent` tool — dispatches a task to a named specialist agent,
//! shared by every caller: `pm`, `ctrl`, and (since #3052 PR A) the
//! black-boxed assistant-tier personas.
//!
//! Why: Keeps the delegation schema and its executor colocated so every
//! caller's tool registry can register a single type that owns both.
//! Pre-flight validation of `agent_name` against the on-disk agent config
//! directory prevents the LLM from hallucinating a specialist name (e.g.
//! inventing `code-searcher` from the `search_code` native tool description,
//! see #204) and crashing the subprocess runner with a confusing IO error.
//! What: `DelegateToAgentTool` wraps an `AgentRunner` and (optionally) an
//! agent config directory. `execute()` parses `{agent_name, task}`, validates
//! the agent TOML exists, and hands off to the runner. On miss, returns a
//! GENERIC error (#3052 PR A code-critic CRITICAL-2: no on-disk roster
//! enumeration — the caller's own instructions, not this shared tool, are
//! the source of truth for which specialist names are legitimate, and this
//! tool's output must stay safe to surface from a black-boxed persona). The
//! tool's schema `description` is likewise generic (CRITICAL-1: no hardcoded
//! internal agent-name examples) — `pm` still knows its roster via its own
//! system prompt's `{{available_agents}}` template substitution
//! (`agents::registry::roster::inject_roster_into_prompt`), which is
//! independent of this schema.
//! Test: `unknown_agent_is_rejected_without_naming_the_agent_or_roster`
//! asserts the error names the REJECTED agent but leaks no on-disk roster
//! entry and no bundled agent filename/internal system name.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{AgentRunner, ToolExecutor, ToolResult};

/// Tool executor that delegates a task to a named specialist agent.
pub struct DelegateToAgentTool {
    runner: Arc<dyn AgentRunner>,
    /// Candidate directories searched (in order) for `<agent>.toml` /
    /// `<agent>/agent.toml` / `<agent>.md` during pre-flight `agent_name`
    /// validation. When non-empty, `execute()` rejects calls whose
    /// `agent_name` resolves in NONE of them, with a generic error (#3052 PR
    /// A: no on-disk roster enumeration — see the module docs). When empty
    /// (legacy callers / tests via `new()` alone), validation is skipped and
    /// the runner is invoked directly.
    ///
    /// #3555 delegate-resolve follow-up: this used to be a single
    /// `Option<PathBuf>`, populated by `build_assistant_tier_registry` with
    /// only the CWD-relative `.trusty-agents/agents` — invisible to the
    /// bundled worker roster deployed at `$HOME/.trusty-agents/agents/`.
    /// Widening to a `Vec` lets callers pass the same multi-tier list
    /// (`agents::agents_dir_candidates()`) the actual spawn resolves
    /// against, so validation and spawn can never diverge.
    config_dirs: Vec<PathBuf>,
    /// Fail-closed allowlist of TARGET agent `role` strings this instance may
    /// delegate to. `None` (the default / `pm`/`ctrl` callers) performs no
    /// role check — the full on-disk roster is a legitimate target for those
    /// callers. `Some(roles)` requires the resolved target's `agent.role` to
    /// be a member of `roles`; anything else (including a name that fails to
    /// resolve at all) is rejected with the SAME generic error `config_dirs`
    /// validation already returns, before any resolution artifact or role
    /// name is ever surfaced.
    ///
    /// Why (#3555 CRITICAL follow-up, code-critic): widening `config_dirs` to
    /// include the `$HOME` fallback tier means an assistant-tier
    /// `delegate_to_agent` can now resolve ANY bundled agent from that tier —
    /// including `pm` (role `orchestrator`), `ctrl` (role `controller`),
    /// `observe-agent` (role `observer`), and `postmortem-agent`/
    /// `analysis-agent` (role `analysis`). `run_subagent`'s tool registry is
    /// built from the SPAWNED child's own role
    /// (`build_registry_for_agent`/`build_assistant_tier_registry`), so
    /// spawning `pm` or `ctrl` from an assistant-tier call would hand the
    /// resulting subprocess an UNRESTRICTED orchestrator/controller registry
    /// (shell, `write_file`, unrestricted `delegate_to_agent`) — a privilege
    /// escalation straight out of the sandboxed assistant, and a reopening
    /// of the internal-orchestration-leak class #3052 PR A closed. This field
    /// gates that at the SAME choke point every other pre-flight check
    /// already lives at, before resolution or spawn.
    /// What: See `with_allowed_target_roles`.
    /// Test: `delegate_assistant_role_gate_rejects_orchestrator_role`,
    /// `delegate_assistant_role_gate_rejects_controller_role`,
    /// `delegate_assistant_role_gate_allows_worker_role`,
    /// `delegate_assistant_role_gate_allows_peer_assistant_role`.
    allowed_target_roles: Option<Vec<String>>,
    /// This instance's OWN privilege tier — the DELEGATOR's tier, not the
    /// target's (#4169, epic #4167 — one-directional L0/L1 delegation gate).
    ///
    /// Why: L0 (orchestration) personas carry PM-tier grants under a YOLO
    /// posture the owner explicitly accepts; L1 (standard) personas are the
    /// documented black-box tier and may ingest untrusted content (Gmail/
    /// Drive/web). If an L1 persona could delegate INTO an L0 target,
    /// untrusted content would acquire the owner's L0 grant just by asking
    /// the L1 persona to hand the work down — issue #4126's class,
    /// reachable through a new path. Privilege must never be acquirable via
    /// delegation, so this check is layered ON TOP of (never a replacement
    /// for) the existing taint-composition machinery (#4161's
    /// `compose_delegation_taint`) and the `allowed_target_roles` gate
    /// above — an independent dimension, checked whenever EITHER
    /// `allowed_target_roles` is set OR this field is explicitly set (see
    /// `execute`'s `needs_full_resolution`).
    /// What: `None` (the default — every caller that constructs a
    /// `DelegateToAgentTool` and never calls `with_delegator_tier`, which
    /// includes `pm`/`ctrl`-as-orchestrator's own unrestricted delegation
    /// and pre-#4169 tests exercising unrelated resolution behavior with
    /// bare/incomplete fixture TOMLs) means "this instance was never asked
    /// to enforce the tier gate" — byte-identical to pre-#4169 behavior,
    /// including the cheap existence-only pre-flight check when
    /// `allowed_target_roles` is ALSO unset. `Some(tier)` (every real
    /// assistant-tier / persona-chat construction site in this crate) means
    /// "enforce the gate using `tier` as this delegator's own tier" and
    /// forces full `AgentConfig` resolution so the target's tier can be
    /// read. When `allowed_target_roles` IS set but this field was left
    /// `None` (a caller that opted into role-gating but forgot tier), the
    /// check still runs with a FAIL-CLOSED `AgentTier::L1Standard` default
    /// (via `unwrap_or_default()`) — belt-and-suspenders: the population
    /// that cares about role-gating can never silently skip tier-gating
    /// too. Set explicitly via `with_delegator_tier`.
    /// Test: `delegate_l1_to_l0_is_refused`, `delegate_l0_to_l1_succeeds`,
    /// `delegate_l1_to_l1_is_unaffected_by_tier_gate`,
    /// `delegate_multi_hop_l1_peer_to_l0_is_refused`,
    /// `delegate_tier_gate_applies_even_without_role_allowlist`,
    /// `delegate_role_gate_without_declared_tier_fails_closed_to_l1`,
    /// `no_tier_and_no_role_gate_preserves_cheap_existence_check`,
    /// `delegator_tier_defaults_to_none_on_construction`.
    delegator_tier: Option<crate::agents::AgentTier>,
    /// #3737 (per-message chat attribution, epic #3052): the id of the task
    /// whose turn this tool is delegating within, used as the `session_id` on
    /// the `AgentSpawned` event emitted on a successful delegation so the API
    /// server can attribute the answer to the agent that actually produced it
    /// (base "Assistant" delegating a weather question to Izzie → the bubble
    /// reads "Izzie", not "Assistant").
    ///
    /// Why: `AgentSpawned`/`PmDelegating` were previously defined and consumed
    /// (REPL display) but never emitted in the in-process dispatch path, so
    /// there was no server-authoritative signal of WHO answered a delegated
    /// turn. Emitting from this single shared delegation choke point makes the
    /// signal real for every caller that opts in via `with_session_id`.
    /// `None` (every existing caller / test) emits nothing — behavior is
    /// byte-identical to before this field existed.
    /// What: See `with_session_id`.
    /// Test: `delegate_emits_agent_spawned_with_session_id`.
    session_id: Option<String>,
}

impl DelegateToAgentTool {
    /// Construct with an injected `AgentRunner`.
    ///
    /// Why: Lets tests substitute an in-process mock runner without touching
    /// production subprocess code.
    /// What: Stores the `Arc<dyn AgentRunner>` for later dispatch. No
    /// pre-flight name validation is performed unless `with_config_dir`/
    /// `with_config_dirs` is also called.
    /// Test: `DelegateToAgentTool::new(Arc::new(MockRunner))` compiles and
    /// yields a tool whose `name()` is `delegate_to_agent`.
    pub fn new(runner: Arc<dyn AgentRunner>) -> Self {
        Self {
            runner,
            config_dirs: Vec::new(),
            allowed_target_roles: None,
            delegator_tier: None,
            session_id: None,
        }
    }

    /// Attach the current task's session id so a successful delegation emits
    /// an `AgentSpawned` event the API server can attribute the answer to
    /// (#3737).
    ///
    /// Why: The in-process chat dispatch (`run_pm_task_with_history` /
    /// `run_pm_task_with_persona`) knows the task's `session_id`; threading it
    /// into this tool lets the tool publish `AgentSpawned { session_id, agent }`
    /// the moment it hands work to a specialist, which `submit_task`'s event
    /// listener captures as the turn's `responder_agent`. Without it there is
    /// no server-side record of who actually answered a delegated turn.
    /// What: Stores `sid`. A blank string is treated as unset (no emit).
    /// Test: `delegate_emits_agent_spawned_with_session_id`,
    /// `session_id_gate_normalizes_blank_to_none`.
    pub fn with_session_id(mut self, sid: impl Into<String>) -> Self {
        let sid = sid.into();
        self.session_id = if sid.trim().is_empty() {
            None
        } else {
            Some(sid)
        };
        self
    }

    /// Attach a SINGLE agent config directory used for pre-flight
    /// `agent_name` validation.
    ///
    /// Why: When the LLM hallucinates an agent name (e.g. `code-searcher`
    /// from the `search_code` native tool, #204), spawning the subprocess
    /// fails with a generic IO error. Validating up front returns a
    /// structured, GENERIC `ToolResult::err` so the LLM can self-correct on
    /// the next turn by re-checking its OWN instructions for the specialists
    /// legitimately available to it — this tool does not enumerate the
    /// on-disk roster (#3052 PR A code-critic CRITICAL-2).
    /// What: Sets `config_dirs` to the single-element `[dir]`. Kept as the
    /// primary constructor for callers (`pm`, `ctrl`) that intentionally
    /// validate against exactly one project-scoped roster and must NOT fall
    /// back to any other tier. Use `with_config_dirs` for a multi-tier
    /// search (e.g. the assistant tier's bundled-roster fallback).
    /// Test: `unknown_agent_is_rejected_without_naming_the_agent_or_roster`.
    pub fn with_config_dir(mut self, dir: PathBuf) -> Self {
        self.config_dirs = vec![dir];
        self
    }

    /// Attach MULTIPLE candidate agent config directories, searched in order,
    /// for pre-flight `agent_name` validation.
    ///
    /// Why (#3555 delegate-resolve follow-up): the assistant tier must
    /// delegate to the bundled worker roster (`engineer`, `qa-agent`, …)
    /// regardless of the invoking CWD. `AgentConfig::by_name` already
    /// resolves this way via `agents_dir_candidates()` (CWD/`TAGENT_CONFIG_DIR`
    /// tier, then `$HOME/.trusty-agents/agents`, which
    /// `agents::bundled::ensure_bundled_agents_deployed` populates at
    /// startup) — a single-directory check here would reject names the
    /// actual spawn resolves fine.
    /// What: Sets `config_dirs` to `dirs` verbatim. An empty `Vec` behaves
    /// identically to the default (unset) constructor — validation skipped.
    /// Test: `delegate_resolves_agent_from_secondary_config_dir`,
    /// `delegate_rejects_agent_absent_from_all_config_dirs`.
    pub fn with_config_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.config_dirs = dirs;
        self
    }

    /// Restrict delegation targets to agents whose resolved `agent.role` is
    /// in `roles` — a fail-closed allowlist gate (#3555 CRITICAL follow-up).
    ///
    /// Why: See the field doc on `allowed_target_roles`. `pm`/`ctrl` never
    /// call this — the full roster is a legitimate delegation target for
    /// them; only `build_assistant_tier_registry` wires it, so an
    /// assistant-tier persona can genuinely act on "bring in a specialist"
    /// (or consult a peer assistant) WITHOUT being able to spawn an
    /// orchestrator/controller/observer subprocess with an unrestricted tool
    /// registry.
    /// What: Stores `roles`. `execute()` resolves the target's `AgentConfig`
    /// (via `config_dirs`) and rejects with the SAME generic "not a
    /// recognized specialist" error used for a missing agent, whether the
    /// name fails to resolve at all OR resolves to a role outside `roles` —
    /// the caller never learns which case it hit, so no role taxonomy or
    /// on-disk roster leaks.
    /// Test: `delegate_assistant_role_gate_rejects_orchestrator_role`,
    /// `delegate_assistant_role_gate_rejects_controller_role`,
    /// `delegate_assistant_role_gate_allows_worker_role`,
    /// `delegate_assistant_role_gate_allows_peer_assistant_role`.
    pub fn with_allowed_target_roles(mut self, roles: Vec<String>) -> Self {
        self.allowed_target_roles = Some(roles);
        self
    }

    /// Declare THIS instance's own delegator tier (#4169, epic #4167).
    ///
    /// Why: See the `delegator_tier` field doc for the full rationale.
    /// Every production call site that constructs a `DelegateToAgentTool`
    /// for an assistant-tier (persona) caller must call this with that
    /// caller's own resolved `AgentInfo::tier()` so the one-directional
    /// L1->L0 gate has something to check against. Callers representing a
    /// caller OUTSIDE the persona tier model entirely — `pm`/`ctrl`-as-
    /// orchestrator, already fully trusted and already unaffected by
    /// `allowed_target_roles` — pass [`crate::agents::AgentTier::L0Orchestration`]
    /// explicitly to mean "this caller is not gated by the L0/L1 boundary",
    /// not because it IS an L0 persona.
    /// What: Stores `Some(tier)`, which — combined with `config_dirs` being
    /// non-empty — forces full target resolution so the tier gate can run.
    /// Test: `delegate_l1_to_l0_is_refused`, `delegate_l0_to_l1_succeeds`.
    pub fn with_delegator_tier(mut self, tier: crate::agents::AgentTier) -> Self {
        self.delegator_tier = Some(tier);
        self
    }
}

#[async_trait]
impl ToolExecutor for DelegateToAgentTool {
    fn name(&self) -> &str {
        "delegate_to_agent"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "delegate_to_agent",
                "description": "Hand a task off to a specialist so it actually gets done. Use this for any implementation work (writing code, running analysis, etc.) rather than doing it yourself. The specialist runs independently and its result is returned to you. NOTE: agent_name must identify an actual specialist you know about (see your own instructions for which ones are available to you) — native tools like search_code, web_search, move_file, create_dir are NOT specialist names — call them directly instead.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "agent_name": {
                            "type": "string",
                            "description": "The specialist to hand this task to, by its internal name. Must be one of the specialists available to you (see your own instructions); native tools are not specialist names."
                        },
                        "task": {
                            "type": "string",
                            "description": "Concrete task description for the specialist."
                        }
                    },
                    "required": ["agent_name", "task"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(agent_name) = args.get("agent_name").and_then(Value::as_str) else {
            return ToolResult::err("delegate_to_agent: missing 'agent_name'");
        };
        let Some(task) = args.get("task").and_then(Value::as_str) else {
            return ToolResult::err("delegate_to_agent: missing 'task'");
        };

        // Pre-flight validation (#204): if any config_dirs were attached,
        // verify the agent resolves in at least one of them before spawning
        // a subprocess. This converts a generic "subprocess failed" IO error
        // into a structured tool error the LLM can act on.
        //
        // #3052 PR A code-critic CRITICAL-2: the error message MUST NOT
        // enumerate the on-disk agent roster — this tool is now reachable
        // from black-boxed assistant-tier personas, and `delegate.rs` is
        // shared by every caller (pm, ctrl, assistant, ...), so the message
        // is sent straight back into whichever persona's turn is in
        // progress. A generic "not recognized" response is enough for the
        // LLM to self-correct (re-check its own instructions for the
        // specialists actually available to it) without the tool itself
        // dumping internal agent IDs/filenames into a black-boxed
        // conversation. `available_agents()` is kept (used by tests only
        // now) rather than deleted, since a future caller-gated surface
        // (e.g. an internal-only diagnostic) may still want it.
        //
        // #3555 delegate-resolve follow-up: uses `agents::agent_name_resolves`
        // (package/flat-toml/flat-md tiers) across ALL `config_dirs`, not a
        // single hand-rolled directory — see the field doc on `config_dirs`.
        // The failure reason is logged at `debug` here (content never left
        // the tool before #3555 — `is_error=true` with no payload anywhere
        // made this undebuggable in production).
        //
        // #3555 CRITICAL follow-up (code-critic): when `allowed_target_roles`
        // is set (assistant tier only), existence is NOT enough — the
        // TARGET's resolved role must also be in the allowlist, checked
        // BEFORE any resolution artifact is used further or a spawn is
        // attempted. See the field doc on `allowed_target_roles` for why
        // (privilege escalation via delegating to `pm`/`ctrl`/etc.). Both the
        // "unknown name" and "role not allowed" cases return the identical
        // generic message below so neither leaks which one occurred.
        if !self.config_dirs.is_empty() {
            // #4169 (epic #4167): the one-directional L0/L1 delegation gate.
            // `needs_full_resolution` decides whether the cheap
            // existence-only check (pre-#4169 behavior, byte-identical for
            // every caller that opts into NEITHER gate — `pm`/`ctrl`-as-
            // orchestrator's own unrestricted delegation, and any test
            // exercising unrelated resolution behavior with bare fixture
            // TOMLs) is enough, or whether the target must be fully parsed
            // so its role AND tier can be inspected. Triggered by EITHER
            // gate being requested — `allowed_target_roles.is_some()`
            // (unchanged from pre-#4169) OR `delegator_tier.is_some()` (new):
            // a caller that opts into tier-gating must get it even if it
            // never set a role allowlist (this is exactly the persona-chat
            // dispatch path's historical shape — see
            // `ctrl::pm_task::dispatch::persona`).
            let needs_full_resolution =
                self.allowed_target_roles.is_some() || self.delegator_tier.is_some();
            let (rejected, tier_blocked) = if needs_full_resolution {
                // Fail-closed default (#4169 constraint 1): a caller that DID
                // opt into role-gating but never declared its own tier is
                // still checked as if it were L1Standard — the population
                // that cares enough to role-gate can never silently skip
                // tier-gating too.
                let delegator_tier = self.delegator_tier.unwrap_or_default();
                match crate::agents::AgentConfig::by_name_in(&self.config_dirs, agent_name) {
                    Ok(cfg) => {
                        let target_tier = cfg.agent.tier();
                        let tier_blocked = target_tier == crate::agents::AgentTier::L0Orchestration
                            && delegator_tier != crate::agents::AgentTier::L0Orchestration;
                        if tier_blocked {
                            tracing::warn!(
                                agent_name,
                                delegator_tier = ?delegator_tier,
                                target_tier = ?target_tier,
                                "delegate_to_agent: refusing an L1-tier delegation into an \
                                 L0-orchestration target — privilege cannot be acquired via \
                                 delegation"
                            );
                        }
                        let role_ok = match &self.allowed_target_roles {
                            Some(allowed_roles) => {
                                let allowed = allowed_roles.iter().any(|r| r == &cfg.agent.role);
                                if !allowed {
                                    tracing::debug!(
                                        agent_name,
                                        target_role = %cfg.agent.role,
                                        allowed_roles = ?allowed_roles,
                                        "delegate_to_agent: target role not in the assistant-tier allowlist"
                                    );
                                }
                                allowed
                            }
                            None => true,
                        };
                        (tier_blocked || !role_ok, tier_blocked)
                    }
                    Err(e) => {
                        tracing::debug!(
                            agent_name,
                            error = %format!("{e:#}"),
                            "delegate_to_agent: gated target failed to resolve"
                        );
                        (true, false)
                    }
                }
            } else if !crate::agents::agent_name_resolves(&self.config_dirs, agent_name) {
                tracing::debug!(
                    agent_name,
                    config_dirs = ?self.config_dirs,
                    "delegate_to_agent: agent not found in any candidate directory"
                );
                (true, false)
            } else {
                (false, false)
            };
            if tier_blocked {
                // #4169 AC: "error message educates the user on the
                // constraint" — deliberately DISTINCT from the generic
                // roster-hiding message below. Unlike the anti-roster-leak
                // rationale for an unknown-name/disallowed-role rejection
                // (which must never confirm internal role taxonomy exists),
                // the L0/L1 tier split is a product-level concept the owner
                // named directly; explaining it here does not enumerate any
                // OTHER roster entry, only the one name the caller already
                // supplied.
                return ToolResult::err(format!(
                    "'{agent_name}' cannot be delegated to: it is an orchestration-tier \
                     (L0) specialist, and this persona is standard-tier (L1). L1 personas \
                     may not delegate to L0 orchestration targets — this boundary is \
                     structural and is never bypassable, including through an \
                     intermediate delegation hop."
                ));
            }
            if rejected {
                return ToolResult::err(format!(
                    "'{agent_name}' is not a recognized specialist. Check your own \
                     instructions for the specialists available to you — native tools \
                     (search_code, web_search, move_file, create_dir, memory_store, \
                     memory_recall, etc.) are NOT specialist names; call them directly \
                     as tools instead of via delegate_to_agent."
                ));
            }
        }

        // Detect coding persona + language idiom skill from the task and
        // agent name. Strips any explicit `[persona]` tag before forwarding so
        // the sub-agent sees a clean task body, then prepends the matching
        // skill bodies as `## Language Conventions` / `## Persona Directive`
        // sections. See `crate::agents::persona` for the detection rules.
        let final_task = crate::agents::persona::assemble_task_with_context(agent_name, task);
        match self.runner.run(agent_name, &final_task).await {
            // PM gets the full content (it may want to inspect code sections).
            Ok(out) => {
                // #3737: a successful delegation is the server-authoritative
                // record of who actually answered this turn. Emit `AgentSpawned`
                // (only when a session id was threaded in) so `submit_task`'s
                // listener captures `agent_name` as the turn's `responder_agent`
                // and the GUI can relabel the bubble accordingly. Emitting on
                // success (not on dispatch) means a delegation that errored and
                // was then answered by the caller itself does not mis-attribute.
                if let Some(sid) = self.session_id.as_deref() {
                    crate::events::publish(crate::events::Event::AgentSpawned {
                        session_id: sid.to_string(),
                        agent: agent_name.to_string(),
                    });
                }
                ToolResult::ok(out.content)
            }
            Err(e) => {
                // #3555 delegate-resolve follow-up: log the full error chain
                // at debug so a spawn/resolution failure is diagnosable
                // locally, even though the ToolResult sent back to the LLM
                // stays generic-safe (it already carries `{e:#}` too, but
                // `tool_loop`'s turn-level log previously logged only
                // `is_error=true` and dropped the content entirely).
                tracing::debug!(
                    agent_name,
                    error = %format!("{e:#}"),
                    "delegate_to_agent: sub-agent run failed"
                );
                ToolResult::err(format!("sub-agent '{agent_name}' failed: {e:#}"))
            }
        }
    }
}

#[cfg(test)]
#[path = "delegate_tests.rs"]
mod tests;
