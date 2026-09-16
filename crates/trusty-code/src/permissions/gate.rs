//! The runtime permission gate: evaluate, ask, await, time out (#7948).
//!
//! Why: `config` says what an operator wrote and `matcher` says which rule
//! applies; this module turns a tool call into "dispatch it" or "refuse it",
//! including the suspend-and-await path an `ask` rule needs, behind one call
//! at the agent loop's dispatch site.
//! What: `PermissionMode` (the headless escape hatch), `Decision` (the pure
//! evaluation), `Outcome`/`DenySource` (the final answer), `PermissionEvents`
//! (the emit seam `SessionRegistry` implements), `PermissionContext` (the
//! session-scoped half), and `PermissionGate` (one agent's gate).
//!
//! An agent with no `permissions:` block short-circuits to `Allow` after the
//! legacy allowlist check, and [`HARNESS_REGISTERED_TOOLS`] skips that check
//! entirely, so stock bundled agents are unaffected.
//! Test: `permissions::tests::gate_tests` — the whole module.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::agents::config::AgentConfig;
use crate::tools::{
    CLEAR_GOAL_TOOL_NAME, DELEGATE_TO_AGENT_TOOL_NAME, FINISH_TASK_TOOL_NAME,
    RECALL_SESSION_TOOL_NAME, SET_GOAL_TOOL_NAME, USE_SKILL_TOOL_NAME,
};

use super::config::RuleDecision;
use super::matcher::subjects_for_in_root;
use super::protocol::PermissionDecision;
use super::redact::redact_subject;
use super::session::SessionPermissions;

/// Environment fallback for [`PermissionMode`] when no CLI flag is given.
pub const PERMISSION_MODE_ENV: &str = "TCODE_PERMISSION_MODE";

/// How long an unanswered `ask` waits before resolving to deny.
///
/// Why: an `ask` that never resolves wedges the agent loop. Five minutes lets
/// an operator read the prompt; a walked-away-from session then fails safe.
pub const DEFAULT_ASK_TIMEOUT_SECS: u64 = 300;

/// `Event::PermissionResolved.source` for an `ask` that
/// [`PermissionMode::AllowAsks`] approved without a client (#7948).
pub const ALLOW_ASKS_SOURCE: &str = "allow_asks";

/// `Event::PermissionResolved.source` for an `ask` a grant this session already
/// remembers satisfied, with no client round trip (#7948).
///
/// Why: distinct from `client` — nobody was asked this time; the decision is
/// the earlier `allow_for_session` answer being reused.
/// Test: `remembered_grant_emits_a_resolution_event`.
pub const REMEMBERED_SOURCE: &str = "remembered";

/// The tools the HARNESS registers on an agent's behalf, exempt from the legacy
/// `tcode_tools` allowlist (#7948).
///
/// Why: `tcode_tools` is the agent author's vocabulary of capabilities the
/// agent asks for. The run's own machinery — delegation, completion, goal
/// slots, session recall, skill loading — is wired by
/// `run_task::execute_run_task` and `task::executor::run_and_record`
/// unconditionally, whatever the agent file lists. Stock `pm.md` lists neither
/// `delegate_to_agent` nor `set_goal`, so without this exemption the gate
/// refuses the PM's own delegation on both run-task paths.
/// What: consulted ONLY by the allowlist branch of [`PermissionGate::evaluate`],
/// which waives the allowlist's blanket "absent means denied". The
/// `permissions:` map still runs afterwards, so a `delegate_to_agent: deny` an
/// operator wrote still refuses, and a tool NOT named here — `bash`, `edit`,
/// anything an author deliberately left out — is denied exactly as before.
/// Test: `harness_registered_tool_bypasses_the_legacy_allowlist`,
/// `stock_pm_agent_may_call_delegate_to_agent`,
/// `explicit_deny_beats_the_harness_registered_exemption`,
/// `harness_exemption_does_not_widen_an_ordinary_tool`.
pub const HARNESS_REGISTERED_TOOLS: &[&str] = &[
    DELEGATE_TO_AGENT_TOOL_NAME,
    FINISH_TASK_TOOL_NAME,
    SET_GOAL_TOOL_NAME,
    CLEAR_GOAL_TOOL_NAME,
    RECALL_SESSION_TOOL_NAME,
    USE_SKILL_TOOL_NAME,
];

/// Whether `tool` is one [`HARNESS_REGISTERED_TOOLS`] names.
/// Test: `harness_registered_tool_bypasses_the_legacy_allowlist`.
fn is_harness_registered(tool: &str) -> bool {
    HARNESS_REGISTERED_TOOLS.contains(&tool)
}

/// What a run does with an `ask` it cannot put to a client.
///
/// Why: `tcode run-task` and a daemon run with nobody attached cannot prompt.
/// The default refuses; a scripted run that accepts the risk must say so.
/// What: `Default` refuses an unanswerable `ask`; `AllowAsks` treats every
/// `ask` as `allow`, logging and emitting each approval. Neither affects a
/// `deny`.
/// Test: `headless_ask_is_denied`, `allow_asks_mode_permits_an_ask`,
/// `allow_asks_mode_emits_an_audit_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    #[default]
    Default,
    AllowAsks,
}

impl PermissionMode {
    /// Parse a mode string leniently (case-insensitive, `_`/`-` interchangeable).
    /// Test: `permission_mode_parses_allow_asks_spellings`.
    pub fn parse_lenient(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "default" => Some(Self::Default),
            "allow-asks" | "allowasks" => Some(Self::AllowAsks),
            _ => None,
        }
    }

    /// Resolve the effective mode: CLI value, then [`PERMISSION_MODE_ENV`],
    /// then `Default`.
    /// Test: `cli_permission_mode_outranks_the_env_var`.
    pub fn resolve(cli: Option<&str>) -> Self {
        Self::resolve_from(cli, std::env::var(PERMISSION_MODE_ENV).ok().as_deref())
    }

    /// [`PermissionMode::resolve`] with both tiers supplied.
    ///
    /// Why: tests the precedence without mutating the process environment.
    /// What: the first tier that parses wins; an unrecognised value contributes
    /// nothing, and with no usable tier the result is `Default` — never a
    /// widening.
    /// Test: `cli_permission_mode_outranks_the_env_var`,
    /// `env_permission_mode_applies_when_no_flag_is_given`,
    /// `unrecognised_permission_mode_falls_back_to_default`.
    pub fn resolve_from(cli: Option<&str>, env: Option<&str>) -> Self {
        [cli, env]
            .into_iter()
            .flatten()
            .find_map(Self::parse_lenient)
            .unwrap_or_default()
    }
}

/// The pure evaluation of a tool call against an agent's configuration.
///
/// Why: `Ask` is kept distinct from `Deny` because the prompt (#3422) must
/// show the rule text.
/// What: `Deny`/`Ask` carry the deciding rule in `tool` or `tool[arg]` form.
/// Test: `legacy_allowlist_denial_precedes_the_map`, `unmatched_tool_is_allowed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { rule: String },
    Ask { rule: String },
}

/// Why a call was refused, and what `Event::PermissionResolved.source` says.
/// Test: `headless_ask_is_denied`, `timed_out_ask_is_denied`, `client_deny_is_denied`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenySource {
    /// A `deny` rule, or the legacy `tcode_tools` allowlist.
    Policy,
    /// An `ask` with nobody able to answer it.
    Headless,
    /// An `ask` nobody answered in time, or whose request was abandoned.
    Timeout,
    /// An `ask` a client answered with `deny`.
    Client,
}

impl DenySource {
    /// The stable wire word for `Event::PermissionResolved.source`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::Headless => "headless",
            Self::Timeout => "timeout",
            Self::Client => "client",
        }
    }
}

/// The gate's final answer for one tool call.
/// Test: `deny_message_names_the_rule`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Allow,
    Denied { rule: String, source: DenySource },
}

impl Outcome {
    /// The recoverable tool-error text a refusal becomes.
    ///
    /// Why: the model must tell "policy says no" from "nobody was there to say
    /// yes"; the `denied by policy (<rule>)` prefix is stable for grep and tests.
    /// Test: `deny_message_names_the_rule`, `timed_out_ask_is_denied`.
    pub fn message(&self) -> Option<String> {
        match self {
            Outcome::Allow => None,
            Outcome::Denied { rule, source } => Some(match source {
                DenySource::Policy => format!("denied by policy ({rule})"),
                DenySource::Headless => format!("denied by policy ({rule}): headless run"),
                DenySource::Timeout => format!("denied by policy ({rule}): no decision"),
                DenySource::Client => format!("denied by policy ({rule}): denied by client"),
            }),
        }
    }
}

/// The emit seam for the two permission events.
///
/// Why: `SessionRegistry` is the sole assigner of an event's `seq`, so this
/// module publishes through a trait instead of depending on the session layer
/// (the same direction `ToolEventSink` points).
/// Test: `ask_emits_requested_then_resolved`.
pub trait PermissionEvents: Send + Sync {
    /// An `ask` is now waiting on a client. The parameters are independent
    /// event fields, mirroring `SessionRegistry::record_tool_finished`.
    #[allow(clippy::too_many_arguments)]
    fn permission_requested(
        &self,
        session_id: &str,
        request_id: &str,
        agent: &str,
        agent_id: &str,
        tool: &str,
        subject: &str,
        rule: &str,
    );
    /// An `ask` reached a decision (or ran out of time).
    fn permission_resolved(
        &self,
        session_id: &str,
        request_id: &str,
        agent: &str,
        agent_id: &str,
        decision: &str,
        source: &str,
    );

    /// Whether anything is consuming this session's events, and so could see a
    /// prompt and answer it.
    ///
    /// Why (#8100): a daemon `task.run` always holds broker state, so
    /// `PermissionContext::ask.is_some()` says only that a rendezvous EXISTS —
    /// never that a client is on the other end of it. A headless run therefore
    /// sat out the full [`DEFAULT_ASK_TIMEOUT_SECS`] per `ask` rule and then
    /// denied anyway. This is the question that distinguishes the two, asked of
    /// the same object that publishes the prompt.
    /// What: the answer is about `session_id` and no other — a process-wide
    /// reading is wrong, because one watched PM session dispatching headless
    /// `task.run` sub-agents would then mark every sub-agent as watched and
    /// restore the stall for all of them. No default: an implementor must
    /// answer deliberately, since `true` wrongly restores the 300 s stall and
    /// `false` wrongly denies an operator who was watching. A sink that cannot
    /// tell must answer `false` — an event nobody consumes is a prompt nobody
    /// sees.
    /// Test: `ask_without_an_attached_prompter_denies_immediately`,
    /// `an_ask_on_an_unwatched_session_is_headless_while_a_sibling_is_watched`,
    /// `ask_emits_requested_then_resolved`.
    fn prompter_attached(&self, session_id: &str) -> bool;
}

/// The session-scoped, agent-independent half of a gate's configuration.
///
/// Why: a gate is per AGENT (its map comes from that agent's config), while
/// the session, the answering channel, and the timeout are shared by every
/// agent in one run, so the sub-agent runner mints a gate per delegation from
/// one cloned context.
/// What: `ask` is `None` for a headless run; an `ask` rule then resolves
/// immediately per `mode`. #8100: `ask` being `Some` is NOT the same as a
/// client being attached — the daemon mints broker state for every `task.run`
/// — so `events.prompter_attached` is consulted too, and a run nobody is
/// watching is headless however the broker was wired. #7948: `root` is the
/// run's working root, which
/// [`subjects_for_in_root`] needs to decide an absolute in-root path against a
/// relatively-written rule; `None` restricts matching to the spelling the model
/// wrote.
/// Test: `headless_ask_is_denied`, `ask_emits_requested_then_resolved`,
/// `absolute_in_root_path_meets_a_relative_deny`.
#[derive(Clone)]
pub struct PermissionContext {
    pub mode: PermissionMode,
    pub timeout: Duration,
    pub session_id: String,
    pub ask: Option<Arc<SessionPermissions>>,
    pub events: Option<Arc<dyn PermissionEvents>>,
    pub root: Option<PathBuf>,
}

impl PermissionContext {
    /// A context that can never prompt — the `tcode run-task` shape.
    /// Test: `headless_ask_is_denied`.
    pub fn headless(mode: PermissionMode) -> Self {
        Self {
            mode,
            timeout: Duration::from_secs(DEFAULT_ASK_TIMEOUT_SECS),
            session_id: String::new(),
            ask: None,
            events: None,
            root: None,
        }
    }

    /// Attach the run's working root.
    ///
    /// Why (#7948): without it, a relative path rule misses the absolute
    /// in-root spelling of the same file, which `tools::fs::scoped_path`
    /// accepts.
    /// Test: `absolute_in_root_path_meets_a_relative_deny`.
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Mint a gate for one agent running under this context.
    /// Test: `gate_for_agent_without_a_map_allows_everything`.
    pub fn gate_for(&self, agent: Arc<AgentConfig>, agent_id: impl Into<String>) -> PermissionGate {
        PermissionGate {
            agent,
            agent_id: agent_id.into(),
            ctx: self.clone(),
        }
    }
}

/// One agent's permission gate — the object the agent loop holds.
///
/// What: consulted in every harness mode. #7948: `HarnessMode::Parity` is not
/// exempt — a `deny` the operator wrote holds there too; the tool SCHEMAS a
/// parity run advertises are unchanged by the gate.
/// Test: `deny_never_dispatches`.
pub struct PermissionGate {
    agent: Arc<AgentConfig>,
    agent_id: String,
    ctx: PermissionContext,
}

impl PermissionGate {
    /// Evaluate one tool call against `agent`'s configuration — the pure half.
    ///
    /// Why: an associated function so precedence is testable with a config and
    /// a JSON value alone.
    /// What: the LEGACY `tcode_tools` allowlist runs FIRST — a tool absent from
    /// it is denied before the map is consulted, so a map cannot widen an
    /// allowlist. #7948: a tool in [`HARNESS_REGISTERED_TOOLS`] skips that
    /// branch, because the harness — not the agent's author — put it in the
    /// registry. Then the map decides over every subject of the call; an
    /// unmatched call is `Allow`.
    /// Test: `legacy_allowlist_denial_precedes_the_map`,
    /// `legacy_allowlist_absent_means_every_tool`, `unmatched_tool_is_allowed`,
    /// `deny_rule_produces_deny`, `ask_rule_produces_ask`,
    /// `harness_registered_tool_bypasses_the_legacy_allowlist`,
    /// `stock_pm_agent_may_call_delegate_to_agent`,
    /// `explicit_deny_beats_the_harness_registered_exemption`.
    pub fn evaluate(agent: &AgentConfig, tool: &str, args: &Value) -> Decision {
        Self::evaluate_in_root(agent, tool, args, None)
    }

    /// [`PermissionGate::evaluate`] with the run's working root supplied.
    ///
    /// Why (#7948): a path rule an operator wrote relative must also decide the
    /// absolute in-root spelling of the same file — see
    /// [`subjects_for_in_root`].
    /// What: identical to [`PermissionGate::evaluate`] but for the extra
    /// root-relative subject, which `evaluate_call`'s safest-wins fold can only
    /// use to restrict.
    /// Test: `absolute_in_root_path_meets_a_relative_deny`,
    /// `path_deny_holds_for_the_absolute_in_root_spelling`.
    pub fn evaluate_in_root(
        agent: &AgentConfig,
        tool: &str,
        args: &Value,
        root: Option<&Path>,
    ) -> Decision {
        // #7948: the allowlist speaks for the agent's author, who never sees
        // the harness-registered tools; only a written `deny` refuses those.
        if let Some(allowed) = agent.tools.as_ref().and_then(|t| t.allowed.as_ref())
            && !allowed.iter().any(|a| a == tool)
            && !is_harness_registered(tool)
        {
            return Decision::Deny {
                rule: "tcode_tools allowlist".to_string(),
            };
        }
        let Some(map) = agent.permissions.as_ref() else {
            return Decision::Allow;
        };
        match map.evaluate_call(tool, &subjects_for_in_root(tool, args, root)) {
            None => Decision::Allow,
            Some(m) => match m.decision {
                RuleDecision::Allow => Decision::Allow,
                RuleDecision::Deny => Decision::Deny { rule: m.rule },
                RuleDecision::Ask => Decision::Ask { rule: m.rule },
            },
        }
    }

    /// Decide one tool call, prompting and waiting when the map says `ask`.
    ///
    /// Why: the single call the agent loop makes; the caller must not dispatch
    /// on `Outcome::Denied`.
    /// What: `evaluate` runs; `Allow`/`Deny` return at once. `Ask` checks this
    /// session's remembered grants, then `AllowAsks` — both auto-allow paths
    /// record the approval — then, only with an answering channel AND a client
    /// watching the session (#8100), emits `PermissionRequested`, awaits an
    /// answer for `ctx.timeout`, and emits `PermissionResolved`. Missing
    /// either is `Denied { source: Headless }`, returned at once rather than
    /// after [`DEFAULT_ASK_TIMEOUT_SECS`].
    ///
    /// Test: `headless_ask_is_denied`, `allow_asks_mode_permits_an_ask`,
    /// `ask_without_an_attached_prompter_denies_immediately`,
    /// `an_ask_on_an_unwatched_session_is_headless_while_a_sibling_is_watched`,
    /// `an_ask_with_no_event_sink_is_headless`,
    /// `allow_asks_mode_emits_an_audit_event`,
    /// `timed_out_ask_is_denied`, `client_deny_is_denied`,
    /// `ask_emits_requested_then_resolved`,
    /// `allow_for_session_is_remembered_for_a_later_matching_call`,
    /// `remembered_grant_emits_a_resolution_event`.
    pub async fn resolve(&self, tool: &str, args: &Value) -> Outcome {
        // #7948: the run's root makes a relative path rule decide the absolute
        // in-root spelling too.
        let root = self.ctx.root.as_deref();
        let rule = match Self::evaluate_in_root(&self.agent, tool, args, root) {
            Decision::Allow => return Outcome::Allow,
            Decision::Deny { rule } => {
                return Outcome::Denied {
                    rule,
                    source: DenySource::Policy,
                };
            }
            Decision::Ask { rule } => rule,
        };

        let subjects = subjects_for_in_root(tool, args, root);
        if let Some(state) = &self.ctx.ask
            && state.is_remembered(tool, &subjects)
        {
            // #7948: a grant reused silently is a call an audit cannot see.
            self.record_auto_allow(
                tool,
                &subjects,
                &rule,
                "allow_for_session",
                REMEMBERED_SOURCE,
                "permission: ask satisfied by a remembered session grant",
            );
            return Outcome::Allow;
        }
        if self.ctx.mode == PermissionMode::AllowAsks {
            // #7948: an auto-approved `ask` still leaves an audit trail.
            self.record_auto_allow(
                tool,
                &subjects,
                &rule,
                "allow_once",
                ALLOW_ASKS_SOURCE,
                "permission: ask auto-approved by allow-asks mode",
            );
            return Outcome::Allow;
        }
        // #8100: BOTH halves must hold — a rendezvous to wait on, and someone
        // watching the events who could answer into it. A daemon `task.run`
        // always has the first, which is why a headless run used to wait out
        // the whole ask timeout before denying.
        let Some(state) = self.ctx.ask.clone().filter(|_| self.prompter_attached()) else {
            return Outcome::Denied {
                rule,
                source: DenySource::Headless,
            };
        };
        self.ask(&state, tool, &subjects, rule).await
    }

    /// Whether a client is watching this session and could answer an `ask`.
    ///
    /// Why (#8100): see [`PermissionEvents::prompter_attached`]. No event sink
    /// at all is headless by the same argument — the `permission_requested`
    /// event carries the `request_id` a client answers with, so a request that
    /// is never published can never be answered.
    /// Test: `ask_without_an_attached_prompter_denies_immediately`,
    /// `an_ask_with_no_event_sink_is_headless`.
    fn prompter_attached(&self) -> bool {
        self.ctx
            .events
            .as_ref()
            .is_some_and(|events| events.prompter_attached(&self.ctx.session_id))
    }

    /// Record an `ask` this gate allowed WITHOUT putting it to a client —
    /// `PermissionMode::AllowAsks`, or a grant remembered earlier this session.
    ///
    /// Why (#7948): without a record, an operator auditing a run cannot tell an
    /// auto-allowed `ask` from a call no rule matched. The two auto-allow paths
    /// differ only in the decision/source words, so they share one emitter.
    /// What: logs at `info` with the redacted subject, and emits
    /// `permission_resolved` under a fresh request id that no
    /// `permission_requested` preceded — the marker of an auto-allow.
    /// Test: `allow_asks_mode_emits_an_audit_event`,
    /// `remembered_grant_emits_a_resolution_event`.
    fn record_auto_allow(
        &self,
        tool: &str,
        subjects: &[String],
        rule: &str,
        decision: &str,
        source: &str,
        reason: &'static str,
    ) {
        let request_id = Uuid::new_v4().to_string();
        let agent_name = self.agent.agent.name.as_str();
        tracing::info!(
            session_id = %self.ctx.session_id,
            agent = %agent_name,
            agent_id = %self.agent_id,
            tool = %tool,
            subject = %redact_subject(&subjects.join("\n")),
            rule = %rule,
            request_id = %request_id,
            decision = %decision,
            source = %source,
            "{reason}"
        );
        if let Some(events) = &self.ctx.events {
            events.permission_resolved(
                &self.ctx.session_id,
                &request_id,
                agent_name,
                &self.agent_id,
                decision,
                source,
            );
        }
    }

    /// Emit the request, await the answer, emit the resolution.
    /// Test: `ask_emits_requested_then_resolved`, `timed_out_ask_is_denied`,
    /// `abandoned_request_resolves_to_deny`.
    async fn ask(
        &self,
        state: &Arc<SessionPermissions>,
        tool: &str,
        subjects: &[String],
        rule: String,
    ) -> Outcome {
        let request_id = Uuid::new_v4().to_string();
        let rx = state.register(&request_id);
        let agent_name = self.agent.agent.name.as_str();

        if let Some(events) = &self.ctx.events {
            events.permission_requested(
                &self.ctx.session_id,
                &request_id,
                agent_name,
                &self.agent_id,
                tool,
                &redact_subject(&subjects.join("\n")),
                &rule,
            );
        }

        let denied = |source| Outcome::Denied {
            rule: rule.clone(),
            source,
        };
        let (outcome, decision, source) = match tokio::time::timeout(self.ctx.timeout, rx).await {
            Ok(Ok(PermissionDecision::AllowOnce)) => (Outcome::Allow, "allow_once", "client"),
            Ok(Ok(PermissionDecision::AllowForSession { pattern })) => {
                state.remember(tool, subjects, pattern.as_deref());
                (Outcome::Allow, "allow_for_session", "client")
            }
            Ok(Ok(PermissionDecision::Deny)) => (denied(DenySource::Client), "deny", "client"),
            // #7948: a dropped sender (abandoned or never registered) produced
            // no decision, exactly like a timeout — never an allow.
            Ok(Err(_)) | Err(_) => {
                state.forget(&request_id);
                (denied(DenySource::Timeout), "deny", "timeout")
            }
        };

        if let Some(events) = &self.ctx.events {
            events.permission_resolved(
                &self.ctx.session_id,
                &request_id,
                agent_name,
                &self.agent_id,
                decision,
                source,
            );
        }
        outcome
    }
}
