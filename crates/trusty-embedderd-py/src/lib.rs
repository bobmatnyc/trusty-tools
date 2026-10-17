//! `trusty-embedderd-py` — opt-in Python/MPS embedding sidecar launcher for
//! trusty-search (epic #3524, slices 2-4).
//!
//! Why: on Apple Silicon a torch/MPS `sentence-transformers` sidecar embeds
//! ~2.4x faster than the Rust ort path with numerically identical results (the
//! spike measured 561 emb/s end-to-end through the real supervisor, vs the 457
//! target). This crate is the launcher that bootstraps a pinned Python venv
//! (from a committed, hashed `uv.lock`) and execs the sidecar. It speaks the
//! EXACT stdio JSON-RPC 2.0 wire protocol of `trusty-embedderd`, so the
//! trusty-search `EmbedderSupervisor` / `StdioEmbedderClient` drive it with
//! ZERO changes to the supervisor/stdio/protocol wire code.
//!
//! Default-OFF: nothing here runs unless `TRUSTY_EMBEDDER=python` selects it in
//! `trusty-search start`. The Rust build does NOT require torch/venv — that is
//! all runtime.
//!
//! Public surface consumed by trusty-search:
//!   * [`bootstrap::ensure_venv_eager`] — eager, once-per-daemon-start venv
//!     bootstrap at `start` (full torch-importing `.ready` recheck).
//!   * [`bootstrap::ensure_venv`] — per-respawn venv bootstrap, called by the
//!     launcher binary via [`run`] (cheap, torch-free `.ready` recheck).
//!   * [`launcher::locate_launcher_binary`] — env/sibling/PATH discovery.
//!   * [`venv_error::failure_kind`] — whether a failed bootstrap is worth
//!     retrying (#9971).
//!   * [`run`] — the whole launcher; trusty-search's `trusty-embedderd-py`
//!     binary is a one-line shim over it. This crate is library-only (#9468).

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

pub mod bootstrap;
pub mod launcher;
mod spawn_retry;
// #9468, #9966: locate uv, or fetch and cache a pinned, SHA-256-verified one (D1a).
pub mod uv_fetch;
// #9971: typed venv-build failures and their transient/permanent kind.
pub mod venv_error;

pub use bootstrap::{ensure_venv, ensure_venv_eager, resolve_layout, VenvLayout};
pub use launcher::{exec_sidecar, locate_launcher_binary};
pub use venv_error::{failure_kind, FailureKind, VenvBuildError};

use std::process::ExitCode;

/// Run the `trusty-embedderd-py` launcher: ensure the venv, then `exec` the
/// sidecar.
///
/// Why (#9468): the launcher binary moved to trusty-search so
/// `cargo install trusty-search` installs it next to the daemon that spawns
/// it; the logic stays here so the shim is one line.
/// What: logs to stderr only (stdout carries the sidecar's JSON-RPC frames),
/// forwards every argument after argv\[0\] (notably `--stdio`) to
/// `python -m trusty_embed_sidecar`, and returns `ExitCode::FAILURE` when the
/// venv bootstrap or the exec fails. A successful exec never returns.
/// Test: the `#[ignore]`d e2e `supervisor_spawns_launcher_and_embeds_batch`
/// runs it; a trusty-search integration test pins the packaging.
pub fn run() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    let layout = match ensure_venv() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("trusty-embedderd-py: venv bootstrap failed: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    // exec replaces this process on success; only returns on failure.
    if let Err(e) = exec_sidecar(&layout, &args) {
        tracing::error!("trusty-embedderd-py: failed to exec sidecar: {e:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
