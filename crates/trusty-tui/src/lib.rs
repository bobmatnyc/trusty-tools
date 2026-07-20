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
//! and the shared event loop speak. **Slice 1.5** (#3413) added the
//! [`model`] module: the engine-supplied data types ([`StatuslineSegment`],
//! [`PickerItem`]/[`PickerRequest`], [`CommandDescriptor`]) that keep the
//! statusline, pickers, and slash-command help free of any product-specific
//! constant (no hardcoded model lists, no hardcoded `/model`/`/provider`
//! strings, no cost formula). **Slice 2** (#3414) adds the terminal layer
//! this doc comment used to say was missing: the panic-safe
//! [`TerminalGuard`] ([`terminal`]), the generic-over-`TuiEngine` render/event
//! loop ([`run`]), and the `crossterm` → [`KeyInput`] translation boundary
//! ([`keys`]). `ratatui`/`crossterm` are now real dependencies of this crate
//! (0.30/0.29 — see `Cargo.toml`'s comment on why that diverges from the
//! rest of the workspace's 0.29/0.28 pin during the migration window) but
//! stay confined to `terminal`/`run`/`keys`; [`event`] and [`model`]
//! themselves still have zero terminal-library dependency, by design.
//! **Slice 4** (#3416, epic #3411, DOC-50 §5 Slice 4) adds the render/reducer
//! model ([`app`]), the widgets that draw it ([`widgets`]), the pure
//! markdown-rendering helpers those widgets call into ([`render`]), and the
//! top-level frame composition ([`layout`]) — generalized, behavior-
//! preserving ports of tagent's `repl/tui/{chat,banner,markdown,layout}.rs`.
//! **Slice 7** (#3420, epic #3411, DOC-50 §5 Slice 7 / §6 Q4) adds the
//! [`commands`] module: the slash-command parser/dispatcher implementing
//! "mixed routing" — `/help`/`/clear`/`/quit`/`/exit` are client-side
//! built-ins that never reach `TuiEngine::handle_input`; every other
//! command (domain commands like `/model`/`/workstream`, plain chat, and
//! any command this crate doesn't recognize) forwards verbatim, per the
//! thin-client axiom. Also adds the inline-picker half of that routing
//! (DOC-50 §3.2/§6 Q6): a bare `/name` matching `TuiEngine::picker(name)`
//! opens [`ReplApp::active_picker`] instead of forwarding.
//! See DOC-50 §5 for the full slice breakdown (Slice 10 is the tagent
//! cutover this crate is building toward).
//!
//! Dependency direction (DOC-50 §2.2, binding): `trusty-code` and
//! `trusty-agents` depend on `trusty-tui`; `trusty-tui` depends on neither.
//! This crate's public API therefore never references a product-specific
//! type.
//!
//! # Spec References
//! - [`SPEC-TTUI-02~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-02~draft) — architecture, the engine-adapter seam.
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — extraction and migration plan.
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 1, 2, and 4 deliverables and acceptance criteria.

pub mod app;
pub mod commands;
pub mod engine;
pub mod event;
pub mod keys;
pub mod layout;
pub mod model;
pub mod render;
pub mod run;
pub mod terminal;
pub mod text;
pub mod widgets;

pub use app::{ChatLine, ChatRole, ReplApp};
pub use commands::{BuiltIn, Forward, Route, dispatch_forward, resolve_forward, route};
pub use engine::TuiEngine;
pub use event::{KeyCode, KeyInput, KeyModifiers, ReplEvent, WorkstreamSummary};
pub use keys::translate_key_event;
pub use layout::draw;
pub use model::{CommandDescriptor, CommandRouting, PickerItem, PickerRequest, StatuslineSegment};
pub use run::{KeyReaderGuard, TuiModel, event_loop, run, spawn_key_reader};
pub use terminal::TerminalGuard;
