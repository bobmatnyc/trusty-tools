//! trusty-console binary entry point.
//!
//! Why: The standalone `trusty-console` binary delegates entirely to the
//! library crate's `run()` function so all daemon logic stays in the library
//! and bundled shim binaries in host crates (trusty-search, trusty-memory,
//! trusty-analyze, trusty-review, trusty-mpm) can reuse the same code path
//! without duplication. Mirrors the pattern of `trusty-embedderd` (issue #187)
//! and `trusty-bm25-daemon` (PR #190).
//! What: Wires the tokio async runtime and forwards execution to
//! `trusty_console::run()`, which handles argument parsing, tracing init,
//! and the full serve lifecycle.
//! Test: Smoke-tested via `cargo run -p trusty-console -- serve --help`.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    trusty_console::run().await
}
