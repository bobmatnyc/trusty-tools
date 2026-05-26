//! `trusty-embedderd` binary entry point.
//!
//! Why: thin shim so the standalone `trusty-embedderd` binary delegates all
//! startup logic to the library crate (`trusty_embedderd::run`). Keeping the
//! shim minimal means the same logic is shared with the bundled binary in
//! `trusty-search`, preventing any divergence.
//!
//! What: calls `trusty_embedderd::run().await`, which parses CLI args,
//! initialises tracing, loads the ONNX model, and serves the configured
//! transport until shutdown.
//!
//! Test: covered indirectly by `embedder_supervisor_e2e` integration tests in
//! `trusty-search`, which spawn this binary.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    trusty_embedderd::run().await
}
