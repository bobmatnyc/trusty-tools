//! Shared test-only synchronization primitives. (#test-hermeticity)
//!
//! Why: Multiple unit tests across different modules (`init::tests`,
//! `mistake_log::tests`, etc.) sandbox `$HOME` with `std::env::set_var` to
//! redirect file I/O into a tempdir. `set_var` is a process-wide mutation
//! and `cargo test` runs unit tests on a multi-threaded executor by default,
//! so two concurrent tests sandboxing HOME stomp on each other and one will
//! observe the other's tempdir (or restore HOME mid-flight). The classic
//! fix is a per-test-module `static Mutex`, but that only serializes tests
//! WITHIN one module — cross-module races (e.g. `init::seed_skills_*` vs
//! `mistake_log::mistake_log_records_nonzero_exit`) still flake.
//! What: Exposes a single process-wide `HOME_LOCK` that every test mutating
//! `$HOME` must hold. Compiled only under `#[cfg(test)]` so it costs nothing
//! in release builds.
//! Test: Used by `mistake_log::tests::mistake_log_records_nonzero_exit` and
//! `init::tests::*`. The mistake_log test was order-dependent before this
//! lock was introduced — passed in isolation, failed under `cargo test`.

#![cfg(test)]

use std::sync::Mutex;

/// Process-wide mutex serializing tests that mutate `$HOME`.
///
/// Why: `std::env::set_var` is a process-global mutation; tokio's default
/// multi-threaded test runtime causes interleaved tests to overwrite each
/// other's HOME before they restore the original value. A single static
/// Mutex shared across all modules keeps such tests sequential without
/// forcing `--test-threads=1` for the whole crate.
/// What: Use `let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());`
/// at the top of any test that calls `std::env::set_var("HOME", ...)`.
/// `unwrap_or_else(into_inner)` ensures a panic in one test doesn't poison
/// the lock for siblings.
pub static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Process-wide mutex serializing tests that mutate LLM credential env vars
/// (#250). Same rationale as `HOME_LOCK` — `std::env::set_var` is global so
/// concurrent credential-routing tests would race.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());
