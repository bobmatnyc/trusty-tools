//! Optional OpenRouter activity classifier gate (§8.3 of SPEC-SESSCTL-01, WI-5).
//!
//! Why: §8.3 makes the OpenRouter LLM classifier an explicit *fallback*, not the
//! default. AC-9 requires that with the classifier disabled (and no key) NO
//! outbound call to openrouter.ai is ever made. The gate that decides whether to
//! call the classifier is the single security/cost-relevant decision point, so
//! it is isolated here as a pure function that can be exhaustively unit-tested
//! without any network, env mutation, or LLM dependency.
//! What: [`ClassifierGate`] captures the three §8.3 preconditions (config flag,
//! key presence, SM-agent connection) and [`ClassifierGate::should_classify`]
//! returns `true` only when all three permit it. No HTTP client lives here —
//! WI-5 ships the gate; the actual OpenRouter call is wired by a later ticket
//! once the gate proves the classifier is permitted.
//! Test: `classifier_gate_*` in the inline `tests` module.

use crate::control::config::ObservabilityConfig;

/// The three §8.3 preconditions that gate the optional OpenRouter classifier.
///
/// Why: §8.3 lists three independent conditions, ALL of which must hold before
/// the daemon may call OpenRouter. Modeling them as one value with one decision
/// method keeps the policy in a single auditable place and prevents a caller
/// from checking only a subset.
/// What: `config_enabled` mirrors `[control_plane.observability].llm_classifier`;
/// `api_key_present` reflects whether `OPENROUTER_API_KEY` is set in the
/// environment; `sm_agent_connected` is `true` when a parent SM agent is
/// attached (in which case the SM provides inference and the daemon must NOT
/// call OpenRouter).
/// Test: `classifier_gate_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifierGate {
    /// `[control_plane.observability].llm_classifier == true`.
    pub config_enabled: bool,
    /// `OPENROUTER_API_KEY` is present in the environment.
    pub api_key_present: bool,
    /// An SM agent is currently connected (it provides inference; §8.3 #3).
    pub sm_agent_connected: bool,
}

impl ClassifierGate {
    /// Build a gate from the observability config and the two runtime signals.
    ///
    /// Why: callers hold a [`ObservabilityConfig`] and two booleans; this
    /// constructor keeps the field-mapping in one spot so the gate's three
    /// inputs always come from the same authoritative sources.
    /// What: copies `config.llm_classifier` into `config_enabled` and stores the
    /// two runtime signals verbatim.
    /// Test: `classifier_gate_from_config`.
    pub fn from_config(
        config: &ObservabilityConfig,
        api_key_present: bool,
        sm_agent_connected: bool,
    ) -> Self {
        Self {
            config_enabled: config.llm_classifier,
            api_key_present,
            sm_agent_connected,
        }
    }

    /// Decide whether the OpenRouter classifier may be invoked (§8.3).
    ///
    /// Why: this is the AC-9 enforcement point — if it returns `false` no
    /// outbound OpenRouter call is permitted, guaranteeing the default path
    /// (classifier off, no key) never touches the network.
    /// What: returns `true` only when ALL of: the config flag is on, the API key
    /// is present, AND no SM agent is connected (an attached SM provides the
    /// inference itself, per §8.3 condition 3).
    /// Test: `classifier_gate_all_off`, `classifier_gate_requires_all_three`,
    /// `classifier_gate_sm_connected_blocks`.
    pub fn should_classify(&self) -> bool {
        self.config_enabled && self.api_key_present && !self.sm_agent_connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(config_enabled: bool, api_key_present: bool, sm_agent_connected: bool) -> ClassifierGate {
        ClassifierGate {
            config_enabled,
            api_key_present,
            sm_agent_connected,
        }
    }

    #[test]
    fn classifier_gate_all_off() {
        // Default path: classifier disabled, no key, no SM → never classify.
        assert!(!gate(false, false, false).should_classify());
    }

    #[test]
    fn classifier_gate_requires_all_three() {
        // Only when config-on AND key-present AND no-SM does the gate open.
        assert!(gate(true, true, false).should_classify());

        // Each single missing precondition closes the gate.
        assert!(!gate(false, true, false).should_classify(), "config off blocks");
        assert!(!gate(true, false, false).should_classify(), "no key blocks");
    }

    #[test]
    fn classifier_gate_sm_connected_blocks() {
        // Even with config-on and key-present, an attached SM agent blocks the
        // classifier (the SM provides inference; §8.3 condition 3).
        assert!(!gate(true, true, true).should_classify());
    }

    #[test]
    fn classifier_gate_from_config() {
        let on = ObservabilityConfig {
            llm_classifier: true,
        };
        let g = ClassifierGate::from_config(&on, true, false);
        assert!(g.config_enabled);
        assert!(g.api_key_present);
        assert!(!g.sm_agent_connected);
        assert!(g.should_classify());

        let off = ObservabilityConfig::default();
        let g2 = ClassifierGate::from_config(&off, true, false);
        assert!(!g2.config_enabled);
        assert!(!g2.should_classify());
    }
}
