//! Caller identity hierarchy for memory scoping in tcode.
//!
//! Why: Memory must be scoped according to who is calling — the operator
//! (full access), the PM (bounded to a session), or a sub-agent (bounded to its
//! own writes). Agents must not be able to self-elevate; the harness determines
//! `CallerIdentity` at spawn time and enforces a recall ceiling so agent code
//! can never bypass it.
//! What: `CallerIdentity` is a sum type with three variants — `Ctrl`, `Pm`,
//! `Agent` — each carrying the IDs needed to apply the right scope filter.
//! `RecallCeiling` is the maximum scope a caller may recall.
//! Test: See unit tests — agent identity caps recall at Agent; `auto_tags`
//! returns the expected tag set for each variant.

use serde::{Deserialize, Serialize};

/// Env var name for spawn-time identity injection.
///
/// Why: Subprocesses cannot share Rust types with their parent; env vars ferry
/// identity over the process boundary.
/// What: `TCODE_CALLER` selects the variant (`ctrl`, `pm`, `agent`).
/// Test: `caller_identity_from_env_*`.
pub const ENV_CALLER: &str = "TCODE_CALLER";
/// Session ID env var.
pub const ENV_SESSION_ID: &str = "TCODE_SESSION_ID";
/// Project ID env var.
pub const ENV_PROJECT_ID: &str = "TCODE_PROJECT_ID";
/// Agent ID env var.
pub const ENV_AGENT_ID: &str = "TCODE_AGENT_ID";

/// Which component is making a memory/context call.
///
/// Why: Each tier has different access rights to the memory store. Encoding
/// the tier as a sum type makes the privilege boundary visible in every
/// signature that needs to enforce it.
/// What: `Ctrl` is the operator (unrestricted); `Pm` runs one session in a
/// project; `Agent` is a single sub-agent inside that session.
/// Test: `auto_tags_for_each_variant`, `max_recall_scope_for_each_variant`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum CallerIdentity {
    /// Operator / controller — unrestricted.
    Ctrl,
    /// PM — bounded to a single session within a project.
    Pm {
        session_id: String,
        project_id: String,
    },
    /// Sub-agent — bounded to its own writes within a session.
    Agent {
        session_id: String,
        project_id: String,
        agent_id: String,
    },
}

impl CallerIdentity {
    /// Maximum scope this caller is allowed to recall.
    ///
    /// Why: Enforced by the harness — not by agent code. An agent that passes
    /// `scope: "all"` is silently downgraded to `Agent`.
    /// What: `Ctrl` → `All`, `Pm` → `Session`, `Agent` → `Agent`.
    /// Test: `max_recall_scope_for_each_variant`.
    pub fn max_recall_scope(&self) -> RecallCeiling {
        match self {
            Self::Ctrl => RecallCeiling::All,
            Self::Pm { .. } => RecallCeiling::Session,
            Self::Agent { .. } => RecallCeiling::Agent,
        }
    }

    /// Tags to automatically apply when this caller stores a memory.
    ///
    /// Why: Auto-tagging at write time means future scope filters can match on
    /// tags without trusting agent-supplied metadata.
    /// What: `Ctrl` → `["scope/user"]`; `Pm` → session+project tags; `Agent`
    /// → session+project+agent tags.
    /// Test: `auto_tags_for_each_variant`.
    pub fn auto_tags(&self) -> Vec<String> {
        match self {
            Self::Ctrl => vec!["scope/user".into()],
            Self::Pm {
                session_id,
                project_id,
            } => vec![
                format!("session/{session_id}"),
                format!("project/{project_id}"),
            ],
            Self::Agent {
                session_id,
                project_id,
                agent_id,
            } => vec![
                format!("session/{session_id}"),
                format!("project/{project_id}"),
                format!("agent/{agent_id}"),
            ],
        }
    }

    /// Try to construct a `CallerIdentity` from environment variables.
    ///
    /// Why: Subprocesses receive identity as env vars; this constructor
    /// centralises the parse logic.
    /// What: Reads `TCODE_CALLER` and the corresponding ID vars.
    /// Returns `None` if the env var is absent or unrecognised.
    /// Test: `caller_identity_from_env_*`.
    pub fn from_env() -> Option<Self> {
        let kind = std::env::var(ENV_CALLER).ok()?;
        match kind.as_str() {
            "ctrl" => Some(Self::Ctrl),
            "pm" => {
                let session_id = std::env::var(ENV_SESSION_ID).ok()?;
                let project_id = std::env::var(ENV_PROJECT_ID).ok()?;
                Some(Self::Pm {
                    session_id,
                    project_id,
                })
            }
            "agent" => {
                let session_id = std::env::var(ENV_SESSION_ID).ok()?;
                let project_id = std::env::var(ENV_PROJECT_ID).ok()?;
                let agent_id = std::env::var(ENV_AGENT_ID).ok()?;
                Some(Self::Agent {
                    session_id,
                    project_id,
                    agent_id,
                })
            }
            _ => None,
        }
    }

    /// Return the session ID if this identity has one.
    ///
    /// Why: Memory tools that scope by session need to extract the ID
    /// without pattern-matching on the full identity.
    /// What: `Some(session_id)` for `Pm` and `Agent`, `None` for `Ctrl`.
    /// Test: `session_id_for_each_variant`.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Ctrl => None,
            Self::Pm { session_id, .. } => Some(session_id.as_str()),
            Self::Agent { session_id, .. } => Some(session_id.as_str()),
        }
    }
}

/// Maximum recall scope for a `CallerIdentity`.
///
/// Why: Provides a named ceiling that the memory tool enforces server-side so
/// agents can't request more than their tier allows.
/// What: `All` > `Session` > `Agent` in scope breadth.
/// Test: Covered by `max_recall_scope_for_each_variant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallCeiling {
    /// May recall all memories.
    All,
    /// May recall memories scoped to the current session.
    Session,
    /// May recall only its own memories.
    Agent,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_recall_scope_for_each_variant() {
        assert_eq!(CallerIdentity::Ctrl.max_recall_scope(), RecallCeiling::All);
        assert_eq!(
            CallerIdentity::Pm {
                session_id: "s1".into(),
                project_id: "p1".into()
            }
            .max_recall_scope(),
            RecallCeiling::Session
        );
        assert_eq!(
            CallerIdentity::Agent {
                session_id: "s1".into(),
                project_id: "p1".into(),
                agent_id: "a1".into()
            }
            .max_recall_scope(),
            RecallCeiling::Agent
        );
    }

    #[test]
    fn auto_tags_for_each_variant() {
        let ctrl_tags = CallerIdentity::Ctrl.auto_tags();
        assert_eq!(ctrl_tags, vec!["scope/user"]);

        let pm_tags = CallerIdentity::Pm {
            session_id: "s42".into(),
            project_id: "p7".into(),
        }
        .auto_tags();
        assert!(pm_tags.contains(&"session/s42".to_string()));
        assert!(pm_tags.contains(&"project/p7".to_string()));
        assert_eq!(pm_tags.len(), 2);

        let agent_tags = CallerIdentity::Agent {
            session_id: "s42".into(),
            project_id: "p7".into(),
            agent_id: "a99".into(),
        }
        .auto_tags();
        assert!(agent_tags.contains(&"agent/a99".to_string()));
        assert_eq!(agent_tags.len(), 3);
    }

    #[test]
    fn session_id_for_each_variant() {
        assert_eq!(CallerIdentity::Ctrl.session_id(), None);
        assert_eq!(
            CallerIdentity::Pm {
                session_id: "sess".into(),
                project_id: "proj".into()
            }
            .session_id(),
            Some("sess")
        );
        assert_eq!(
            CallerIdentity::Agent {
                session_id: "sess".into(),
                project_id: "proj".into(),
                agent_id: "ag".into()
            }
            .session_id(),
            Some("sess")
        );
    }

    #[test]
    fn caller_identity_roundtrips_json() {
        let agent = CallerIdentity::Agent {
            session_id: "s1".into(),
            project_id: "p1".into(),
            agent_id: "engineer".into(),
        };
        let json = serde_json::to_string(&agent).expect("serialize");
        let back: CallerIdentity = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(agent, back);
    }
}
