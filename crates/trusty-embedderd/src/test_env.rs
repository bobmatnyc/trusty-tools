//! The one env lock for this crate's lib tests (#5937 pattern, #8616).
//!
//! Why: env vars are process-global and tests run in parallel. Every lib test
//! that writes `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` or `ORT_DYLIB_PATH` holds
//! this lock, so no writer races another writer or a reader.
//! What: [`env_lock`] returns the guard of one static mutex, recovering from a
//! poisoned lock so one failing test does not fail every later one.
//! Test: used by the env-mutating tests in `readiness` and `ort_probe`.

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire [`ENV_LOCK`], recovering from poison (the mutex guards no data).
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
