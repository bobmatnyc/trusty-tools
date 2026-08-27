//! Shared fixtures for the `trusty-memory` integration tests.
//!
//! Why: each file under `tests/` compiles as its own binary, so a fixture used
//! by more than one of them has to live in a module they all declare. This is
//! that module.
//! What: [`DaemonGuard`], the owning handle for a spawned
//! `trusty-memory serve --foreground`.
//! Test: exercised by every test that spawns a daemon —
//! `serve_stdio_concurrent_e2e`, `serve_stdio_e2e`, `codex_stdio_e2e_5265`.

use std::process::Child;

/// Owns a spawned `serve --foreground` daemon and kills it on drop.
///
/// Why (#5188): the spawn helpers returned a raw `Child` and relied on an
/// explicit `.kill()` at the end of the test body. Every `assert!` between
/// those two points — including the readiness-poll assert inside the spawn
/// helper itself — orphans the daemon on failure. An orphan re-parents to PID
/// 1, keeps its dream loop running against the temp data dir, and on the
/// reporting machine six of them accumulated from one `cargo test` run and
/// went on calling a local model. `Drop` runs on the panic unwind, so the
/// daemon dies with the test that spawned it.
/// What: kills and reaps in `Drop`. Both results are discarded — the child may
/// already have exited, and a teardown error must not mask the test's own
/// failure.
/// Test: `serve_stdio_concurrent_two_bridges_both_work` and the other daemon
/// tests; the guarantee itself is structural.
pub struct DaemonGuard {
    child: Child,
}

impl DaemonGuard {
    /// Take ownership of an already-spawned daemon process.
    pub fn new(child: Child) -> Self {
        Self { child }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
