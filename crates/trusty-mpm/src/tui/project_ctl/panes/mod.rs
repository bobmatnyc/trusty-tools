//! Pane renderers for the `tm projects` TUI (#2118, DOC-35 §5).
//!
//! Why: DOC-35 §5 pins one submodule per pane so each stays small, focused,
//! and independently testable (the pure row/line builders vs. the terminal-
//! touching `render` calls). [`super::layout::render`] composes the frame's
//! `Rect`s and calls into each of these.
//! What: [`projects`] (left, 25%), [`sessions`] (right, 75%),
//! [`activity`] (bottom strip, live `/activity`-endpoint wiring per #2119),
//! and [`actions_bar`] (the 1-row key-hint / notice line).
//! Test: each submodule's pure builders are unit-tested in its own `tests`
//! block; the `render` functions are terminal glue exercised by launching the
//! TUI.

pub mod actions_bar;
pub mod activity;
pub mod projects;
pub mod sessions;
