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
// #3766: pure local-failure recovery policy split out of `history.rs`,
// which sits against the 500-SLOC production cap.
// #4788: `pub(crate)` is REQUIRED, not redundant with the `pm_task/mod.rs`
// re-export that carries it to the ctrl_turn REPL route — `pub(crate) use` of
// a `pub(super)` module is E0365 ("private, and cannot be re-exported").
pub(crate) mod local_fallback;
mod persona;
// #4171 (epic #4167): pure gating helpers split out of `persona.rs`, which
// sits exactly at the 500-SLOC production cap.
mod persona_gate;
// #4278: `api::server::chat_history` reads back what `spawn_persist_turn`
// writes, so `session_id_for` must have exactly one implementation both sides
// share — a second `format!("persona-{agent}")` would silently diverge.
pub(crate) mod persona_memory;
// #446 (epic #3052): `[[plugins.python]]` registration, split out of
// `persona.rs` for the same SLOC-cap reason as `persona_gate`.
mod persona_plugins;

pub use history::run_pm_task_with_history;
pub use persona::run_pm_task_with_persona;
