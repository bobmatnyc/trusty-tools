//! Daemon-owned [`ManagerState`] for the Layer-3 portfolio manager (WI-1, #2578).
//!
//! Why: DOC-36 §3.1 makes `tm manager` a component INSIDE the existing `tm`
//! daemon ("daemon as source of truth"), with a `ManagerState` sitting alongside
//! `DaemonState` — exactly as the L2 proxy focus store does. This is the single
//! struct every manager route reaches through; owning it on [`DaemonState`]
//! (mirroring `proxy_focus_store`) keeps one source of truth and no global
//! statics. Phase 1a threads in the portfolio palace handle (§3.4, WI-5); later
//! phases attach the configured `trusty_common::inference::InferenceAdapter`
//! (§3.3) and the proactive poll loop (§6 phase 3) onto this same struct without
//! re-plumbing route wiring.
//! What: [`ManagerState`] holds the [`PortfolioPalace`] provisioned at daemon
//! startup. Built once via [`ManagerState::provision`] from the framework root;
//! shared into handlers as an `Arc<ManagerState>` via
//! [`DaemonState::manager_state`](crate::daemon::state::DaemonState::manager_state).
//! Test: `manager_state_provisions_palace` here plus the HTTP-level
//! `manager_version_route_reports_capabilities` in `tests/manager_routes.rs`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::actuator::ManagerActuator;
use super::chat_store::ChatStore;
use super::inference::ManagerInference;
use super::memory::PortfolioPalace;

/// The daemon-owned state for the `tm manager` (Layer-3) surface.
///
/// Why: a single injectable struct for the manager routes, provisioned once at
/// startup and shared read-only through `Arc` — the same ownership shape as
/// every other daemon subsystem. Keeping it distinct from `DaemonState` (rather
/// than scattering manager fields across it) means the L3 surface has one
/// cohesive home the later phases grow into.
/// What: holds the [`PortfolioPalace`] (WI-5), the [`ManagerInference`] seam for
/// the digest/chat LLM calls (WI-3/WI-4, §3.3), and the conversation-keyed
/// [`ChatStore`] (WI-4). The proactive poll loop attaches here in phase 3 (§6).
/// Test: `manager_state_provisions_palace`.
pub struct ManagerState {
    /// The portfolio-scoped memory palace (WI-5, #2582), provisioned at startup.
    palace: PortfolioPalace,
    /// The inference resolution seam for digest/chat (WI-3/WI-4, §3.3).
    inference: ManagerInference,
    /// Conversation-keyed recent-turn store for the chat loop (WI-4).
    conversations: ChatStore,
    /// Optional TEST override for the `/manager/act` execution seam (WI-9, #2586).
    ///
    /// `None` in production (the handler builds a fresh production
    /// [`super::actuator::ProxyActuator`] per request); `Some` when the hermetic
    /// suite installs a double via [`ManagerState::set_actuator`], mirroring how
    /// [`ManagerInference::set_adapter`] swaps the inference seam.
    actuator: Mutex<Option<Arc<dyn ManagerActuator>>>,
}

impl std::fmt::Debug for ManagerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let has_actuator = self.actuator.lock().map(|g| g.is_some()).unwrap_or(false);
        f.debug_struct("ManagerState")
            .field("palace", &self.palace)
            .field("inference", &self.inference)
            .field("conversations", &self.conversations)
            .field("actuator_override", &has_actuator)
            .finish()
    }
}

impl ManagerState {
    /// Provision the manager state at daemon startup (§7 Q3 auto-create).
    ///
    /// Why: DOC-36 §7 Q3 fixes provisioning as "auto-created at daemon startup",
    /// so the daemon constructs this eagerly in its own constructor. It must be
    /// infallible from the daemon's perspective — a palace that cannot be opened
    /// degrades inside [`PortfolioPalace::provision`], and the inference seam
    /// degrades at call time when no provider is configured (§4 degrade bar), so
    /// neither can fail startup.
    /// What: provisions the portfolio palace rooted at `framework_root`, the
    /// credential-backed inference seam, and an empty conversation store.
    /// Test: `manager_state_provisions_palace`.
    pub fn provision(framework_root: &Path) -> Self {
        Self {
            palace: PortfolioPalace::provision(framework_root),
            inference: ManagerInference::provision(),
            conversations: ChatStore::default(),
            actuator: Mutex::new(None),
        }
    }

    /// The portfolio palace handle.
    ///
    /// Why: the `version` capabilities stub and later-phase consumers read palace
    /// availability / write observations through this handle.
    /// What: a shared reference to the provisioned [`PortfolioPalace`].
    /// Test: `manager_state_provisions_palace`.
    pub fn palace(&self) -> &PortfolioPalace {
        &self.palace
    }

    /// The inference resolution seam for digest/chat.
    ///
    /// Why: the `/manager/digest` and `/manager/chat` handlers resolve a live
    /// adapter (or a typed degrade) through this; the hermetic suite swaps its
    /// source through the shared `Arc` via [`ManagerInference::set_adapter`].
    /// What: a shared reference to the provisioned [`ManagerInference`].
    /// Test: `resolve_reports_no_provider_when_unconfigured` (inference module);
    /// `tests/manager_inference.rs`.
    pub fn inference(&self) -> &ManagerInference {
        &self.inference
    }

    /// The conversation-keyed chat turn store.
    ///
    /// Why: `/manager/chat` reads recent turns for context and records each
    /// completed exchange through this shared, bounded store.
    /// What: a shared reference to the [`ChatStore`].
    /// Test: `record_and_history_round_trips` (chat_store module);
    /// `tests/manager_inference.rs`.
    pub fn conversations(&self) -> &ChatStore {
        &self.conversations
    }

    /// The installed `/manager/act` execution-seam override, if any.
    ///
    /// Why: the act handler consults this before building a production actuator so
    /// the hermetic suite can run the propose→confirm flow over a test-double
    /// launcher + `SessionProxy` (WI-9, #2586). `None` in production.
    /// What: clones the `Arc<dyn ManagerActuator>` under the lock, or `None`.
    /// Test: exercised via `tests/manager_routing.rs`.
    pub fn actuator_override(&self) -> Option<Arc<dyn ManagerActuator>> {
        self.actuator
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Install a test override for the `/manager/act` execution seam.
    ///
    /// Why: mirrors [`ManagerInference::set_adapter`] — the hermetic suite builds a
    /// real `DaemonState` and installs a [`super::actuator::ProxyActuator`] over
    /// doubles through the shared `Arc<ManagerState>`, no bespoke constructor.
    /// This reconfigures ACTION execution for a test; it is never set in production.
    /// What: stores `actuator` as the override.
    /// Test: `tests/manager_routing.rs`.
    pub fn set_actuator(&self, actuator: Arc<dyn ManagerActuator>) {
        *self.actuator.lock().unwrap_or_else(|p| p.into_inner()) = Some(actuator);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Provisioning always yields a palace handle carrying the stable id — the
    /// availability of that palace is a build/runtime concern the palace tests
    /// cover; here we assert the state wires one through at all.
    #[test]
    fn manager_state_provisions_palace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = ManagerState::provision(dir.path());
        assert_eq!(state.palace().id(), super::super::PORTFOLIO_PALACE_ID);
    }
}
