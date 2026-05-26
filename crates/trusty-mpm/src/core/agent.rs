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
