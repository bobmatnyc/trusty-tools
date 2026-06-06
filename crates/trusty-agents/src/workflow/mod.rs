//! Workflow engine: declarative multi-phase agent orchestration.
//!
//! Why: Some tasks benefit from a fixed pipeline (research -> plan -> code ->
//! QA -> observe) rather than dynamic PM delegation. This module provides
//! that pipeline as configurable JSON workflows.
//! What: Re-exports `WorkflowEngine`, `WorkflowContext`, `WorkflowDef`,
//! `WorkflowError`.
//! Test: See per-submodule unit tests.

pub mod autopush;
pub mod config;
pub mod context;
pub mod engine;
pub mod error;
pub mod parallel;
pub mod resolver;
pub mod tickets;
pub mod worktree;

#[allow(unused_imports)]
pub use config::{
    Assignments, AutoPushConfig, FileAssignment, ParallelSubtask, PhaseDef, TicketManagementConfig,
    WaveDef, WorkflowDef,
};
#[allow(unused_imports)]
pub use context::{WorkflowContext, WorkflowContextBuilder};
pub use engine::WorkflowEngine;
#[allow(unused_imports)]
pub use error::WorkflowError;
#[allow(unused_imports)]
pub use tickets::TicketManager;
