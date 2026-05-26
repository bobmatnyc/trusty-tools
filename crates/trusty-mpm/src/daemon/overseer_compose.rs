//! Composite overseer: deterministic rules first, LLM for uncertain cases.
//!
//! Why: the deterministic overseer is fast and dependency-free but can only
//! match substrings; the LLM overseer is nuanced but costs a network round
//! trip. Composing them gives the best of both — the deterministic layer
//! short-circuits the obvious cases (an explicit block or auto-approve), and
//! only the *uncertain* events fall through to the model.
//! What: [`CompositeOverseer`] holds both overseers. `pre_tool_use` runs the
//! deterministic layer first; a definitive `Block` or `Respond` is returned
//! immediately, and only an `Allow`/`FlagForHuman` is escalated to the LLM.
//! Test: `cargo test -p trusty-mpm-daemon overseer_compose` checks the
//! short-circuit and escalation paths with a stub LLM.

use crate::core::overseer::{Overseer, OverseerContext, OverseerDecision};

/// An [`Overseer`] that chains a deterministic layer and an LLM layer.
///
/// Why: keeps the daemon's hook relay calling one `dyn Overseer` while still
/// benefiting from both strategies.
/// What: owns a primary (deterministic) and a secondary (LLM) overseer; the
/// primary's verdict is authoritative for `Block`, the secondary refines
/// `Allow`/`FlagForHuman`.
/// Test: `block_short_circuits`, `allow_escalates_to_llm`.
#[derive(Debug)]
pub struct CompositeOverseer {
    /// Fast rule-based layer, consulted first.
    primary: Box<dyn Overseer>,
    /// LLM-backed layer, consulted only for uncertain primary verdicts.
    secondary: Box<dyn Overseer>,
}

impl CompositeOverseer {
    /// Build a composite from a primary (deterministic) and secondary (LLM).
    ///
    /// Why: the daemon constructs this when both oversight layers are
    /// configured; an explicit constructor documents the ordering.
    /// What: stores the two overseers; `primary` is always consulted first.
    /// Test: `block_short_circuits`, `allow_escalates_to_llm`.
    pub fn new(primary: Box<dyn Overseer>, secondary: Box<dyn Overseer>) -> Self {
        Self { primary, secondary }
    }
}

impl Overseer for CompositeOverseer {
    fn pre_tool_use(&self, ctx: &OverseerContext) -> OverseerDecision {
        match self.primary.pre_tool_use(ctx) {
            // A definitive verdict from the rules wins outright — no LLM call.
            decision @ (OverseerDecision::Block { .. } | OverseerDecision::Respond { .. }) => {
                decision
            }
            // Allow / FlagForHuman are "uncertain"; escalate to the LLM, but
            // only when the LLM layer is actually active.
            _ if self.secondary.is_enabled() => self.secondary.pre_tool_use(ctx),
            other => other,
        }
    }

    fn post_tool_use(&self, ctx: &OverseerContext, output: &str) -> OverseerDecision {
        // Post-hoc output: the deterministic layer is authoritative.
        self.primary.post_tool_use(ctx, output)
    }

    fn session_question(&self, ctx: &OverseerContext, question: &str) -> OverseerDecision {
        // Prefer a deterministic auto-response; fall back to the LLM's verdict.
        match self.primary.session_question(ctx, question) {
            OverseerDecision::Respond { text } => OverseerDecision::Respond { text },
            _ if self.secondary.is_enabled() => self.secondary.session_question(ctx, question),
            other => other,
        }
    }

    fn is_enabled(&self) -> bool {
        // Oversight is active when either layer is.
        self.primary.is_enabled() || self.secondary.is_enabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::SessionId;

    /// A stub overseer returning a fixed verdict, for composition tests.
    #[derive(Debug)]
    struct StubOverseer {
        decision: OverseerDecision,
        enabled: bool,
    }

    impl Overseer for StubOverseer {
        fn pre_tool_use(&self, _ctx: &OverseerContext) -> OverseerDecision {
            self.decision.clone()
        }
        fn post_tool_use(&self, _ctx: &OverseerContext, _output: &str) -> OverseerDecision {
            self.decision.clone()
        }
        fn session_question(&self, _ctx: &OverseerContext, _q: &str) -> OverseerDecision {
            self.decision.clone()
        }
        fn is_enabled(&self) -> bool {
            self.enabled
        }
    }

    fn ctx() -> OverseerContext {
        OverseerContext::new(SessionId::new(), "tmpm-test", Some("Bash".into()), None)
    }

    #[test]
    fn block_short_circuits() {
        // A primary Block must win — the LLM is never consulted.
        let primary = Box::new(StubOverseer {
            decision: OverseerDecision::Block {
                reason: "rule".into(),
            },
            enabled: true,
        });
        let secondary = Box::new(StubOverseer {
            decision: OverseerDecision::Allow,
            enabled: true,
        });
        let composite = CompositeOverseer::new(primary, secondary);
        assert!(matches!(
            composite.pre_tool_use(&ctx()),
            OverseerDecision::Block { .. }
        ));
    }

    #[test]
    fn allow_escalates_to_llm() {
        // A primary Allow escalates: the LLM's Block verdict is returned.
        let primary = Box::new(StubOverseer {
            decision: OverseerDecision::Allow,
            enabled: true,
        });
        let secondary = Box::new(StubOverseer {
            decision: OverseerDecision::Block {
                reason: "llm".into(),
            },
            enabled: true,
        });
        let composite = CompositeOverseer::new(primary, secondary);
        assert!(matches!(
            composite.pre_tool_use(&ctx()),
            OverseerDecision::Block { .. }
        ));
    }

    #[test]
    fn disabled_llm_is_not_consulted() {
        // When the LLM layer is disabled, the primary Allow stands.
        let primary = Box::new(StubOverseer {
            decision: OverseerDecision::Allow,
            enabled: true,
        });
        let secondary = Box::new(StubOverseer {
            decision: OverseerDecision::Block {
                reason: "llm".into(),
            },
            enabled: false,
        });
        let composite = CompositeOverseer::new(primary, secondary);
        assert_eq!(composite.pre_tool_use(&ctx()), OverseerDecision::Allow);
    }

    #[test]
    fn enabled_when_either_layer_active() {
        let primary = Box::new(StubOverseer {
            decision: OverseerDecision::Allow,
            enabled: false,
        });
        let secondary = Box::new(StubOverseer {
            decision: OverseerDecision::Allow,
            enabled: true,
        });
        assert!(CompositeOverseer::new(primary, secondary).is_enabled());
    }
}
