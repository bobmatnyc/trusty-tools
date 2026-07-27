//! Agent delegation model.
//!
//! Why: the dashboard must render a per-session delegation tree (which subagent
//! delegated to which, on what model tier, with what circuit-breaker state).
//! A shared type keeps the daemon's tracker, the TUI tree widget, and the MCP
//! `agent_delegate` tool aligned on one representation.
//! What: `ModelTier` (haiku/sonnet/opus), `DelegationId`, `DelegationStatus`,
//! and `Delegation` — a node in the per-session delegation tree.
//! Test: `cargo test -p trusty-mpm-core` round-trips a `Delegation` through JSON
//! and checks tier parsing.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::core::circuit::CircuitState;
use crate::core::session::SessionId;

/// Coarse model tier an agent delegation runs on.
///
/// Why: claude-mpm enforces a tier policy (PM/planner on opus, specialists on
/// sonnet, cheap tasks on haiku). The dashboard colour-codes by tier and the
/// circuit breaker counts opus delegations more strictly.
/// What: three variants mapping to Claude's model families.
/// Test: `tier_parses_from_model_id` covers the `from_model_id` mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Cheapest, fastest tier — Haiku family.
    Haiku,
    /// Mid tier — Sonnet family (default for specialists).
    Sonnet,
    /// Top tier — Opus family (PM, planner, architecture work).
    Opus,
}

impl ModelTier {
    /// Infer a tier from a Claude model identifier.
    ///
    /// Why: hook payloads and agent frontmatter carry full model ids
    /// (`claude-opus-4-7`); the dashboard wants the coarse tier.
    /// What: substring match on the model family; unknown ids fall back to
    /// `Sonnet` (the safe default specialist tier).
    /// Test: `tier_parses_from_model_id`.
    pub fn from_model_id(model: &str) -> Self {
        let m = model.to_ascii_lowercase();
        if m.contains("opus") {
            ModelTier::Opus
        } else if m.contains("haiku") {
            ModelTier::Haiku
        } else {
            ModelTier::Sonnet
        }
    }
}

/// Tool names Claude Code uses to dispatch a native subagent.
///
/// Why (#2864): delegation tracking hangs off the `PreToolUse`/`PostToolUse`
/// hook, which names the tool being invoked. The subagent-dispatch tool was
/// called `Task` in earlier Claude Code releases and is called `Agent` in
/// current ones (empirically confirmed against Claude Code 2.1.220, whose
/// `PreToolUse` payload carries `"tool_name": "Agent"`). Matching only one of
/// the two names silently disables tracking on half the installed base, so both
/// are recognised and neither may be removed without re-probing the live hook
/// payload.
/// What: the exact `tool_name` values that mean "a subagent is being spawned".
/// Test: `subagent_dispatch_tool_matches_both_names`.
pub const SUBAGENT_DISPATCH_TOOLS: &[&str] = &["Task", "Agent"];

/// Is `tool_name` the native subagent-dispatch tool?
///
/// Why: the single predicate both the `tm hook` payload builder and the daemon's
/// delegation tracker consult, so the two can never disagree about what counts
/// as a delegation. It is also the hot-path early-out — every non-dispatch tool
/// call in every managed session hits this and nothing else.
/// What: case-sensitive membership test against [`SUBAGENT_DISPATCH_TOOLS`]
/// (Claude Code emits the tool name verbatim, so an exact match is correct and
/// avoids matching an unrelated user tool named `agent`).
/// Test: `subagent_dispatch_tool_matches_both_names`.
pub fn is_subagent_dispatch_tool(tool_name: &str) -> bool {
    SUBAGENT_DISPATCH_TOOLS.contains(&tool_name)
}

/// Stable identifier for one agent delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DelegationId(pub Uuid);

impl DelegationId {
    /// Generate a fresh random delegation id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DelegationId {
    fn default() -> Self {
        Self::new()
    }
}

/// Lifecycle state of a single agent delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatus {
    /// Delegation has been requested but the subagent has not started.
    Queued,
    /// Subagent is actively running.
    Running,
    /// Subagent finished successfully.
    Completed,
    /// Subagent failed.
    Failed,
    /// Delegation was cancelled before completion.
    Cancelled,
}

/// How a delegation record came to exist.
///
/// Why (#2864): tracking used to be opt-in — a delegation existed only if the PM
/// voluntarily called the `agent_delegate` MCP tool, so a PM that dispatched
/// work with the native subagent tool alone left no trace at all. The daemon now
/// also *observes* dispatches from the `PreToolUse` hook, which is non-opt-in.
/// Recording which of the two produced a record lets the dedup pass merge the
/// declaration and the observation of the *same* delegation into one node
/// instead of double-counting it, and lets consumers tell a PM-asserted
/// intention from an observed fact.
/// What: `McpDeclared` — created by the `agent_delegate` MCP tool (the PM said
/// it would delegate). `HookObserved` — created from a `PreToolUse` hook naming
/// a [`SUBAGENT_DISPATCH_TOOLS`] tool (the runtime actually dispatched).
/// Test: `delegation_defaults_to_mcp_declared_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationSource {
    /// Declared through the `agent_delegate` MCP tool.
    #[default]
    McpDeclared,
    /// Observed from a `PreToolUse` subagent-dispatch hook.
    HookObserved,
}

/// A node in a session's agent-delegation tree.
///
/// Why: the dashboard renders delegations as a tree (PM → research → ...).
/// `parent` lets the TUI reconstruct that tree from a flat list.
/// What: pairs the delegating relationship with the target agent, its model
/// tier, current status, and the circuit-breaker state for that agent.
/// Test: `delegation_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegation {
    /// Unique id for this delegation.
    pub id: DelegationId,
    /// Session the delegation belongs to.
    pub session: SessionId,
    /// Parent delegation, or `None` for a top-level (PM) delegation.
    #[serde(default)]
    pub parent: Option<DelegationId>,
    /// Target agent name (matches an `AgentArtifact::name`).
    pub agent: String,
    /// Model tier the delegation runs on.
    pub tier: ModelTier,
    /// Current lifecycle status.
    pub status: DelegationStatus,
    /// Circuit-breaker state for this agent at the time of the snapshot.
    pub circuit: CircuitState,
    /// Short description of the delegated task.
    #[serde(default)]
    pub task: String,
    /// When the delegation was created (UTC).
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// How this record was created (#2864).
    #[serde(default)]
    pub source: DelegationSource,
    /// Claude Code's `tool_use_id` for the dispatching tool call, when known.
    ///
    /// Why: this is the key that joins `PreToolUse` to its matching
    /// `PostToolUse` — both carry the identical `toolu_…` id. It is what makes
    /// correlation exact under concurrency instead of a "most recent" guess.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Claude Code's subagent `agent_id`, learned from the `PostToolUse`
    /// response and matched against `SubagentStop.agent_id` (#2864).
    ///
    /// Why: `SubagentStop` does not carry `tool_use_id`; it carries `agent_id`.
    /// `PostToolUse.tool_response.agentId` is the only place the two identifier
    /// spaces meet, so it must be persisted here for the stop hook to resolve.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Transcript the dispatching turn was writing to, when known.
    #[serde(default)]
    pub transcript_path: Option<std::path::PathBuf>,
    /// Working directory the dispatch was issued from, when known.
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    /// When the subagent actually started running (UTC), when known.
    #[serde(default)]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the delegation reached a terminal status (UTC), when known.
    #[serde(default)]
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Delegation {
    /// Build a freshly-queued delegation stamped with the current time.
    pub fn new(
        session: SessionId,
        parent: Option<DelegationId>,
        agent: impl Into<String>,
        tier: ModelTier,
        task: impl Into<String>,
    ) -> Self {
        Self {
            id: DelegationId::new(),
            session,
            parent,
            agent: agent.into(),
            tier,
            status: DelegationStatus::Queued,
            circuit: CircuitState::Closed,
            task: task.into(),
            created_at: chrono::Utc::now(),
            source: DelegationSource::McpDeclared,
            tool_use_id: None,
            agent_id: None,
            transcript_path: None,
            cwd: None,
            started_at: None,
            ended_at: None,
        }
    }

    /// Build a `Running` delegation observed from a subagent-dispatch hook.
    ///
    /// Why (#2864): a `PreToolUse` naming a [`SUBAGENT_DISPATCH_TOOLS`] tool is
    /// proof the runtime is spawning a subagent *now* — not queueing it — so
    /// unlike [`Self::new`] this starts in [`DelegationStatus::Running`] with
    /// `started_at` stamped. Carrying `tool_use_id` from birth is what lets the
    /// matching `PostToolUse` find exactly this record among concurrent
    /// siblings.
    /// What: same shape as [`Self::new`] but `source = HookObserved`, status
    /// `Running`, and `started_at = created_at`.
    /// Test: `observed_delegation_starts_running`.
    pub fn observed(
        session: SessionId,
        agent: impl Into<String>,
        task: impl Into<String>,
        tool_use_id: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: DelegationId::new(),
            session,
            parent: None,
            agent: agent.into(),
            tier: ModelTier::Sonnet,
            status: DelegationStatus::Running,
            circuit: CircuitState::Closed,
            task: task.into(),
            created_at: now,
            source: DelegationSource::HookObserved,
            tool_use_id,
            agent_id: None,
            transcript_path: None,
            cwd: None,
            started_at: Some(now),
            ended_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_parses_from_model_id() {
        assert_eq!(ModelTier::from_model_id("claude-opus-4-7"), ModelTier::Opus);
        assert_eq!(
            ModelTier::from_model_id("claude-haiku-3-5"),
            ModelTier::Haiku
        );
        assert_eq!(
            ModelTier::from_model_id("claude-sonnet-4"),
            ModelTier::Sonnet
        );
        // Unknown ids fall back to Sonnet.
        assert_eq!(ModelTier::from_model_id("mystery-model"), ModelTier::Sonnet);
    }

    #[test]
    fn delegation_round_trips() {
        let d = Delegation::new(
            SessionId::new(),
            None,
            "research",
            ModelTier::Sonnet,
            "find the bug",
        );
        let json = serde_json::to_string(&d).unwrap();
        let back: Delegation = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent, "research");
        assert_eq!(back.status, DelegationStatus::Queued);
        assert_eq!(back.tier, ModelTier::Sonnet);
        assert!(back.parent.is_none());
    }

    #[test]
    fn subagent_dispatch_tool_matches_both_names() {
        // Both the legacy (`Task`) and current (`Agent`, Claude Code 2.1.220)
        // names must match — recognising only one silently disables tracking.
        assert!(is_subagent_dispatch_tool("Task"));
        assert!(is_subagent_dispatch_tool("Agent"));
        // Exact match only: ordinary tools and lookalikes must not match.
        assert!(!is_subagent_dispatch_tool("Bash"));
        assert!(!is_subagent_dispatch_tool("agent"));
        assert!(!is_subagent_dispatch_tool("TaskRunner"));
        assert!(!is_subagent_dispatch_tool(""));
    }

    #[test]
    fn delegation_defaults_to_mcp_declared_source() {
        // `Delegation::new` is the `agent_delegate` path and must keep its
        // pre-#2864 semantics: Queued, MCP-declared, no hook correlation keys.
        let d = Delegation::new(SessionId::new(), None, "qa", ModelTier::Sonnet, "test");
        assert_eq!(d.source, DelegationSource::McpDeclared);
        assert_eq!(d.status, DelegationStatus::Queued);
        assert!(d.tool_use_id.is_none());
        assert!(d.started_at.is_none());
        assert!(d.ended_at.is_none());
    }

    #[test]
    fn observed_delegation_starts_running() {
        let d = Delegation::observed(
            SessionId::new(),
            "engineer",
            "implement",
            Some("toolu_01ABC".into()),
        );
        assert_eq!(d.source, DelegationSource::HookObserved);
        assert_eq!(d.status, DelegationStatus::Running);
        assert_eq!(d.tool_use_id.as_deref(), Some("toolu_01ABC"));
        assert_eq!(d.started_at, Some(d.created_at));
        assert!(d.ended_at.is_none());
    }

    #[test]
    fn legacy_delegation_json_without_new_fields_round_trips() {
        // Records persisted before #2864 carry none of the new fields; they must
        // still deserialize (every new field is `#[serde(default)]`) and default
        // to the MCP-declared source they in fact came from.
        let legacy = serde_json::json!({
            "id": Uuid::new_v4(),
            "session": SessionId::new(),
            "agent": "research",
            "tier": "sonnet",
            "status": "queued",
            "circuit": CircuitState::Closed,
            "task": "investigate",
            "created_at": chrono::Utc::now(),
        });
        let back: Delegation =
            serde_json::from_value(legacy).expect("legacy delegation JSON must deserialize");
        assert_eq!(back.source, DelegationSource::McpDeclared);
        assert!(back.tool_use_id.is_none());
        assert!(back.agent_id.is_none());
        assert!(back.cwd.is_none());
    }

    #[test]
    fn delegation_tree_parent_links() {
        let session = SessionId::new();
        let root = Delegation::new(session, None, "pm", ModelTier::Opus, "orchestrate");
        let child = Delegation::new(
            session,
            Some(root.id),
            "engineer",
            ModelTier::Sonnet,
            "implement",
        );
        assert_eq!(child.parent, Some(root.id));
    }
}
