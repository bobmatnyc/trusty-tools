//! Pure rendering helpers that turn text into styled ratatui primitives,
//! independent of [`crate::app::ReplApp`] state.
//!
//! Why: DOC-50 §3.1 puts `render/markdown.rs` alongside the widgets as its
//! own module specifically because these functions (fence detection, table
//! layout, cell truncation) take plain strings in and `Line`/`Span` out —
//! they never touch `ReplApp`, so keeping them out of `widgets` (which does
//! read app state) makes the "this has zero app-state dependency" property
//! visible from the module boundary, not just from reading each function.
//!
//! # Spec References
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — §3.1 module layout (`render/markdown.rs`).

pub mod markdown;
