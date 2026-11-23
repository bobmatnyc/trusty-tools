//! Caller identity hierarchy for memory scoping (#193).
//!
//! Why: Memory must be scoped according to *who* is asking — CTRL gets
//! everything, the PM sees the whole session, individual agents see only
//! their own writes. Agents must not be able to self-elevate; the harness
//! determines `CallerIdentity` at spawn time and enforces a recall ceiling
//! server-side so agent code can never bypass it.
//! What: `CallerIdentity` is a sum type with three variants — `Ctrl`,
//! `Pm`, `Agent` — each carrying the IDs needed to apply the right scope
//! filter. `RecallCeiling` is the maximum scope a caller may recall (All,
//! Session, Agent). Construction from `TAGENT_CALLER` env vars happens
//! in tools at request time.
//! Test: See `tests` submodule — agent identity caps recall at Agent;
//! auto_tags returns the expected tag set for each variant.

use serde::{Deserialize, Serialize};

/// Env var name conventions for spawn-time identity injection.
///
/// Why: Subprocesses cannot share Rust types with their parent, so we use
/// environment variables to ferry identity over the process boundary.
/// What: Four TAGENT_* variables — `TAGENT_CALLER` selects the variant, the
/// rest carry the IDs each variant needs. The deprecated OPEN_MPM_* names
/// are still honoured via `crate::env_compat::env_var` fallback shims.
/// Test: `caller_identity_from_env_*` round-trips for each variant.
pub const ENV_CALLER: &str = "TAGENT_CALLER";
pub const ENV_SESSION_ID: &str = "TAGENT_SESSION_ID";
pub const ENV_PROJECT_ID: &str = "TAGENT_PROJECT_ID";
pub const ENV_AGENT_ID: &str = "TAGENT_AGENT_ID";

// Deprecated legacy names — used only as fallback keys in env_compat reads.
// These were the OPEN_MPM_* names before the open-mpm → trusty-agents rename (#831).
pub const ENV_CALLER_DEPRECATED: &str = "OPEN_MPM_CALLER";
pub const ENV_SESSION_ID_DEPRECATED: &str = "OPEN_MPM_SESSION_ID";
pub const ENV_PROJECT_ID_DEPRECATED: &str = "OPEN_MPM_PROJECT_ID";
pub const ENV_AGENT_ID_DEPRECATED: &str = "OPEN_MPM_AGENT_ID";

/// Which component is making a memory call.
///
/// Why: Each tier (CTRL > PM > Agent) has different access rights to the
/// memory store. Encoding the tier as a sum type makes the privilege
/// boundary visible in every signature that needs to enforce it.
/// What: `Ctrl` is the user-facing controller; `Pm` runs a session in a
/// project; `Agent` is a single sub-agent inside that session.
/// Test: `auto_tags_for_each_variant`, `max_recall_scope_for_each_variant`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum CallerIdentity {
    /// CTRL — the user, unrestricted.
    Ctrl,
    /// PM — bounded to a single session within a project.
    Pm {
        session_id: String,
        project_id: String,
    },
    /// Agent — bounded to its own writes within a session.
    Agent {
        session_id: String,
        project_id: String,
        agent_id: String,
    },
}

impl CallerIdentity {
    /// Maximum scope this caller is allowed to recall.
    ///
    /// Why: Enforced by the harness — not by agent code. An agent that
    /// passes `scope: "all"` is silently downgraded to `Agent` rather
    /// than erroring, so the model can't infer the boundary.
    /// What: Returns `RecallCeiling::All` for CTRL, `Session` for PM,
    /// `Agent` for sub-agents.
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
    /// Why: Auto-tagging at write time means future scope filters can
    /// match on tags without trusting agent-supplied metadata.
    /// What: `Ctrl` -> `scope/user`; `Pm` -> session+project; `Agent` ->
    /// session+project+agent.
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

    /// Session id when present on this identity (PM/Agent).
    ///
    /// Why: Recall filters need the session id to scope by it; CTRL has
    /// no session.
    /// What: `Some(session_id)` for PM/Agent, `None` for CTRL.
    /// Test: `session_id_returns_for_pm_and_agent`.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Ctrl => None,
            Self::Pm { session_id, .. } | Self::Agent { session_id, .. } => Some(session_id),
        }
    }

    /// Agent id when present on this identity (Agent only).
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent { agent_id, .. } => Some(agent_id),
            _ => None,
        }
    }

    /// Construct a `CallerIdentity` by reading the well-known env vars.
    ///
    /// Why: Subprocesses inherit identity over the env-var bridge defined
    /// by the harness; reading at request time means the same tool
    /// instance can serve multiple processes (the binary is re-invoked
    /// per agent run).
    /// What: Reads `TAGENT_CALLER`. For `pm`/`agent` requires
    /// `TAGENT_SESSION_ID` + `TAGENT_PROJECT_ID` (and `TAGENT_AGENT_ID`
    /// for agents). Returns `None` if the caller var is missing or
    /// inconsistent — callers should treat that as "no scope ceiling
    /// information" and fall back to legacy behavior.
    /// Test: `caller_identity_from_env_round_trips`.
    pub fn from_env() -> Option<Self> {
        use crate::env_compat::env_var;
        let caller = env_var(ENV_CALLER, ENV_CALLER_DEPRECATED)
            .ok()?
            .to_lowercase();
        match caller.as_str() {
            "ctrl" => Some(Self::Ctrl),
            "pm" => {
                let session_id = env_var(ENV_SESSION_ID, ENV_SESSION_ID_DEPRECATED).ok()?;
                let project_id = env_var(ENV_PROJECT_ID, ENV_PROJECT_ID_DEPRECATED).ok()?;
                Some(Self::Pm {
                    session_id,
                    project_id,
                })
            }
            "agent" => {
                let session_id = env_var(ENV_SESSION_ID, ENV_SESSION_ID_DEPRECATED).ok()?;
                let project_id = env_var(ENV_PROJECT_ID, ENV_PROJECT_ID_DEPRECATED).ok()?;
                let agent_id = env_var(ENV_AGENT_ID, ENV_AGENT_ID_DEPRECATED).ok()?;
                Some(Self::Agent {
                    session_id,
                    project_id,
                    agent_id,
                })
            }
            _ => None,
        }
    }
}

/// Maximum scope a caller is allowed to recall.
///
/// Why: A small, total ordering of access tiers makes the ceiling
/// trivially comparable in tool code without needing to remember the
/// privilege rules at every callsite.
/// What: `All` > `Session` > `Agent`.
/// Test: ceiling-comparison tests live alongside the tools that consume them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallCeiling {
    All,
    Session,
    Agent,
}

pub mod user_profile;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_tags_for_each_variant() {
        assert_eq!(CallerIdentity::Ctrl.auto_tags(), vec!["scope/user"]);

        let pm = CallerIdentity::Pm {
            session_id: "s1".into(),
            project_id: "proj".into(),
        };
        assert_eq!(pm.auto_tags(), vec!["session/s1", "project/proj"]);

        let agent = CallerIdentity::Agent {
            session_id: "s1".into(),
            project_id: "proj".into(),
            agent_id: "code-agent".into(),
        };
        assert_eq!(
            agent.auto_tags(),
            vec!["session/s1", "project/proj", "agent/code-agent"]
        );
    }

    #[test]
    fn max_recall_scope_for_each_variant() {
        assert_eq!(CallerIdentity::Ctrl.max_recall_scope(), RecallCeiling::All);
        assert_eq!(
            CallerIdentity::Pm {
                session_id: "s".into(),
                project_id: "p".into(),
            }
            .max_recall_scope(),
            RecallCeiling::Session
        );
        assert_eq!(
            CallerIdentity::Agent {
                session_id: "s".into(),
                project_id: "p".into(),
                agent_id: "a".into(),
            }
            .max_recall_scope(),
            RecallCeiling::Agent
        );
    }

    #[test]
    fn session_id_returns_for_pm_and_agent() {
        assert_eq!(CallerIdentity::Ctrl.session_id(), None);
        assert_eq!(
            CallerIdentity::Pm {
                session_id: "abc".into(),
                project_id: "p".into()
            }
            .session_id(),
            Some("abc")
        );
        assert_eq!(
            CallerIdentity::Agent {
                session_id: "abc".into(),
                project_id: "p".into(),
                agent_id: "a".into()
            }
            .session_id(),
            Some("abc")
        );
    }

    #[test]
    fn agent_id_returns_only_for_agent_variant() {
        assert_eq!(CallerIdentity::Ctrl.agent_id(), None);
        assert_eq!(
            CallerIdentity::Pm {
                session_id: "s".into(),
                project_id: "p".into()
            }
            .agent_id(),
            None
        );
        assert_eq!(
            CallerIdentity::Agent {
                session_id: "s".into(),
                project_id: "p".into(),
                agent_id: "code".into()
            }
            .agent_id(),
            Some("code")
        );
    }
}
