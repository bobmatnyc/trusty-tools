//! Bundled `trusty-embedderd` shim — produced by `cargo install trusty-search`.
//!
//! Why: bundle trusty-embedderd into trusty-search's install surface so
//! `cargo install trusty-search` produces both binaries from a single
//! command. The daemon's actual logic lives in the trusty-embedderd library
//! crate; this shim just wires up the tokio runtime and calls into it.
//! Users coming from pre-Phase-2 (before issue #187) only need to run
//! `cargo install trusty-search --locked --force` — no separate
//! `cargo install trusty-embedderd` required.
//!
//! What: forwards all argument parsing and daemon startup to
//! `trusty_embedderd::run()`, which reads `std::env::args()` via clap,
//! initialises tracing to stderr, loads the ONNX model, and dispatches to
//! the configured transport (--stdio / --http / --socket).
//!
//! Test: covered indirectly by the `embedder_supervisor_e2e` integration
//! tests in `trusty-search/tests/`, which spawn the binary and probe it.
//! The bundled-install acceptance test in
//! `trusty-search/tests/bundled_install.rs` (`#[ignore]`) verifies that
//! `cargo install --path crates/trusty-search` produces both binaries.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    trusty_embedderd::run().await
}
