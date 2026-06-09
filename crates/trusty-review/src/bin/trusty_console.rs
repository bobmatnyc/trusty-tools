//! Bundled `trusty-console` shim — produced by `cargo install trusty-review`.
//!
//! Why: Bundle trusty-console into trusty-review's install surface so
//! `cargo install trusty-review` produces trusty-console alongside the
//! review daemon — one install command, all dependencies present
//! (Single-Install Convention, epic #943). The dashboard's actual logic lives
//! in the trusty-console library crate; this shim just wires up the tokio
//! runtime and delegates.
//! What: Forwards all argument parsing and daemon startup to
//! `trusty_console::run()`, which initialises tracing to stderr, parses argv
//! via clap, and runs the axum HTTP dashboard until SIGTERM/SIGINT.
//! Test: Covered by the trusty-console crate's own unit tests. The bundled-
//! install convention is validated via `cargo install --path crates/trusty-review
//! --root /tmp/test` + `ls /tmp/test/bin/` (both trusty-review AND
//! trusty-console must appear).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    trusty_console::run().await
}
