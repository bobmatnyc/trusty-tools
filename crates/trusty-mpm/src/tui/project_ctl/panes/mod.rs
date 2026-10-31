//! Pane renderers for the `tm projects` TUI (#2118, DOC-35 §5).
//!
//! Why: DOC-35 §5 pins one submodule per pane so each stays small, focused,
//! and independently testable (the pure row/line builders vs. the terminal-
//! touching `render` calls). [`super::layout::render`] composes the frame's
//! `Rect`s and calls into each of these.
//! What: [`tree`] (#10476: the Owner -> Project -> Sessions tree that
//! replaced the Projects and Sessions panes), [`sessions`] (the session row
//! builders the tree reuses), [`session_details`] (the read-only session
//! overlay Enter opens), [`activity`] (bottom strip, live `/activity`-endpoint wiring per #2119),
//! [`actions_bar`] (the 1-row key-hint / notice line), [`deliverables_view`]
//! (a centred overlay, the read-only Deliverable/Milestone view reachable
//! from the Projects pane, DOC-35 §10.8 `show`, #2383), and [`config_form`]
//! (a centred overlay, the editable deterministic config form reachable from
//! the Projects pane, DOC-35 §6, #2120).
//! Test: each submodule's pure builders are unit-tested in its own `tests`
//! block; the `render` functions are terminal glue exercised by launching the
//! TUI.

pub mod actions_bar;
pub mod activity;
pub mod config_form;
pub mod deliverables_view;
pub mod session_details;
pub mod sessions;
pub mod tree;
