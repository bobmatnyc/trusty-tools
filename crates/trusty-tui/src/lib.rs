//! `trusty-tui` — the engine-agnostic terminal-UI seam shared by
//! trusty-code's `tcode tui` and trusty-agents' tagent REPL.
//!
//! Why: today, tagent owns a mature ratatui-based REPL
//! (`crates/trusty-agents/src/repl/tui/`) and trusty-code has no interactive
//! TUI at all. Forking tagent's REPL for trusty-code would duplicate ~2,500
//! lines of event-loop, widget, and line-editing code and let the two drift.
//! DOC-50 §2.2 resolves this by extracting the engine-agnostic parts into
//! this crate; each product supplies a thin [`TuiEngine`] adapter instead.
//!
//! What: **Slice 1** (#3412, epic #3411) ships only the seam itself — the
//! [`TuiEngine`] trait and the [`ReplEvent`] enum both products' adapters
//! and the eventual shared event loop will speak. No terminal library
//! (ratatui/crossterm), no rendering, no event loop yet — see DOC-50 §5 for
//! the full slice breakdown (Slice 2 adds the terminal layer, Slice 4 the
//! widgets, Slice 10 the tagent cutover).
//!
//! Dependency direction (DOC-50 §2.2, binding): `trusty-code` and
//! `trusty-agents` depend on `trusty-tui`; `trusty-tui` depends on neither.
//! This crate's public API therefore never references a product-specific
//! type.
//!
//! # Spec References
//! - [`SPEC-TTUI-02~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-02~draft) — architecture, the engine-adapter seam.
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — extraction and migration plan.
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 1 deliverable and acceptance criteria.

pub mod engine;
pub mod event;

pub use engine::TuiEngine;
pub use event::{
    CommandDescriptor, KeyCode, KeyInput, KeyModifiers, PickerItem, ReplEvent, StatuslineSegment,
    WorkstreamSummary,
};
