//! Tiny async-runtime bridge for the otherwise-synchronous command dispatch.
//!
//! Why: `tctl`'s dispatcher (`main.rs`) is synchronous, but several Phase-1
//! commands (`install`, `upgrade`, `updates`) call async primitives in
//! `trusty_common::update` (network probes, `cargo install`). Rather than make
//! the whole binary `#[tokio::main]` — which would change every handler's
//! signature and complicate the `up` path's direct `process::exit` — we provide
//! helpers that build ONE runtime per command and drive futures on it.
//!
//! What:
//! - [`with_runtime`] builds a single multi-thread tokio runtime, hands the
//!   caller its [`Handle`], and tears it down when the closure returns. A
//!   command that needs to await several futures (e.g. `upgrade` gathers
//!   candidates *then* applies them) builds the runtime exactly once and reuses
//!   the handle, avoiding both the per-call construction cost and the
//!   nested-runtime panic risk of constructing a second runtime while the first
//!   is still live.
//! - [`block_on`] is the single-future convenience wrapper over [`with_runtime`]
//!   for commands that await exactly one future (`install`, `updates`,
//!   `auto_update`).
//!
//! Test: `tests::block_on_runs_future` and `tests::with_runtime_reuses_handle`.

use std::future::Future;

use tokio::runtime::{Builder, Handle, Runtime};

/// Build the one multi-thread tokio runtime used for a command's async work.
///
/// Why: Centralising construction in a single audited place keeps the `expect`
/// rationale in one spot and lets every caller reuse the same runtime config.
///
/// What: Builds a multi-threaded tokio runtime (the workspace pins `tokio` with
/// `features = ["full"]`). Uses `expect` because a runtime-construction failure
/// is a fatal environment problem (no usable async executor) at a binary entry
/// point, not a recoverable condition — there is nothing sensible to fall back
/// to.
///
/// Test: Exercised via [`with_runtime`] / [`block_on`] in `tests`.
fn build_runtime() -> Runtime {
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
}

/// Build one runtime, run `f` with its [`Handle`], then drop the runtime.
///
/// Why: A command that awaits more than one future (e.g. `upgrade` gathers
/// candidates and *then* applies them) must build the runtime ONCE and reuse it
/// for every `handle.block_on(...)` call — constructing a fresh runtime per
/// future is wasteful and risks a nested-runtime panic. This helper makes the
/// single-runtime lifetime explicit and scoped.
///
/// What: Builds the runtime via [`build_runtime`], invokes `f` with a borrowed
/// [`Handle`], and returns `f`'s result. The runtime is dropped when this
/// function returns.
///
/// Test: `tests::with_runtime_reuses_handle`.
pub fn with_runtime<T>(f: impl FnOnce(&Handle) -> T) -> T {
    let rt = build_runtime();
    f(rt.handle())
}

/// Run a single async future to completion on a freshly-built tokio runtime.
///
/// Why: Commands that await exactly one future want a one-liner; this keeps the
/// runtime construction in [`with_runtime`] so there is still a single place
/// runtimes are built.
///
/// What: Builds one runtime via [`with_runtime`] and blocks on `fut`, returning
/// its output.
///
/// Test: `tests::block_on_runs_future`.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    with_runtime(|handle| handle.block_on(fut))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Confirm the single-future bridge actually drives a future to
    /// completion.
    /// What: Runs `async { 21 + 21 }` and asserts the result.
    /// Test: This is the test.
    #[test]
    fn block_on_runs_future() {
        let v = block_on(async { 21 + 21 });
        assert_eq!(v, 42);
    }

    /// Why: The whole point of `with_runtime` is that ONE runtime drives several
    /// futures; prove the same handle services two sequential `block_on`s
    /// without panicking (a second runtime built while the first is live would
    /// panic on a nested `block_on`).
    /// What: Builds one runtime and awaits two futures on its handle, asserting
    /// both results.
    /// Test: This is the test.
    #[test]
    fn with_runtime_reuses_handle() {
        let (a, b) = with_runtime(|handle| {
            let a = handle.block_on(async { 1 + 1 });
            let b = handle.block_on(async { 2 + 2 });
            (a, b)
        });
        assert_eq!((a, b), (2, 4));
    }
}
