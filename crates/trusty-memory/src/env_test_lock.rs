//! The lib target's one env lock (#5937, #7995; hoisted out of `commands` by
//! #9271).
//!
//! Why: the engine's tests now share a process with the daemon's, and a
//! `--no-default-features` test build has no `commands` module, so the lock
//! has to live where every feature set compiles it.
//! Test: `env_lock_ratchet_tests::env_writing_tests_take_env_test_lock`.

/// Process-wide lock for tests that mutate `TRUSTY_DATA_DIR_OVERRIDE` and
/// related env vars.
///
/// Why: Rust's default test runner executes tests in the same process with
/// thread parallelism, but `std::env::set_var` / `remove_var` mutate
/// process-wide state. Tests that pin the data dir override to a tempdir
/// can otherwise observe each other's writes mid-run, with the symptom that
/// the handler resolves the *real* user data dir and the test asserts the
/// wrong directory. Acquiring this lock at the top of each env-touching
/// test forces them to run one-at-a-time without pulling in `serial_test`.
/// What: a `tokio::sync::Mutex<()>` (chosen over `std::sync::Mutex` so the
/// guard can be held across `.await` points without tripping clippy's
/// `await_holding_lock` lint). Tests hold the guard for the duration of
/// the env-sensitive section.
/// Test: indirectly via the integration tests in `prompt_context` and
/// `inbox_check`.
#[cfg(test)]
pub(crate) fn env_test_lock() -> &'static tokio::sync::Mutex<()> {
    use std::sync::OnceLock;
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
