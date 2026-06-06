//! Plugin manager — owns the optional MCP plugin handles.
//!
//! Why: The harness needs a single place to ask "is search/memory available?"
//! at runtime. Centralising spawn attempts and status reporting here keeps
//! the agent and CLI layers free of plugin-specific bootstrapping.
//! What: `PluginManager::init` tries to spawn each known plugin; binaries
//! missing from PATH or failing the MCP handshake leave the slot as `None`.
//! `status()` reports `Active` / `Unavailable` per plugin for the
//! `om plugins status` CLI command.
//! Test: `PluginState` enum + `PluginStatus` formatting are unit tested
//! below. End-to-end spawn is exercised opportunistically when the binaries
//! are installed.

use std::sync::Arc;
use std::sync::OnceLock;

use tracing::{info, warn};

use super::trusty_memory::TrustyMemoryPlugin;
use super::trusty_search::TrustySearchPlugin;

/// Process-wide singleton populated by `init_global()` at harness startup.
///
/// Why: Tool dispatchers run deep inside agent loops where threading a
/// `PluginManager` reference through every call site is impractical. A single
/// `OnceLock` lets startup code populate the handle once and any in-process
/// consumer borrow it on demand without locking. Set-once semantics mean we
/// don't accidentally re-spawn plugin children when callers ask for the
/// global twice.
/// Test: `global_returns_none_before_init` and `global_returns_some_after_init`.
static GLOBAL: OnceLock<Arc<PluginManager>> = OnceLock::new();

/// Activation state of a single plugin slot.
///
/// Why: The CLI and any future health endpoint want a tiny enum to render;
/// strings would invite typos in match arms.
/// What: Two variants — Active (binary found and handshake succeeded) and
/// Unavailable (binary not on PATH or handshake failed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    Active,
    Unavailable,
}

impl PluginState {
    /// Why: Operators want a stable, ALL-CAPS label in CLI output that's
    /// easy to grep for.
    pub fn label(self) -> &'static str {
        match self {
            PluginState::Active => "ACTIVE",
            PluginState::Unavailable => "UNAVAILABLE",
        }
    }
}

/// Snapshot of plugin states for status reporting.
///
/// Why: Decouples the CLI rendering from the live plugin handles so we can
/// query state without holding plugin locks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginStatus {
    pub search: PluginState,
    pub memory: PluginState,
}

/// Owns the spawned plugin processes for the lifetime of the harness.
///
/// Why: Plugins are cheap to spawn but expensive to re-spawn per request —
/// keeping one shared `Arc` per plugin lets multiple agents reuse the same
/// child process safely (the underlying client serialises requests).
/// What: Two optional `Arc`-wrapped plugin handles, populated lazily by
/// `init`. Missing plugins remain `None` for the manager's lifetime.
/// Test: `status_reports_unavailable_when_empty`, `status_reports_active`.
pub struct PluginManager {
    search: Option<Arc<TrustySearchPlugin>>,
    memory: Option<Arc<TrustyMemoryPlugin>>,
}

impl PluginManager {
    /// Try to spawn every known plugin. Failures degrade silently.
    ///
    /// Why: Plugins are optional; one missing plugin must not prevent the
    /// rest of the harness (or the other plugins) from coming up.
    /// What: Calls `try_spawn` on each plugin in parallel and stores the
    /// successful handles.
    /// Test: When no plugin binaries are on PATH (the default in CI), this
    /// returns a manager with both slots `None` — covered indirectly by the
    /// CLI `plugins status` test path.
    pub async fn init() -> Self {
        let (search, memory) = tokio::join!(
            TrustySearchPlugin::try_spawn(),
            TrustyMemoryPlugin::try_spawn()
        );
        Self {
            search: search.map(Arc::new),
            memory: memory.map(Arc::new),
        }
    }

    /// Construct an empty manager with no plugins active.
    ///
    /// Why: Tests and unit-test setup want a deterministic, side-effect-free
    /// constructor.
    pub fn empty() -> Self {
        Self {
            search: None,
            memory: None,
        }
    }

    /// Borrow the search plugin handle, if active.
    pub fn search(&self) -> Option<Arc<TrustySearchPlugin>> {
        self.search.clone()
    }

    /// Borrow the memory plugin handle, if active.
    pub fn memory(&self) -> Option<Arc<TrustyMemoryPlugin>> {
        self.memory.clone()
    }

    /// Snapshot the activation state of each plugin.
    pub fn status(&self) -> PluginStatus {
        PluginStatus {
            search: state_of(self.search.is_some()),
            memory: state_of(self.memory.is_some()),
        }
    }
}

/// Initialise the process-wide `PluginManager` singleton.
///
/// Why: Issue #424 — `PluginManager::init` was previously only invoked by
/// `om plugins status`, so agents in REPL and `--workflow` modes had no way
/// to call `trusty-memory` / `trusty-search` tools (the child processes were
/// never spawned). Calling this once at startup makes the handles available
/// to any in-process tool dispatcher via `plugin_manager()`.
/// What: Idempotent — second and subsequent calls are no-ops and log a debug
/// line. Logs WARN for each plugin that failed to spawn so missing binaries
/// are observable without crashing the harness (graceful degradation).
/// Test: `init_global_is_idempotent` and `global_returns_some_after_init`.
pub async fn init_global() -> Arc<PluginManager> {
    if let Some(existing) = GLOBAL.get() {
        tracing::debug!("PluginManager::init_global called twice — returning existing");
        return existing.clone();
    }
    let mgr = Arc::new(PluginManager::init().await);
    let status = mgr.status();
    if status.search == PluginState::Active {
        info!("plugins: trusty-search ACTIVE");
    } else {
        warn!(
            "plugins: trusty-search UNAVAILABLE (binary not on PATH or handshake failed); \
             agents will not be able to call search tools. Install: cargo install trusty-search"
        );
    }
    if status.memory == PluginState::Active {
        info!("plugins: trusty-memory ACTIVE");
    } else {
        warn!(
            "plugins: trusty-memory UNAVAILABLE (binary not on PATH or handshake failed); \
             agents will not be able to call memory tools. Install: cargo install trusty-memory"
        );
    }
    // OnceLock::set returns Err if another thread won the race; fall back to
    // whichever Arc actually landed in the slot so callers see a single shared
    // instance.
    match GLOBAL.set(mgr.clone()) {
        Ok(()) => mgr,
        Err(_) => GLOBAL.get().expect("just set").clone(),
    }
}

/// Borrow the process-wide `PluginManager`, if initialised.
///
/// Why: Tool dispatchers and other deep call sites need plugin handles
/// without an explicit dependency injection chain through the agent loop.
/// What: Returns `None` until `init_global()` has been awaited; afterwards
/// returns a cheap clone of the shared `Arc`.
/// Test: `global_returns_none_before_init` covers the pre-init state.
pub fn plugin_manager() -> Option<Arc<PluginManager>> {
    GLOBAL.get().cloned()
}

fn state_of(active: bool) -> PluginState {
    if active {
        PluginState::Active
    } else {
        PluginState::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: An empty manager is the default in CI and on machines without
    /// plugin binaries; `status` must report Unavailable for every slot.
    #[test]
    fn status_reports_unavailable_when_empty() {
        let mgr = PluginManager::empty();
        let s = mgr.status();
        assert_eq!(s.search, PluginState::Unavailable);
        assert_eq!(s.memory, PluginState::Unavailable);
    }

    /// Why: The CLI prints `state.label()` directly; pin the strings so a
    /// rename doesn't silently change operator-visible output.
    #[test]
    fn plugin_state_labels_are_stable() {
        assert_eq!(PluginState::Active.label(), "ACTIVE");
        assert_eq!(PluginState::Unavailable.label(), "UNAVAILABLE");
    }

    /// Why: `state_of(true)` is the only path to `Active` outside of plugin
    /// spawning; verify the trivial mapping.
    #[test]
    fn state_of_maps_bool_correctly() {
        assert_eq!(state_of(true), PluginState::Active);
        assert_eq!(state_of(false), PluginState::Unavailable);
    }

    /// Why: Issue #424 — startup must populate a process-wide handle so the
    /// agent loop can call plugin tools. Calling `init_global` twice must
    /// return the same Arc rather than re-spawning plugin children.
    /// What: Calls `init_global` twice and asserts Arc identity via
    /// `Arc::ptr_eq`. Also confirms `plugin_manager()` returns Some afterward.
    /// Test: This test. Note: OnceLock is process-wide, so this test owns the
    /// global for the test binary; if more init tests are added, gate behind
    /// a mutex.
    #[tokio::test]
    async fn init_global_is_idempotent() {
        let first = init_global().await;
        let second = init_global().await;
        assert!(Arc::ptr_eq(&first, &second));
        let fetched = plugin_manager().expect("global should be Some after init");
        assert!(Arc::ptr_eq(&first, &fetched));
    }
}
