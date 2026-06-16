//! `tctl stack health` and `tctl stack doctor` — stack-wide rollup commands.
//!
//! Why: These are the primary observability entry points for the whole stack
//! (DOC-4). `stack health` fans a liveness probe across every member and renders
//! a verdict; `stack doctor` adds per-member diagnostic checks (binary on PATH,
//! plist installed, port reachable, installed version). Splitting them into
//! sibling submodules keeps each file focused and under the 500-SLOC cap; this
//! `mod.rs` is a thin facade re-exporting the two entry points.
//!
//! What: re-exports [`health::run_health`] and [`doctor::run_doctor`]. The shared
//! probe primitives live in `super::probe`; the launchd label/plist resolution in
//! `super::plist_label`.
//!
//! Test: each submodule carries its own tests; this facade has none.

pub mod doctor;
pub mod health;

pub use doctor::run_doctor;
pub use health::run_health;
