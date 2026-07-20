//! `trusty-embedderd-py` launcher binary.
//!
//! Spawned by the trusty-search `EmbedderSupervisor` exactly like the reference
//! `trusty-embedderd`: `<bin> --stdio` with piped stdin/stdout and inherited
//! stderr. It ensures the venv (fast `.ready` path after the eager bootstrap at
//! `trusty-search start`) then `exec`s the venv's
//! `python -m trusty_embed_sidecar --stdio`, forwarding args + env.
//!
//! On bootstrap failure it logs and exits non-zero so the supervisor's startup
//! probe fails cleanly (trusty-search's `TRUSTY_EMBEDDER=python` arm already
//! fell back to the ort path at `start` on the eager bootstrap; this is the
//! belt-and-suspenders path for a lazy spawn on a broken venv).

use std::process::ExitCode;

fn main() -> ExitCode {
    // Logs to stderr only — stdout is reserved for the sidecar's JSON-RPC
    // frames (the supervisor pipes stdout).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Forward all args after argv[0] (notably `--stdio`) to the sidecar.
    let args: Vec<String> = std::env::args().skip(1).collect();

    let layout = match trusty_embedderd_py::ensure_venv() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("trusty-embedderd-py: venv bootstrap failed: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    // exec replaces this process on success; only returns on failure.
    if let Err(e) = trusty_embedderd_py::exec_sidecar(&layout, &args) {
        tracing::error!("trusty-embedderd-py: failed to exec sidecar: {e:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
