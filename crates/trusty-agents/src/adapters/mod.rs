//! Harness adapter framework.
//!
//! Why: Phase 1 of the TM (Tmux Manager) feature — provides the trait and
//! pattern infrastructure that concrete adapters (Phase 2) plug into.
//! What: Re-exports `Pattern` helpers, the `HarnessAdapter` trait + value
//! types, and the `AdapterRegistry`.
//! Test: `cargo test -- adapters` runs the unit tests in each submodule.

pub mod patterns;
pub mod registry;
pub mod traits;

// Phase 2 adapters (issue #312):
pub mod augment;
pub mod claude_code;
pub mod claude_mpm;
pub mod codex;
pub mod gemini;
pub mod shell;
pub mod trusty_agents_adapter;

pub use patterns::{Pattern, any_match, best_match, last_n_lines};
pub use registry::AdapterRegistry;
pub use traits::{AdapterInfo, DetectionResult, HarnessAdapter, HarnessObservation, HarnessState};

pub use augment::AugmentAdapter;
pub use claude_code::ClaudeCodeAdapter;
pub use claude_mpm::ClaudeMpmAdapter;
pub use codex::CodexAdapter;
pub use gemini::GeminiAdapter;
pub use shell::ShellAdapter;
pub use trusty_agents_adapter::TrustyAgentsAdapter;
