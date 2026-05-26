//! Standalone `trusty-bm25-daemon` binary entry point.
//!
//! Why: the thin shim keeps this binary's entry point minimal — all daemon
//! logic lives in `lib.rs` so the bundled shim in
//! `trusty-memory/src/bin/bm25_daemon.rs` can delegate to the same path
//! without duplicating code.
//! What: parses CLI flags via clap, initialises tracing to stderr at the
//! requested verbosity, projects the flags onto `DaemonConfig`, and hands
//! off to `trusty_bm25_daemon::run(config)`.
//! Test: end-to-end coverage in `tests/bm25_daemon.rs`.

use clap::Parser;

use trusty_bm25_daemon::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    trusty_common::init_tracing(cli.verbose);
    let config = cli.into_config();
    trusty_bm25_daemon::run(config).await
}
