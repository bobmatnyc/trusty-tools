//! The ONE process-environment lock shared by every env-touching test in this
//! crate's `embedder` module tree.
//!
//! Why: env vars are process-global and Rust's test harness runs tests in
//! parallel threads within a single binary, so concurrent `setenv`/`getenv`
//! (even on disjoint keys) is unsound across libc implementations — Rust 2024
//! reflects this by marking `std::env::set_var` `unsafe`. It is ALSO a
//! correctness hazard beyond soundness: a test that mutates
//! `TRUSTY_EMBEDDER_MODEL` while another resolves the default model makes the
//! second test silently exercise the wrong model (issue #3711).
//!
//! This module exists because "one shared lock" was previously only true
//! per-file: `mod.rs`'s test module and `provider_tests.rs` each declared their
//! OWN static, and two independent locks do not serialise against each other —
//! so the #3711 race could still occur between a `TRUSTY_DEVICE` /
//! `TRUSTY_EMBEDDER_MODEL` mutation in one file and a model resolution in the
//! other. Hoisting the lock (and its RAII guard) to module scope makes the
//! guarantee real crate-wide instead of aspirational.
//!
//! What: [`ENV_LOCK`], a `std::sync::Mutex` every env-touching test in
//! `embedder` must hold for the whole window in which the env is mutated OR
//! read, plus [`EnvVarGuard`], the RAII set/restore helper.
//!
//! It is deliberately a SYNCHRONOUS mutex. An async test must therefore not
//! hold the guard across an `.await` (`clippy::await_holding_lock`); the
//! convention in this module tree is a plain `#[test]` driving an explicit
//! `tokio::runtime::Builder::new_current_thread()` runtime, which keeps the
//! lock held across the whole construct-and-embed window without an async lock
//! that sync tests could not share.
//! Test: used by `mod.rs`'s `mod tests`, `tests/reference_accuracy_tests.rs`,
//! and `provider_tests.rs`.

/// Process-global lock guarding every test in the `embedder` module tree that
/// mutates or reads the process environment.
///
/// Why/What: see the module docs — one lock for the whole module tree, held
/// across the entire mutate-and-observe window.
/// Test: `resolve_default_embedding_model_int8_opt_in`,
/// `default_model_matches_sentence_transformers_reference`,
/// `resolve_expected_python_provider_forces_cpu`.
pub(super) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire [`ENV_LOCK`], recovering from a poisoned lock instead of panicking.
///
/// Why (issue #4940): `ENV_LOCK.lock().unwrap()` turned one failing test into
/// seven. When `default_model_matches_sentence_transformers_reference` panicked
/// on a CI runner that could not download the ONNX model, it poisoned the lock,
/// and the six `resolve_*` tests — pure model/provider/cache-dir resolution
/// logic that never touches fastembed — then failed with `PoisonError` instead
/// of passing, burying the one real cause under six casualties.
///
/// Why recovery is correct here, not papering over a failure: poisoning exists
/// to warn that DATA behind the mutex may be half-updated. `ENV_LOCK` is a
/// `Mutex<()>` — it guards no data, only serialisation order. The state it
/// actually protects is the process environment, and that is restored by
/// [`EnvVarGuard`]'s `Drop`, which runs during the panicking test's unwind. So
/// the next `lock()` caller observes a consistent environment; propagating
/// poison communicates nothing except "some other test failed", which the test
/// harness already reports.
/// What: `lock()`, mapping `PoisonError` to the guard it carries.
/// Test: `poisoned_env_lock_is_still_acquirable`.
pub(super) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII helper: set or clear an env var for the duration of a test and restore
/// it on drop.
///
/// Why: env vars are process-global; without restore, a stray value would skew
/// every later resolver call in the same binary. Restoring on `Drop` means an
/// assertion failure (an unwind) cannot leak the value into sibling tests
/// either.
/// What: captures the prior value, applies the new one (or removes it for
/// `None`), and reinstates the prior value on `Drop`.
/// Test: used by the `cuda_options_*`, `resolve_default_embedding_model_*`, and
/// `resolve_expected_python_provider_*` tests.
pub(super) struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    /// Apply `value` to `key`, remembering the previous value for `Drop`.
    ///
    /// The caller MUST hold [`ENV_LOCK`] for at least as long as the returned
    /// guard lives — that is the invariant the `unsafe` blocks below rely on.
    pub(super) fn apply(key: &'static str, value: Option<&str>) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: every caller holds `ENV_LOCK`, so no other thread reads
        // or writes the environment concurrently.
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: same single-threaded-under-ENV_LOCK invariant as `apply`.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ENV_LOCK, env_lock};

    /// Why: issue #4940 — a single ONNX-model initialisation failure in
    /// `default_model_matches_sentence_transformers_reference` reported as
    /// SEVEN failures, because the panic poisoned [`ENV_LOCK`] and the six
    /// `resolve_*` tests then died on `PoisonError` at their
    /// `ENV_LOCK.lock().unwrap()`. Those six exercise pure resolution logic
    /// and must not be reachable from another test's failure.
    /// What: poisons the real static the way a failing test does — unwind with
    /// the guard live — then asserts [`env_lock`] still hands out a guard.
    /// Test: this test.
    #[test]
    fn poisoned_env_lock_is_still_acquirable() {
        let poisoner = std::thread::Builder::new()
            .name("env-lock-poisoner-4940".into())
            .spawn(|| {
                let _g = env_lock();
                panic!("deliberate panic while holding ENV_LOCK — see #4940");
            })
            .expect("spawning the poisoner thread must succeed");
        assert!(
            poisoner.join().is_err(),
            "the poisoner thread must actually have panicked"
        );
        assert!(
            ENV_LOCK.is_poisoned(),
            "a panic with the guard live must poison ENV_LOCK — otherwise this \
             test proves nothing"
        );

        // #4940: the line that used to be `ENV_LOCK.lock().unwrap()`. It is
        // what turned one failure into seven.
        drop(env_lock());

        assert!(
            ENV_LOCK.lock().is_err(),
            "env_lock() must RECOVER from poison, not silently clear it — the \
             flag stays set so a genuine data-guarding mutex elsewhere is \
             unaffected by this policy"
        );

        // Leave the shared static clean for whatever test runs next.
        ENV_LOCK.clear_poison();
    }
}
