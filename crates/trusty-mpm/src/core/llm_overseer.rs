//! LLM-backed session overseer — placeholder for future work.
//!
//! Why: the overseer is a strategy (`Box<dyn Overseer>` / `Arc<dyn Overseer>`);
//! a future implementation will consult a small model (e.g. Claude Haiku) to
//! make nuanced allow/block/respond decisions. Reserving the type now keeps the
//! seam visible and documents the intended `[llm]` config section.
//! What: [`LlmOverseer`] is a stub that is never enabled; every method defers
//! to safe defaults (`Allow` / `FlagForHuman`). It is *not* wired into the
//! daemon — the daemon uses [`crate::core::deterministic_overseer::DeterministicOverseer`].
//! Test: `cargo test -p trusty-mpm-core llm_overseer` confirms the stub is
//! inert (`is_enabled()` is always `false`).

use crate::core::overseer::{Overseer, OverseerContext, OverseerDecision};

/// Placeholder LLM-backed [`Overseer`]. Not yet implemented.
///
/// Why: marks the intended extension point for model-driven oversight without
/// committing to an LLM client dependency today.
/// What: a zero-field stub whose methods are inert; construct via
/// [`LlmOverseer::placeholder`].
/// Test: `placeholder_is_disabled`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LlmOverseer;

impl LlmOverseer {
    /// Construct the inert placeholder overseer.
    ///
    /// Why: gives callers an explicit, greppable construction point for the
    /// future LLM overseer.
    /// What: returns `LlmOverseer` (a unit struct).
    /// Test: `placeholder_is_disabled`.
    pub fn placeholder() -> Self {
        Self
    }
}

impl Overseer for LlmOverseer {
    fn pre_tool_use(&self, _ctx: &OverseerContext) -> OverseerDecision {
        // Inert: the LLM overseer is not implemented; never block.
        OverseerDecision::Allow
    }

    fn post_tool_use(&self, _ctx: &OverseerContext, _output: &str) -> OverseerDecision {
        OverseerDecision::Allow
    }

    fn session_question(&self, _ctx: &OverseerContext, question: &str) -> OverseerDecision {
        // Inert: escalate every question until the LLM overseer is built.
        OverseerDecision::FlagForHuman {
            summary: format!("LLM overseer not implemented; question needs review: {question}"),
        }
    }

    fn is_enabled(&self) -> bool {
        // The placeholder is never active.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::SessionId;

    #[test]
    fn placeholder_is_disabled() {
        let overseer = LlmOverseer::placeholder();
        assert!(!overseer.is_enabled());
        let ctx = OverseerContext::new(SessionId::new(), "tmpm-stub", None, None);
        assert_eq!(overseer.pre_tool_use(&ctx), OverseerDecision::Allow);
        assert!(matches!(
            overseer.session_question(&ctx, "anything?"),
            OverseerDecision::FlagForHuman { .. }
        ));
    }
}
