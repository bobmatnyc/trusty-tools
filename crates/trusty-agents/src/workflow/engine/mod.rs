//! Workflow engine facade.
//!
//! Why: The engine was a single 4,900-line file (#172). Splitting it into
//! focused sub-modules keeps each concern reviewable while preserving the
//! public surface (`workflow::WorkflowEngine`, `workflow::engine::WorkflowEngine`).
//! What: Declares the engine sub-modules and re-exports the public types plus
//! the `pub(crate)` items consumed by other parts of the crate.
//! Test: Behavior is covered by each sub-module's own `#[cfg(test)]` block and
//! the integration tests in `executor`.

mod executor;
mod helpers;
mod qa;
mod retry;
mod skills;
mod state;
mod step_dispatch;

pub use executor::{DiscoveredSkill, WorkflowEngine};
