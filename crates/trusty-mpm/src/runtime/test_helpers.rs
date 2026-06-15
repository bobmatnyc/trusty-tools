//! Shared test doubles for the runtime adapter unit tests.
//!
//! Why: the `claude_code`, `tcode`, and `mod` test blocks all need an in-memory
//! [`ManagedTmuxDriver`] that records the lines sent to it so adapter spawn paths
//! can be exercised without a real `tmux` binary. Keeping three byte-identical
//! copies of that fake (one per test module) was a maintenance hazard flagged in
//! the #1212 review (#1213); this module is the single source of truth.
//! What: defines [`FakeTmux`], a `Mutex`-guarded recorder implementing
//! [`ManagedTmuxDriver`]; every method is a no-op except `send_line`, which
//! appends `(session_name, text)` so tests can assert exactly what was sent.
//! Test: consumed by the `tcode`/`claude_code`/`mod` test blocks; its own
//! behaviour is verified transitively through those tests (e.g.
//! `tcode_adapter_spawn_sends_run_task`).

use std::sync::{Arc, Mutex};

use crate::session_manager::{ManagedError, ManagedTmuxDriver};

/// In-memory [`ManagedTmuxDriver`] that records every `send_line` call.
///
/// Why: adapter tests must assert the exact shell line an adapter sends to the
/// pane without spawning `tmux`; recording sends in a `Mutex<Vec<…>>` makes that
/// assertion trivial and deterministic.
/// What: stores a `Vec<(session_name, text)>` behind a `Mutex`; all driver
/// methods are inert except `send_line`, which pushes the call. `sends` is
/// `pub(crate)` so test blocks can lock and inspect it.
/// Test: exercised by every runtime adapter test that calls `spawn`.
pub(crate) struct FakeTmux {
    pub(crate) sends: Mutex<Vec<(String, String)>>,
}

impl FakeTmux {
    /// Construct an empty recorder wrapped in an `Arc`.
    ///
    /// Why: adapters take `Arc<dyn ManagedTmuxDriver + Send + Sync>`, so the fake
    /// is handed out pre-wrapped; returning `Arc<Self>` (not `Arc<dyn …>`) lets
    /// the caller keep a typed handle for `clone()` + later inspection.
    /// What: allocates a `FakeTmux` with an empty send log inside an `Arc`.
    /// Test: called at the top of every runtime adapter test.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            sends: Mutex::new(Vec::new()),
        })
    }
}

impl ManagedTmuxDriver for FakeTmux {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.sends
            .lock()
            .expect("FakeTmux send log mutex poisoned")
            .push((name.to_owned(), text.to_owned()));
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: u32) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }

    fn session_exists(&self, _name: &str) -> bool {
        false
    }
}
