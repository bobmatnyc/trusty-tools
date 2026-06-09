//! Bundled `trusty-console` shim — produced by `cargo install trusty-analyze`.
//!
//! Why: Bundle trusty-console into trusty-analyze's install surface so
//! `cargo install trusty-analyze` produces trusty-console alongside the
//! analysis daemon — one install command, all dependencies present
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
