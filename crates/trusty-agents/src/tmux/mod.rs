//! Tmux session and pane orchestration.
//!
//! Why: Phase 1 of the TM (Tmux Manager) feature — provides a typed wrapper
//! around the `tmux` CLI so subsequent phases can spawn, observe, and steer
//! tmux sessions hosting AI agent harnesses.
//! What: Re-exports `TmuxOrchestrator`, `TmuxSession`, `TmuxPane`, and the
//! `TmuxError` / `Result` aliases from this module's submodules.
//! Test: `cargo test -- tmux` runs the unit tests in each submodule.

pub mod error;
pub mod orchestrator;
pub mod session;

pub use error::{Result, TmuxError};
pub use orchestrator::TmuxOrchestrator;
pub use session::{TmuxPane, TmuxSession};
