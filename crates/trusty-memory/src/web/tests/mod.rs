//! Integration-style tests for the `web` module HTTP handlers.
//!
//! Why: Each submodule exercises a cohesive slice of the handler surface
//! in isolation against an in-process `AppState` backed by a `tempdir` — no
//! live daemon or network required. Splitting by feature area keeps each
//! file under the 500-line cap while preserving all original test coverage.
//! What: Shared test helpers (`test_state`) plus submodules grouping tests
//! by feature area.
//! Test: Run with `cargo test -p trusty-memory`.

use crate::AppState;

/// Build a fresh `AppState` rooted in an ephemeral `tempdir`.
///
/// Why: Each test needs an isolated data root so palace creates / drawers
/// never collide across concurrent test threads.
/// What: Creates a `tempdir`, leaks it (so the directory persists for the
/// test's lifetime without being explicitly held), bypasses the project-slug
/// enforcement gate (`TRUSTY_SKIP_PALACE_ENFORCEMENT=1`), and flips the
/// daemon readiness to `Ready` so handlers don't reject requests with a
/// "warming" error.
/// Test: Every test that calls `test_state()`.
pub(crate) fn test_state() -> AppState {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    // Issue #88: bypass the project-slug enforcement gate so tests can
    // create palaces with arbitrary names without having a real project
    // root on disk. The env var is harmless once set to "1" because all
    // tests in this process use the same setting.
    // SAFETY: no other thread reads/writes this var concurrently — the
    // const value "1" is idempotent and the write happens before any
    // test that creates a palace via the HTTP layer.
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(root);
    // Pre-existing tests exercise functional paths — flip to Ready so the
    // issue #911 warming preflight does not reject them.
    state.set_ready();
    state
}

mod activity_tests;
mod admin_tests;
mod attribution_tests;
mod chat_tests;
mod dream_sse_tests;
mod health_tests;
mod kg_tests;
mod palace_crud_tests;
mod palace_tests;
mod prompt_tests;
mod recall_tests;
