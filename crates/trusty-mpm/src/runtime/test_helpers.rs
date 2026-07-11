//! Shared test doubles for the runtime adapter unit tests.
//!
//! Why: the `claude_code`, `tcode`, and `mod` test blocks all need an in-memory
//! [`ManagedTmuxDriver`] that records the lines sent to it so adapter spawn paths
//! can be exercised without a real `tmux` binary. Keeping three byte-identical
//! copies of that fake (one per test module) was a maintenance hazard flagged in
//! the #1212 review (#1213); this module is the single source of truth.
//! What: defines [`FakeTmux`], a `Mutex`-guarded recorder implementing
//! [`ManagedTmuxDriver`]; every method is a no-op except `send_line`, which
//! appends `(session_name, text)`, and `send_line_to_pane`, which appends
//! `(session_name, pane_id, text)` (sibling-window hijack fix, follow-up to
//! #2456), so tests can assert exactly what was sent and whether it was
//! session-scoped or pane-scoped.
//! Test: consumed by the `tcode`/`claude_code`/`mod` test blocks; its own
//! behaviour is verified transitively through those tests (e.g.
//! `tcode_adapter_spawn_sends_run_task`).

use std::sync::{Arc, Mutex};

use crate::session_manager::{ManagedError, ManagedTmuxDriver};

/// In-memory [`ManagedTmuxDriver`] that records every `send_line` /
/// `send_line_to_pane` call.
///
/// Why: adapter tests must assert the exact shell line an adapter sends to the
/// pane without spawning `tmux`; recording sends in a `Mutex<Vec<…>>` makes that
/// assertion trivial and deterministic. Separating the two logs
/// (`sends` vs. `pane_sends`) lets a test assert not just WHAT was sent but
/// whether it targeted the session-scoped "active pane" or a SPECIFIC pane —
/// the exact distinction the sibling-window hijack fix depends on.
/// What: stores `Vec<(session_name, text)>` (`sends`) and
/// `Vec<(session_name, pane_id, text)>` (`pane_sends`) behind separate
/// `Mutex`es; all driver methods are inert except those two. Both are
/// `pub(crate)` so test blocks can lock and inspect them.
/// Test: exercised by every runtime adapter test that calls `spawn`/
/// `spawn_resume`.
pub(crate) struct FakeTmux {
    pub(crate) sends: Mutex<Vec<(String, String)>>,
    /// Records every `send_line_to_pane` call as `(session_name, pane_id,
    /// text)` (sibling-window hijack fix, follow-up to #2456).
    pub(crate) pane_sends: Mutex<Vec<(String, String, String)>>,
    /// Records every `set_environment` call as `(session, key, value)` (#2157
    /// item 1) so adapter tests can assert `TM_MANAGED_SESSION_ID` is durably
    /// published at spawn/resume, not just exported into the pane shell line.
    pub(crate) env_sets: Mutex<Vec<(String, String, String)>>,
}

impl FakeTmux {
    /// Construct an empty recorder wrapped in an `Arc`.
    ///
    /// Why: adapters take `Arc<dyn ManagedTmuxDriver + Send + Sync>`, so the fake
    /// is handed out pre-wrapped; returning `Arc<Self>` (not `Arc<dyn …>`) lets
    /// the caller keep a typed handle for `clone()` + later inspection.
    /// What: allocates a `FakeTmux` with empty send logs inside an `Arc`.
    /// Test: called at the top of every runtime adapter test.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            sends: Mutex::new(Vec::new()),
            pane_sends: Mutex::new(Vec::new()),
            env_sets: Mutex::new(Vec::new()),
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

    /// Records `(name, pane_id, text)` instead of delegating to `send_line`
    /// (the trait default) — a test asserting `pane_sends` got the call and
    /// `sends` stayed empty is exactly what proves the adapter chose the
    /// pane-scoped path over the session-scoped one.
    fn send_line_to_pane(&self, name: &str, pane_id: &str, text: &str) -> Result<(), ManagedError> {
        self.pane_sends
            .lock()
            .expect("FakeTmux pane-send log mutex poisoned")
            .push((name.to_owned(), pane_id.to_owned(), text.to_owned()));
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }

    fn session_exists(&self, _name: &str) -> bool {
        false
    }

    fn set_environment(&self, name: &str, key: &str, value: &str) -> Result<(), ManagedError> {
        self.env_sets
            .lock()
            .expect("FakeTmux env-set log mutex poisoned")
            .push((name.to_owned(), key.to_owned(), value.to_owned()));
        Ok(())
    }
}
