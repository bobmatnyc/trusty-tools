//! trusty-mpm TUI dashboard shim (`trusty-mpm-tui`).
//!
//! Why: kept as a backward-compatible standalone binary — the primary entry
//! point is now `trusty-mpm tui`, which calls the same [`trusty_mpm::tui::run`].
//! What: parses CLI flags and delegates to the library's `run`.
//! Test: `cargo run -p trusty-mpm-tui` launches the live dashboard.

use clap::Parser;

/// trusty-mpm TUI command-line options.
#[derive(Debug, Parser)]
#[command(name = "trusty-mpm-tui", version, about = "trusty-mpm dashboard")]
struct Args {
    /// Base URL of the trusty-mpm daemon.
    #[arg(long, env = "TRUSTY_MPM_URL", default_value = "http://127.0.0.1:7880")]
    url: String,

    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    interval_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    trusty_mpm::tui::run(args.url, args.interval_ms).await
}
