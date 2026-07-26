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
