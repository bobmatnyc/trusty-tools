//! ratatui widgets that render [`crate::app::ReplApp`] state.
//!
//! Why: DOC-50 §3.1 groups the actual `Frame`-drawing functions under
//! `widgets/` (as distinct from [`crate::render`], whose functions take
//! plain strings and never see `ReplApp`) so "reads app state and draws a
//! `Rect`" has one obvious home. Each submodule here is a
//! behavior-preserving, generalized port of one tagent `repl/tui/*.rs` file
//! (see each submodule's doc comment for its specific source).
//!
//! What: [`scrollback`] (chat pane, from tagent's `chat.rs`),
//! [`input_composer`] (the prompt row, also from `chat.rs`), [`banner`]
//! (startup splash, from `banner.rs`), and [`status_line`] (the engine-
//! supplied statusline, new in this slice — tagent's `status.rs` equivalent
//! is NOT ported, since it hardcodes an OpenRouter cost formula the shared
//! crate must never contain; see DOC-50 §3.2 and Q9).
//!
//! # Spec References
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — §3.1 module layout (`widgets/`).
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4).

pub mod banner;
pub mod input_composer;
pub mod scrollback;
pub mod status_line;
