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
//! What: **Slice 1** (#3412, epic #3411) shipped the seam itself — the
//! [`TuiEngine`] trait and the [`ReplEvent`] enum both products' adapters
//! and the shared event loop speak. **Slice 2** (#3414) adds the terminal
//! layer this doc comment used to say was missing: the panic-safe
//! [`TerminalGuard`] ([`terminal`]), the generic-over-`TuiEngine` render/event
//! loop ([`run`]), and the `crossterm` → [`KeyInput`] translation boundary
//! ([`keys`]). `ratatui`/`crossterm` are now real dependencies of this crate
//! (0.30/0.29 — see `Cargo.toml`'s comment on why that diverges from the
//! rest of the workspace's 0.29/0.28 pin during the migration window) but
//! stay confined to `terminal`/`run`/`keys`; [`event`] itself still has zero
//! terminal-library dependency, by design. See DOC-50 §5 for the full slice
//! breakdown (Slice 4 adds the widgets, Slice 10 the tagent cutover).
//!
//! Dependency direction (DOC-50 §2.2, binding): `trusty-code` and
//! `trusty-agents` depend on `trusty-tui`; `trusty-tui` depends on neither.
//! This crate's public API therefore never references a product-specific
//! type.
//!
//! # Spec References
//! - [`SPEC-TTUI-02~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-02~draft) — architecture, the engine-adapter seam.
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — extraction and migration plan.
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 1 and Slice 2 deliverables and acceptance criteria.

pub mod engine;
pub mod event;
pub mod keys;
pub mod run;
pub mod terminal;

pub use engine::TuiEngine;
pub use event::{
    CommandDescriptor, KeyCode, KeyInput, KeyModifiers, PickerItem, ReplEvent, StatuslineSegment,
    WorkstreamSummary,
};
pub use keys::translate_key_event;
pub use run::{TuiModel, event_loop, run, spawn_key_reader};
pub use terminal::TerminalGuard;
