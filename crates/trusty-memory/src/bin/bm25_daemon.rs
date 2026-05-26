//! Bundled `trusty-bm25-daemon` shim — produced by `cargo install trusty-memory`.
//!
//! Why: bundle trusty-bm25-daemon into trusty-memory's install surface so
//! `cargo install trusty-memory` produces all three binaries from a single
//! command: trusty-memory, trusty-memory-mcp-bridge, AND trusty-bm25-daemon.
//! Users who set TRUSTY_BM25_DAEMON=1 without separately installing the daemon
//! previously got silently degraded lexical recall — this bundling closes that
//! footgun. Mirrors PR #190 which did the same for trusty-embedderd in
//! trusty-search.
//!
//! What: forwards argument parsing and daemon startup to
//! `trusty_bm25_daemon::run()`, which reads `std::env::args()` via clap,
//! initialises tracing to stderr, loads the BM25 palace snapshot, and runs
//! the accept loop until SIGTERM / SIGINT.
//!
//! Test: `cargo install --path crates/trusty-memory --root /tmp/test-bundled`
//! followed by `ls /tmp/test-bundled/bin/` should list trusty-bm25-daemon.
//! The binary's behaviour is covered by
//! `crates/trusty-bm25-daemon/tests/bm25_daemon.rs`.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    trusty_bm25_daemon::run().await
}
