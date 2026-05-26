//! Artifact model — agents, skills, and hooks.
//!
//! Why: trusty-mpm must be 100% compatible with claude-mpm artifacts so existing
//! agent/skill libraries work unchanged. This module mirrors claude-mpm's
//! `.md` + YAML-frontmatter format and the Claude Code hook event vocabulary.
//! What: Parses agent/skill markdown into structured types and enumerates hook events.
//! Test: `cargo test -p trusty-mpm-core` parses a fixture agent and asserts fields.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::error::{Error, Result};

/// An agent definition loaded from a claude-mpm-compatible `.md` file.
///
/// Frontmatter accepts both MPM-proprietary fields (`agent_id`, `agent_type`,
/// `resource_tier`) and Claude Code native fields (`name`, `model`, `tools`).
/// Unknown keys are preserved in `extra` so artifacts survive round-trips.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentArtifact {
    /// Canonical agent name (Claude Code `name` field).
    pub name: String,
    /// Human-readable description / routing hints.
    #[serde(default)]
    pub description: String,
    /// Preferred model tier, e.g. `claude-opus-4-7`.
    #[serde(default)]
    pub model: Option<String>,
    /// Skills bundled with the agent.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Any frontmatter keys not modeled above (kept for compatibility).
    #[serde(flatten)]
    pub extra: serde_yaml::Mapping,
    /// Markdown body after the frontmatter block.
    #[serde(skip)]
    pub body: String,
}

/// A skill definition loaded from a `SKILL.md` (or bundled `.md`) file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillArtifact {
    /// Skill identifier used in slash-command resolution.
    pub name: String,
    /// Short description shown in skill listings.
    #[serde(default)]
    pub description: String,
    /// Markdown body injected into the prompt when the skill resolves.
    #[serde(skip)]
    pub body: String,
}

/// Claude Code hook events the daemon can intercept out-of-band.
///
/// Names match claude-mpm exactly so settings.json semantics are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    SubagentStop,
    SessionStart,
    UserPromptSubmit,
}

impl AgentArtifact {
    /// Parse an agent artifact from markdown with YAML frontmatter.
    ///
    /// Why: The daemon's artifact store serves agents OOB; it must read the
    /// same files claude-mpm writes without a conversion step.
    /// What: Splits frontmatter from body, deserializes the YAML head.
    /// Test: See `parses_minimal_agent` below.
    pub fn parse(markdown: &str) -> Result<Self> {
        let parsed = gray_matter::Matter::<gray_matter::engine::YAML>::new().parse(markdown);
        let data = parsed
            .data
            .ok_or_else(|| Error::Artifact("agent file has no frontmatter".into()))?;
        let yaml: serde_yaml::Value = data
            .deserialize()
            .map_err(|e| Error::Artifact(format!("frontmatter: {e}")))?;
        let mut agent: AgentArtifact = serde_yaml::from_value(yaml)?;
        agent.body = parsed.content;
        Ok(agent)
    }

    /// Load an agent artifact from a file path.
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Self::parse(&raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_agent() {
        let md = "---\nname: engineer\ndescription: builds things\nmodel: claude-sonnet\n---\n# Engineer\nBody text.";
        let agent = AgentArtifact::parse(md).expect("should parse");
        assert_eq!(agent.name, "engineer");
        assert_eq!(agent.model.as_deref(), Some("claude-sonnet"));
        assert!(agent.body.contains("Body text."));
    }

    #[test]
    fn rejects_missing_frontmatter() {
        let err = AgentArtifact::parse("# No frontmatter here").unwrap_err();
        assert!(matches!(err, Error::Artifact(_)));
    }
}
