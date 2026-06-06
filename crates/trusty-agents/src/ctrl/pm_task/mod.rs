//! PM task dispatch — the canonical `run_pm_task_*` entry points and their
//! conversational helpers.
//!
//! Why: The PM-side dispatch path (history-aware, persona-aware, session-aware)
//! is the largest single concern in the ctrl module. Splitting it into focused
//! files keeps each under the 500-line cap while preserving the entry-point API.
//! What: `helpers` holds the thin entry points + pure conversational helpers
//! (`extract_name_from_input`, `match_any_glob`); `dispatch` holds the two large
//! `run_pm_task_with_history` / `run_pm_task_with_persona` bodies.
//! Test: Unit tests for `extract_name_from_input` and `match_any_glob` live in
//! `ctrl::tests::pm_task_tests`; the dispatch functions are exercised
//! end-to-end via the ctrl integration tests.

mod dispatch;
mod helpers;

// Internal re-export — preserve `super::pm_task::run_pm_task` used by the PM
// actor loop in state.rs.
pub(crate) use helpers::run_pm_task;

// Test-only re-exports — `ctrl::tests::pm_task_tests` reaches these pure helpers
// via `super::super::pm_task::<item>`; they are otherwise internal to `helpers`.
#[cfg(test)]
pub(crate) use helpers::{extract_name_from_input, match_any_glob};

// Public surface — the `run_pm_task_*` API consumed by ctrl/mod.rs re-exports.
pub use dispatch::{run_pm_task_with_history, run_pm_task_with_persona};
pub use helpers::run_pm_task_with_session;
