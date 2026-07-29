//! Full PM-task dispatch bodies — history-aware delegation and persona chat.
//!
//! Why: `run_pm_task_with_history` (tool-armed delegation + conversational
//! fast-path) and `run_pm_task_with_persona` (tools-gated persona chat) are the
//! two largest functions in the ctrl module; each gets its own file to stay
//! under the 500-line cap.
//! What: re-exports `run_pm_task_with_history` (from `history`) and
//! `run_pm_task_with_persona` (from `persona`).
//! Test: Exercised end-to-end via the ctrl integration tests.

mod classification;
mod history;
mod persona;
// #4171 (epic #4167): pure gating helpers split out of `persona.rs`, which
// sits exactly at the 500-SLOC production cap.
mod persona_gate;
mod persona_memory;
// #446 (epic #3052): `[[plugins.python]]` registration, split out of
// `persona.rs` for the same SLOC-cap reason as `persona_gate`.
mod persona_plugins;

pub use history::run_pm_task_with_history;
pub use persona::run_pm_task_with_persona;
