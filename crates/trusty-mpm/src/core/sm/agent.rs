//! Session Manager agent skeleton (DOC-14 spec §6.1, §1A.1).
//!
//! Why: the SM is a daemon-side, interface-agnostic orchestrator that — once
//! complete — delegates ALL work by spawning t-mpm sessions, never implementing
//! directly (spec §3 prohibitions). SM-1 lands only the skeleton: a struct that
//! holds the SM config so later tickets can fill in the moving parts
//! (`SmProviders` §5/SM-2, `SmMemory` §8/SM-4, `SmContextEngine` §7/SM-5,
//! `SmGoals` §9/SM-6) and the JSON-RPC/HTTP adapters (SM-7/SM-STDIO). Crucially,
//! constructing the agent is inert: with `enabled = false` (the default) nothing
//! in this skeleton runs, so existing behavior is unchanged.
//! What: [`SessionManagerAgent`] stores a clone of [`SessionManagerConfig`] and
//! exposes `new`, `config`, and `is_enabled`. It has NO inference, NO memory, NO
//! loop yet — those arrive in SM-2..SM-8.
//! Test: `agent_new_is_inert`, `agent_default_is_disabled` below.

use super::config::SessionManagerConfig;

/// The Session Manager orchestrator (skeleton — SM-1).
///
/// Why: gives the SM build a stable type to grow into. Later tickets attach the
/// provider abstraction, memory palace, rolling-context engine, and goal store
/// as fields here, and the stdio/HTTP adapters call into it. Holding the config
/// now means every later subsystem reads its settings from one place.
/// What: a thin owner of [`SessionManagerConfig`]. Construction performs no I/O,
/// spawns no sessions, and makes no inference calls — it is a pure value, so an
/// SM with `enabled = false` is a guaranteed runtime no-op.
/// Test: `agent_new_is_inert`, `agent_default_is_disabled`.
#[derive(Debug, Clone)]
pub struct SessionManagerAgent {
    /// The SM configuration this agent was built from (spec §10).
    config: SessionManagerConfig,
}

impl SessionManagerAgent {
    /// Construct an SM agent from its configuration.
    ///
    /// Why: callers (the future stdio/HTTP adapters, SM-7/SM-STDIO) need one
    /// entry point that takes the parsed `[session_manager]` config. SM-1 keeps
    /// it side-effect-free so wiring it up early cannot regress anything.
    /// What: stores `config` and returns the agent. No I/O, no inference, no
    /// session spawning.
    /// Test: `agent_new_is_inert`.
    pub fn new(config: SessionManagerConfig) -> Self {
        Self { config }
    }

    /// Borrow the agent's configuration.
    ///
    /// Why: later subsystems (inference, memory, context, goals) read their
    /// settings from the SM config; exposing it via an accessor keeps the field
    /// private while letting collaborators read it.
    /// What: returns a shared reference to the held [`SessionManagerConfig`].
    /// Test: `agent_new_is_inert`.
    pub fn config(&self) -> &SessionManagerConfig {
        &self.config
    }

    /// Whether the SM is opted in.
    ///
    /// Why: the daemon's wiring (SM-7) will gate the SM path on this so the
    /// default-disabled config stays a strict no-op against the legacy overseer.
    /// What: returns `self.config.enabled`.
    /// Test: `agent_default_is_disabled`.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: prove that constructing the skeleton from a default config succeeds
    /// and is inert — it merely retains the config, performing no side effects.
    /// What: builds an agent from `SessionManagerConfig::default()` and checks
    /// the held config round-trips.
    /// Test: this is the test.
    #[test]
    fn agent_new_is_inert() {
        let cfg = SessionManagerConfig::default();
        let agent = SessionManagerAgent::new(cfg.clone());
        assert_eq!(agent.config(), &cfg);
    }

    /// Why: the default config disables the SM, so a default-built agent must
    /// report `is_enabled() == false` — the runtime no-op guarantee.
    /// What: builds an agent from the default config and asserts it is disabled.
    /// Test: this is the test.
    #[test]
    fn agent_default_is_disabled() {
        let agent = SessionManagerAgent::new(SessionManagerConfig::default());
        assert!(!agent.is_enabled());
    }
}
