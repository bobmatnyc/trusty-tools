//! Session Manager agent (DOC-14 spec §6.1, §7.5, §1A.1).
//!
//! Why: the SM is a daemon-side, interface-agnostic orchestrator that delegates
//! ALL work by spawning t-mpm sessions (spec §3). SM-7 turns the SM-1 skeleton
//! into a real *conversational* agent: it composes the SM building blocks —
//! the system prompt (SM-3), memory recall (SM-4), the rolling-context engine
//! (SM-5), and the multi-provider inference layer (SM-2) — into one chat turn,
//! and the daemon's `coordinator/chat` endpoint routes through it when
//! `[session_manager].enabled = true`. The goal store (SM-6) is constructed/
//! available but the goal-driven *delegation* loop (launch/observe/verify) is
//! SM-8 — SM-7 stops at the conversational turn.
//!
//! What: [`SessionManagerAgent`] holds the [`SessionManagerConfig`] plus an
//! optional *runtime* (a [`ProviderRegistry`] for inference, a `data_root` for
//! the per-conversation context engine's state files, and — behind the
//! `sm-memory` feature — an [`SmMemory`] handle for recall). Two constructors
//! exist: [`SessionManagerAgent::new`] (inert, config-only — the SM-1 contract,
//! still a runtime no-op) and [`SessionManagerAgent::with_runtime`] (the daemon
//! path with inference wired). [`SessionManagerAgent::chat`] (in the [`chat`]
//! submodule) drives one §7.5 working-prompt turn. The chat turn itself never
//! constructs a concrete provider — the [`ProviderRegistry`] does — so tests
//! inject a mock registry/provider with no network.
//! Test: `agent_new_is_inert`, `agent_default_is_disabled` (this file);
//! `agent/chat_tests.rs` covers the composed turn, degraded mode, and recall.

mod chat;

#[cfg(test)]
pub mod mock;

use std::path::PathBuf;
use std::sync::Arc;

use super::config::SessionManagerConfig;
use super::providers::TierResolver;

#[cfg(feature = "sm-memory")]
use super::memory::SmMemory;

pub use chat::{SmAgentError, SmChatOutcome};

/// The Session Manager orchestrator (SM-7: real conversational chat turn).
///
/// Why: gives the SM a single type that owns its configuration AND the runtime
/// handles a chat turn needs (inference registry, context-engine storage root,
/// optional memory palace). Holding them here means [`SessionManagerAgent::chat`]
/// takes only the per-call inputs (message + conv_id) and the call site (the
/// daemon endpoint, SM-7) stays thin.
/// What: a thin owner of [`SessionManagerConfig`] plus an optional
/// [`AgentRuntime`]. Built inert via [`Self::new`] (no runtime — a guaranteed
/// no-op, the SM-1 contract) or with inference via [`Self::with_runtime`].
/// Construction performs no I/O and makes no inference calls.
/// Test: `agent_new_is_inert`, `agent_default_is_disabled`; the chat path in
/// `chat_tests.rs`.
#[derive(Clone)]
pub struct SessionManagerAgent {
    /// The SM configuration this agent was built from (spec §10).
    config: SessionManagerConfig,
    /// Inference + storage runtime; `None` for the inert SM-1 constructor.
    runtime: Option<AgentRuntime>,
}

impl std::fmt::Debug for SessionManagerAgent {
    /// Why: the agent lives on `DaemonState`, which derives `Debug`; but the
    /// runtime holds non-`Debug` handles (`Arc<dyn TierResolver>`, `SmMemory`).
    /// A hand-written impl prints the auditable surface — enabled + whether a
    /// runtime is wired — without requiring those handles to be `Debug`.
    /// What: formats `enabled` and `has_runtime`.
    /// Test: compilation (DaemonState derives Debug).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManagerAgent")
            .field("enabled", &self.config.enabled)
            .field("has_runtime", &self.runtime.is_some())
            .finish()
    }
}

/// The runtime handles the SM chat turn composes over (SM-2/4/5).
///
/// Why: separating the runtime from the config lets the inert [`Self::new`]
/// constructor stay a pure value (no env reads, no provider construction) while
/// the daemon path ([`Self::with_runtime`]) carries the credential-aware
/// [`ProviderRegistry`], the storage root for context-engine state files, and —
/// when the `sm-memory` feature is on — the dedicated SM memory palace for
/// recall. Bundling them keeps `SessionManagerAgent` one field wider, not five.
/// What: the [`ProviderRegistry`] (built once from the environment), the
/// `data_root` under which per-`conv_id` context-engine state lives, and the
/// optional [`SmMemory`] recall handle.
/// Test: `chat_tests.rs` constructs this with a mock registry + tempdir.
#[derive(Clone)]
struct AgentRuntime {
    /// Per-tier provider resolver (SM-2). Resolves the orchestration/compaction
    /// tier per request and surfaces degraded mode when no provider has
    /// credentials. Behind `Arc<dyn TierResolver>` so the daemon wires the real
    /// [`ProviderRegistry`](super::providers::ProviderRegistry) and tests inject
    /// a mock with no network.
    resolver: Arc<dyn TierResolver>,
    /// Storage root for the rolling-context engine's per-conversation state
    /// files (SM-5). Each `conv_id` opens/persists under here.
    data_root: PathBuf,
    /// Dedicated SM memory palace for §7.5 step-3 recall (SM-4). Behind the
    /// `sm-memory` feature; recall is skipped gracefully when absent.
    #[cfg(feature = "sm-memory")]
    memory: Option<SmMemory>,
}

impl SessionManagerAgent {
    /// Construct an inert SM agent from its configuration (SM-1 contract).
    ///
    /// Why: callers that only need the config view (or tests asserting the
    /// no-op guarantee) build the agent without wiring inference. With no
    /// runtime, [`Self::chat`] always reports degraded — there is no provider —
    /// so a `new`-built agent never makes a network call.
    /// What: stores `config` with `runtime = None`. No I/O, no inference.
    /// Test: `agent_new_is_inert`.
    pub fn new(config: SessionManagerConfig) -> Self {
        Self {
            config,
            runtime: None,
        }
    }

    /// Construct an SM agent wired for inference (the daemon path, SM-7).
    ///
    /// Why: the daemon builds the agent once at startup with a credential-aware
    /// [`ProviderRegistry`] (from the process environment), the storage root for
    /// context-engine state, and — under `sm-memory` — the SM palace handle.
    /// Keeping construction side-effect-free (the registry is just resolved
    /// credentials; no provider is built until a chat turn) means wiring it up
    /// cannot regress the legacy path.
    /// What: stores `config` and an [`AgentRuntime`] bundling `registry`,
    /// `data_root`, and (feature-gated) `memory`.
    /// Test: `chat_tests.rs::chat_drives_full_turn_with_mock_provider`.
    #[cfg(feature = "sm-memory")]
    pub fn with_runtime(
        config: SessionManagerConfig,
        resolver: Arc<dyn TierResolver>,
        data_root: impl Into<PathBuf>,
        memory: Option<SmMemory>,
    ) -> Self {
        Self {
            config,
            runtime: Some(AgentRuntime {
                resolver,
                data_root: data_root.into(),
                memory,
            }),
        }
    }

    /// Construct an SM agent wired for inference (no-memory build).
    ///
    /// Why: the same daemon path as the `sm-memory` constructor, but for builds
    /// without the heavy memory-core feature. Recall is simply skipped — the
    /// chat turn still composes prompt + context engine + provider.
    /// What: stores `config` and an [`AgentRuntime`] with `registry` + `data_root`.
    /// Test: `chat_tests.rs::chat_drives_full_turn_with_mock_provider`.
    #[cfg(not(feature = "sm-memory"))]
    pub fn with_runtime(
        config: SessionManagerConfig,
        resolver: Arc<dyn TierResolver>,
        data_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config,
            runtime: Some(AgentRuntime {
                resolver,
                data_root: data_root.into(),
            }),
        }
    }

    /// Test-only: build an agent over an explicit resolver + data root.
    ///
    /// Why: cross-module tests (the daemon endpoint tests in `api_tests`) need to
    /// wire an enabled SM agent over a mock resolver with NO network and NO real
    /// palace, regardless of the `sm-memory` feature. This hides the feature-gated
    /// `with_runtime` signature behind one stable test seam.
    /// What: builds a runtime with `resolver` + `data_root` and (under the
    /// feature) `memory = None`.
    /// Test: used by `api_tests` SM-path tests.
    #[cfg(test)]
    pub fn for_test(
        config: SessionManagerConfig,
        resolver: Arc<dyn TierResolver>,
        data_root: impl Into<PathBuf>,
    ) -> Self {
        #[cfg(feature = "sm-memory")]
        {
            Self::with_runtime(config, resolver, data_root, None)
        }
        #[cfg(not(feature = "sm-memory"))]
        {
            Self::with_runtime(config, resolver, data_root)
        }
    }

    /// Borrow the agent's configuration.
    ///
    /// Why: collaborators (the endpoint, health reporting) read SM settings from
    /// one place; exposing it via an accessor keeps the field private.
    /// What: returns a shared reference to the held [`SessionManagerConfig`].
    /// Test: `agent_new_is_inert`.
    pub fn config(&self) -> &SessionManagerConfig {
        &self.config
    }

    /// Whether the SM is opted in.
    ///
    /// Why: the daemon's endpoint gates the SM path on this so a default-disabled
    /// config stays a strict no-op against the legacy overseer.
    /// What: returns `self.config.enabled`.
    /// Test: `agent_default_is_disabled`.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Whether the agent has an inference runtime wired.
    ///
    /// Why: the endpoint's routing decision (§5.3) is "SM path iff enabled AND a
    /// provider is available"; this reports whether inference was wired at all
    /// (the inert [`Self::new`] agent has no runtime → no provider → degraded).
    /// What: returns `true` when an [`AgentRuntime`] is present.
    /// Test: `agent_new_has_no_runtime`, `chat_tests.rs`.
    pub fn has_runtime(&self) -> bool {
        self.runtime.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: prove the SM-1 contract survives — an agent built from a default
    /// config retains that config and carries no runtime (a guaranteed no-op).
    /// What: builds via [`SessionManagerAgent::new`] and checks config + runtime.
    /// Test: this is the test.
    #[test]
    fn agent_new_is_inert() {
        let cfg = SessionManagerConfig::default();
        let agent = SessionManagerAgent::new(cfg.clone());
        assert_eq!(agent.config(), &cfg);
        assert!(!agent.has_runtime(), "new() must not wire a runtime");
    }

    /// Why: the default config disables the SM, so a default-built agent must
    /// report `is_enabled() == false` — the runtime no-op guarantee.
    /// What: builds from the default config and asserts it is disabled.
    /// Test: this is the test.
    #[test]
    fn agent_default_is_disabled() {
        let agent = SessionManagerAgent::new(SessionManagerConfig::default());
        assert!(!agent.is_enabled());
    }

    /// Why: `has_runtime` must report `false` for the inert constructor so the
    /// endpoint routes a `new()`-built agent straight to degraded/fallback.
    /// What: asserts `has_runtime()` is false for a `new()` agent.
    /// Test: this is the test.
    #[test]
    fn agent_new_has_no_runtime() {
        let agent = SessionManagerAgent::new(SessionManagerConfig::default());
        assert!(!agent.has_runtime());
    }
}
