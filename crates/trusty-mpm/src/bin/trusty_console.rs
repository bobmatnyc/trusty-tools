//! Bundled `trusty-console` shim — produced by `cargo install trusty-mpm`.
//!
//! Why: Bundle trusty-console into trusty-mpm's install surface so
//! `cargo install trusty-mpm` produces trusty-console alongside the
//! MPM platform binaries — one install command, all dependencies present
//! (Single-Install Convention, epic #943). The dashboard's actual logic lives
//! in the trusty-console library crate; this shim just wires up the tokio
//! runtime and delegates.
//! What: Forwards all argument parsing and daemon startup to
//! `trusty_console::run()`, which initialises tracing to stderr, parses argv
//! via clap, and runs the axum HTTP dashboard until SIGTERM/SIGINT.
//! Test: Covered by the trusty-console crate's own unit tests.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    trusty_console::run().await
}
