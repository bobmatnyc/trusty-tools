//! Bundled `trusty-bm25-daemon` shim — produced by `cargo install trusty-memory`.
//!
//! Why: bundle trusty-bm25-daemon into trusty-memory's install surface so
//! `cargo install trusty-memory` produces both binaries from a single command:
//! trusty-memory AND trusty-bm25-daemon. (The former trusty-memory-mcp-bridge
//! binary was removed in PR3 of the #914 stdio-cutover epic.) Users who set
//! TRUSTY_BM25_DAEMON=1 without separately installing the daemon previously got
//! silently degraded lexical recall — this bundling closes that footgun. Mirrors
//! PR #190 which did the same for trusty-embedderd in trusty-search.
//!
//! What: parses CLI flags via the `trusty_bm25_daemon::Cli` re-export,
//! initialises tracing to stderr at the requested verbosity, projects the
//! flags onto `DaemonConfig`, and delegates to `trusty_bm25_daemon::run`.
//! Sharing the `Cli` parser with the upstream crate guarantees the bundled
//! binary's flag surface stays in lock-step with the standalone binary.
//!
//! Test: `cargo install --path crates/trusty-memory --root /tmp/test-bundled`
//! followed by `ls /tmp/test-bundled/bin/` should list trusty-bm25-daemon.
//! The binary's behaviour is covered by
//! `crates/trusty-bm25-daemon/tests/bm25_daemon.rs`.

use clap::Parser;

use trusty_bm25_daemon::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    trusty_common::init_tracing(cli.verbose);
    let config = cli.into_config();
    trusty_bm25_daemon::run(config).await
}
