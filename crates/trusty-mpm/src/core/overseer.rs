//! Session overseer trait and decision model.
//!
//! Why: an optional oversight layer must be able to inspect every Claude Code
//! hook event and either let it through, block it, inject a reply, or escalate
//! to a human — without the daemon knowing *how* that decision is made. A trait
//! (strategy pattern) keeps the rule-based and future LLM-backed overseers
//! interchangeable behind one `Box<dyn Overseer>`.
//! What: [`OverseerDecision`] (the verdict), [`OverseerContext`] (the event the
//! overseer evaluates), and the [`Overseer`] trait the daemon dispatches to.
//! Test: `cargo test -p trusty-mpm-core overseer` round-trips the decision enum
//! and exercises the `DeterministicOverseer` implementation in its own module.

use serde::{Deserialize, Serialize};

use crate::core::session::SessionId;

/// Verdict returned by the [`Overseer`] for one hook event.
///
/// Why: the daemon needs a small, explicit set of actions it can take after
/// consulting the overseer; an enum makes every outcome handled at the call
/// site.
/// What: [`Allow`](OverseerDecision::Allow) lets the event proceed,
/// [`Block`](OverseerDecision::Block) halts it with a reason,
/// [`Respond`](OverseerDecision::Respond) injects text into the session, and
/// [`FlagForHuman`](OverseerDecision::FlagForHuman) escalates for review.
/// Test: `decision_json_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision", content = "detail")]
pub enum OverseerDecision {
    /// Let the event proceed unmodified.
    Allow,
    /// Halt the event; `reason` explains why for logs and the operator.
    Block {
        /// Human-readable explanation of the block.
        reason: String,
    },
    /// Inject `text` into the session as a reply to a pending question.
    Respond {
        /// Text to send into the session.
        text: String,
    },
    /// Escalate to a human; `summary` describes what needs attention.
    FlagForHuman {
        /// Short description of why human attention is needed.
        summary: String,
    },
}

impl OverseerDecision {
    /// Short lowercase tag for this decision, used by the audit logger.
    ///
    /// Why: the audit log records one stable token per decision so log
    /// consumers can filter without parsing the full enum.
    /// What: returns `"allow" | "block" | "respond" | "flag"`.
    /// Test: `decision_tag_is_stable`.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block { .. } => "block",
            Self::Respond { .. } => "respond",
            Self::FlagForHuman { .. } => "flag",
        }
    }

    /// Human-readable reason/summary for this decision, empty for `Allow`.
    ///
    /// Why: the audit log carries a `reason` column for every event; pulling it
    /// off the decision keeps the logger generic.
    /// What: returns the block reason / respond text / flag summary, or `""`.
    /// Test: `decision_reason_extracts_text`.
    pub fn reason(&self) -> &str {
        match self {
            Self::Allow => "",
            Self::Block { reason } => reason,
            Self::Respond { text } => text,
            Self::FlagForHuman { summary } => summary,
        }
    }
}

/// Context passed to every [`Overseer`] call.
///
/// Why: the overseer's decision depends on which session triggered the event
/// and what tool (and input) is involved; bundling these keeps the trait
/// methods to a single argument.
/// What: the session id, its friendly tmux name, and the optional tool name /
/// serialized tool input for the event under evaluation.
/// Test: exercised by the `DeterministicOverseer` tests.
#[derive(Debug, Clone)]
pub struct OverseerContext {
    /// Session the event originated from.
    pub session_id: SessionId,
    /// Friendly tmux session name (`tmpm-<adjective>-<noun>`).
    pub tmux_name: String,
    /// Tool name for `PreToolUse` / `PostToolUse` events, if any.
    pub tool_name: Option<String>,
    /// Serialized tool input for the event, if any.
    pub tool_input: Option<String>,
}

impl OverseerContext {
    /// Build a context for a tool-use event.
    ///
    /// Why: most call sites have all four fields; a constructor keeps them
    /// from drifting in field order.
    /// What: stores every field as given.
    /// Test: exercised by the `DeterministicOverseer` tests.
    pub fn new(
        session_id: SessionId,
        tmux_name: impl Into<String>,
        tool_name: Option<String>,
        tool_input: Option<String>,
    ) -> Self {
        Self {
            session_id,
            tmux_name: tmux_name.into(),
            tool_name,
            tool_input,
        }
    }
}

/// The session overseer — a pluggable oversight strategy.
///
/// Why: the daemon consults an overseer on hook events but must stay agnostic
/// of whether the policy is rule-based ([`DeterministicOverseer`]) or
/// LLM-backed (a future `LlmOverseer`); the trait is the seam.
/// What: three evaluation hooks (`pre_tool_use`, `post_tool_use`,
/// `session_question`) plus [`is_enabled`](Overseer::is_enabled) so the daemon
/// can cheaply skip oversight entirely.
/// Test: `DeterministicOverseer` carries the behavioural tests for this trait.
//
// `Debug` is a supertrait so that types holding an `Arc<dyn Overseer>` (e.g.
// the daemon's shared state) can still `#[derive(Debug)]`.
pub trait Overseer: std::fmt::Debug + Send + Sync {
    /// Evaluate a tool invocation *before* it runs.
    fn pre_tool_use(&self, ctx: &OverseerContext) -> OverseerDecision;

    /// Evaluate a tool's output *after* it runs.
    fn post_tool_use(&self, ctx: &OverseerContext, output: &str) -> OverseerDecision;

    /// Evaluate a question the session is asking the operator.
    fn session_question(&self, ctx: &OverseerContext, question: &str) -> OverseerDecision;

    /// Whether oversight is active; when `false` the daemon skips all calls.
    fn is_enabled(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_json_roundtrip() {
        for decision in [
            OverseerDecision::Allow,
            OverseerDecision::Block {
                reason: "blocked".into(),
            },
            OverseerDecision::Respond { text: "yes".into() },
            OverseerDecision::FlagForHuman {
                summary: "needs review".into(),
            },
        ] {
            let json = serde_json::to_string(&decision).unwrap();
            let back: OverseerDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(back, decision);
        }
    }

    #[test]
    fn decision_tag_is_stable() {
        assert_eq!(OverseerDecision::Allow.tag(), "allow");
        assert_eq!(
            OverseerDecision::Block { reason: "x".into() }.tag(),
            "block"
        );
        assert_eq!(
            OverseerDecision::Respond { text: "x".into() }.tag(),
            "respond"
        );
        assert_eq!(
            OverseerDecision::FlagForHuman {
                summary: "x".into()
            }
            .tag(),
            "flag"
        );
    }

    #[test]
    fn decision_reason_extracts_text() {
        assert_eq!(OverseerDecision::Allow.reason(), "");
        assert_eq!(
            OverseerDecision::Block {
                reason: "danger".into()
            }
            .reason(),
            "danger"
        );
        assert_eq!(
            OverseerDecision::Respond {
                text: "proceed".into()
            }
            .reason(),
            "proceed"
        );
    }
}
