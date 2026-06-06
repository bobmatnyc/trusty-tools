//! TM (Tmux Manager) — directory-rooted project and session model.
//!
//! Why: Groups related tmux sessions under a project and provides automatic
//! framework/language detection so the orchestrator can present meaningful
//! context for each working directory.
//! What: Re-exports the core data model (TmProject, TmSession, etc.), the
//! framework detection helper, the JSON-backed registry, and the high-level
//! TmManager facade.
//! Test: Imports must compile and the re-exported names must be reachable —
//! covered by `cargo test tm`.

pub mod commands;
pub mod framework;
pub mod manager;
pub mod monitor;
pub mod project;
pub mod project_config;
pub mod registry;

pub use project_config::{
    HarnessConfig, ProjectConfig, ProjectConfigStore, ProjectMeta, default_startup_command_for,
    next_session_name,
};

pub use commands::{handle_tm_command, write_tm_help};
pub use framework::detect_framework;
pub use manager::{ReconcileReport, TmManager};
pub use monitor::TmMonitor;
pub use project::{
    AdapterType, DetectedFramework, ProjectProcessState, SessionStatus, SessionSummary, TmProject,
    TmSession,
};
pub use registry::TmSessionRegistry;
