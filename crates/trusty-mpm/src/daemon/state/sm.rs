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

use super::DaemonState;
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

/// The open intent a process takes on the SM and portfolio palaces (#9274).
///
/// Why: `tm daemon run` and `tm sm serve --stdio` both build a [`DaemonState`]
/// over the same root. Only one process can hold a palace's `kg.redb` as
/// writer, so the daemon owns the palaces and `tm sm serve` reads beside it.
/// What: `Writer` opens with `OpenIntent::Writer`, so a format-0 palace
/// migrates; `ReadOnly` opens with `OpenIntent::Observer` (#9659), which reads
/// a snapshot copy and never locks the live files, whether or not a daemon
/// runs. It cannot create a palace or read a format-0 one.
/// Test: `a_read_only_open_recalls_while_a_writer_holds_the_palace`,
/// `an_existing_format_0_sm_palace_migrates_at_open`,
/// `sm_serve_first_then_daemon_gets_the_writer_lock`,
/// `daemon_not_running_sm_serve_still_recalls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalaceAccess {
    /// The daemon: the owner of the palaces.
    Writer,
    /// `tm sm serve --stdio`: a reader beside a running daemon.
    ReadOnly,
}

impl PalaceAccess {
    /// The trusty-common open intent for this access.
    #[cfg(any(feature = "sm-memory", feature = "manager-memory"))]
    pub(crate) fn open_intent(self) -> trusty_common::memory_core::store::OpenIntent {
        use trusty_common::memory_core::store::OpenIntent;
        match self {
            Self::Writer => OpenIntent::Writer,
            // #9659: `tm sm serve` must leave the live files to the daemon.
            Self::ReadOnly => OpenIntent::Observer,
        }
    }
}

impl DaemonState {
    /// Construct default state under `root`, opening the palaces as writer.
    ///
    /// Why: the daemon (`tm daemon run`, through [`Self::new`]) owns the SM and
    /// portfolio palaces, so a format-0 palace migrates when it opens (#9274).
    /// What: `Self::with_root_access` with [`PalaceAccess::Writer`].
    /// Test: `pairing_persists_to_disk`, `pairing_reset_clears_disk`.
    pub fn with_root(root: std::path::PathBuf) -> Self {
        Self::with_root_access(root, PalaceAccess::Writer)
    }

    /// Default state for `tm sm serve --stdio`, opening the palaces read-only.
    ///
    /// Why: with the daemon running, a writer open of a palace it holds fails
    /// after the handoff retry, and `tm sm serve` would lose SM recall, the goal
    /// store and the portfolio palace (#9274).
    /// What: `Self::with_root_access` over the default framework root with
    /// [`PalaceAccess::ReadOnly`], wrapped in an `Arc`.
    /// Test: `a_read_only_open_recalls_while_a_writer_holds_the_palace`.
    pub fn shared_read_only() -> Arc<Self> {
        let root = super::overseer::default_framework_root();
        Arc::new(Self::with_root_access(root, PalaceAccess::ReadOnly))
    }
}

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
/// daemon startup). `access` is the open intent for that palace (#9274).
/// Returns an `Arc` for cheap sharing into handlers.
/// Test: `sm_agent_built_disabled_by_default`.
pub(super) fn build_session_manager_agent(
    framework_root: &Path,
    access: PalaceAccess,
) -> Arc<SessionManagerAgent> {
    let config = MpmConfig::load(framework_root).session_manager;
    let resolver = Arc::new(ProviderRegistry::from_env());
    let data_root = framework_root.join(SM_DATA_SUBDIR);

    #[cfg(feature = "sm-memory")]
    {
        let memory = open_sm_memory(&data_root, &config, access);
        Arc::new(SessionManagerAgent::with_runtime(
            config, resolver, data_root, memory,
        ))
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        let _ = access;
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
/// What: calls [`SmMemory::open_with_intent`](crate::core::sm::memory::SmMemory::open_with_intent)
/// rooted at `data_root` with the config's memory settings and the caller's
/// [`PalaceAccess`]; returns `None` on error, logged at ERROR for the writer
/// (#9659) and at WARN for `tm sm serve`.
/// Test: covered indirectly by the daemon construction tests (which run without
/// the feature) and by the SM-4 memory tests (under the feature).
#[cfg(feature = "sm-memory")]
fn open_sm_memory(
    data_root: &Path,
    config: &crate::core::sm::SessionManagerConfig,
    access: PalaceAccess,
) -> Option<crate::core::sm::memory::SmMemory> {
    // #9274: the daemon opens as writer, `tm sm serve` as a read-only client.
    let intent = access.open_intent();
    match crate::core::sm::memory::SmMemory::open_with_intent(
        data_root.join("palace"),
        &config.memory,
        intent,
    ) {
        Ok(mem) => Some(mem),
        // #9659: the daemon that cannot hold its SM palace fails loud.
        Err(e) if access == PalaceAccess::Writer => {
            tracing::error!(
                "session-manager memory palace: writer open failed: {e}; SM recall and \
                 remember are unavailable until the other holder exits and the daemon restarts"
            );
            None
        }
        Err(e) => {
            tracing::warn!("session-manager memory palace unavailable: {e}; recall disabled");
            None
        }
    }
}
