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
//!   * [`bootstrap::ensure_venv`] — eager venv bootstrap at `start`.
//!   * [`launcher::locate_launcher_binary`] — sibling/PATH/env discovery.

pub mod bootstrap;
pub mod launcher;

pub use bootstrap::{ensure_venv, resolve_layout, VenvLayout};
pub use launcher::{exec_sidecar, locate_launcher_binary};
