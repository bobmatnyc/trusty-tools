//! Standalone `trusty-bm25-daemon` binary entry point.
//!
//! Why: the thin shim keeps this binary's entry point minimal — all daemon
//! logic lives in `lib.rs` so the bundled shim in
//! `trusty-memory/src/bin/bm25_daemon.rs` can delegate to the same path
//! without duplicating code.
//! What: wires a tokio runtime and calls `trusty_bm25_daemon::run()`.
//! Test: end-to-end coverage in `tests/bm25_daemon.rs`.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    trusty_bm25_daemon::run().await
}
