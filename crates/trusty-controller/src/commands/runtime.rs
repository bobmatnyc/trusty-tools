//! Tiny async-runtime bridge for the otherwise-synchronous command dispatch.
//!
//! Why: `tctl`'s dispatcher (`main.rs`) is synchronous, but several Phase-1
//! commands (`install`, `upgrade`, `updates`) call async primitives in
//! `trusty_common::update` (network probes, `cargo install`). Rather than make
//! the whole binary `#[tokio::main]` — which would change every handler's
//! signature and complicate the `up` path's direct `process::exit` — we provide
//! one helper that runs a future to completion on a small runtime, used only by
//! the async handlers.
//!
//! What: [`block_on`] builds a multi-thread tokio runtime (the workspace pins
//! `tokio` with `features = ["full"]`) and drives `fut` to completion.
//!
//! Test: `tests::block_on_runs_future` runs a trivial async block and asserts
//! the result.

use std::future::Future;

/// Run an async future to completion on a fresh tokio runtime.
///
/// Why: The sync dispatcher needs to call async update primitives; one helper
/// keeps the runtime construction in a single audited place.
///
/// What: Builds a multi-threaded tokio runtime and blocks on `fut`, returning
/// its output. Builds with `expect` because a runtime-construction failure is a
/// fatal environment problem (no usable async executor) at a binary entry point,
/// not a recoverable condition — there is nothing sensible to fall back to.
///
/// Test: `tests::block_on_runs_future`.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(fut)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Confirm the bridge actually drives a future to completion.
    /// What: Runs `async { 21 + 21 }` and asserts the result.
    /// Test: This is the test.
    #[test]
    fn block_on_runs_future() {
        let v = block_on(async { 21 + 21 });
        assert_eq!(v, 42);
    }
}
