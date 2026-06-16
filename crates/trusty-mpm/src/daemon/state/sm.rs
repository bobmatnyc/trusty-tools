//! Session Manager agent construction for daemon startup (DOC-14 SM-7).
//!
//! Why: the daemon must build ONE [`SessionManagerAgent`] at startup, wired with
//! the credential-aware provider resolver (SM-2), the storage root for the
//! rolling-context engine's state files (SM-5), and — under `sm-memory` — the
//! dedicated SM memory palace (SM-4). Isolating that wiring here keeps the
//! `DaemonState` constructors tidy and gives both `new()` and `with_paths()` one
//! shared builder. The agent is built unconditionally (cheap: config + a
//! credentials probe; no provider is constructed until a chat turn) so the
//! endpoint can consult `is_enabled()` + `has_runtime()` at request time. With
//! `[session_manager].enabled = false` (the default) the agent is wired but the
//! endpoint never routes through it — a strict no-op against the legacy overseer.
//! What: [`build_session_manager_agent`] loads `[session_manager]` from the
//! framework config, builds a [`ProviderRegistry`] from the environment, and
//! constructs the agent rooted at `<framework_root>/sm`.
//! Test: `sm_agent_built_disabled_by_default` in `super::tests`.

use std::path::Path;
use std::sync::Arc;

use crate::core::config::MpmConfig;
use crate::core::sm::SessionManagerAgent;
use crate::core::sm::providers::ProviderRegistry;

/// Subdirectory under the framework root that holds SM runtime state.
///
/// Why: the rolling-context engine's per-conversation state files and the SM
/// memory palace both live under one well-known directory, mirroring how the
/// managed session store lives under `<root>/session-manager`.
/// What: the `sm` segment joined onto the framework root.
/// Test: exercised via `build_session_manager_agent`.
const SM_DATA_SUBDIR: &str = "sm";

/// Build the daemon's [`SessionManagerAgent`] rooted under `framework_root`.
///
/// Why: SM-7 wires the SM into the daemon. This is the single place that turns
/// the on-disk `[session_manager]` config into a live agent with inference,
/// context storage, and (feature-gated) memory. Building it unconditionally —
/// even when disabled — means the endpoint can make its routing decision from
/// the agent's `is_enabled()`/`has_runtime()` without re-reading config.
/// What: loads `MpmConfig` from `framework_root` (graceful defaults on a missing/
/// malformed file), builds a credential-aware [`ProviderRegistry::from_env`], and
/// constructs the agent with `data_root = <framework_root>/sm`. Under
/// `sm-memory`, also opens the dedicated SM palace under that root (a palace-open
/// failure is logged and the agent is built WITHOUT recall rather than failing
/// daemon startup). Returns an `Arc` for cheap sharing into handlers.
/// Test: `sm_agent_built_disabled_by_default`.
pub(super) fn build_session_manager_agent(framework_root: &Path) -> Arc<SessionManagerAgent> {
    let config = MpmConfig::load(framework_root).session_manager;
    let resolver = Arc::new(ProviderRegistry::from_env());
    let data_root = framework_root.join(SM_DATA_SUBDIR);

    #[cfg(feature = "sm-memory")]
    {
        let memory = open_sm_memory(&data_root, &config);
        Arc::new(SessionManagerAgent::with_runtime(
            config, resolver, data_root, memory,
        ))
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        Arc::new(SessionManagerAgent::with_runtime(
            config, resolver, data_root,
        ))
    }
}

/// Open the dedicated SM memory palace, degrading to `None` on failure.
///
/// Why: recall enriches the working prompt but must never block daemon startup;
/// a palace-open failure (unwritable root, corrupt store) degrades to "no recall"
/// — the chat turn still composes prompt + context + provider.
/// What: calls [`SmMemory::open`](crate::core::sm::memory::SmMemory::open) rooted
/// at `data_root` with the config's memory settings; logs and returns `None` on
/// error.
/// Test: covered indirectly by the daemon construction tests (which run without
/// the feature) and by the SM-4 memory tests (under the feature).
#[cfg(feature = "sm-memory")]
fn open_sm_memory(
    data_root: &Path,
    config: &crate::core::sm::SessionManagerConfig,
) -> Option<crate::core::sm::memory::SmMemory> {
    match crate::core::sm::memory::SmMemory::open(data_root.join("palace"), &config.memory) {
        Ok(mem) => Some(mem),
        Err(e) => {
            tracing::warn!("session-manager memory palace unavailable: {e}; recall disabled");
            None
        }
    }
}
