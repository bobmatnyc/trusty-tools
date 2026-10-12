//! Leaf library for the trusty-mpm Architect.
//!
//! Why: the Architect's poller and launcher live in Python scripts today, and
//! `tm fleet init` must call their Rust port. A crate that `tm` calls cannot
//! depend on `trusty-mpm`, so this one is a leaf: std, serde, thiserror and
//! tracing only.
//! What: the [`Tmux`] trait with a real implementation ([`SystemTmux`]) and a
//! recording fake ([`FakeTmux`]), exact pane ids ([`PaneId`]), and the input-box
//! state contract ([`InputState`]) that [`send_guarded`] enforces before it
//! sends keys. Phase A2 adds the pure-logic ports of the live prototype's
//! scripts, held to byte-identical JSON: [`classify`] (`fleet-poll.py`'s inline classifier),
//! [`classify_input`] (`input-state.py`), [`quiet`] (`quiet-sessions.py`) and
//! [`self_ctx`] (`self-ctx.py`), with [`to_python_json`] writing Python's
//! `json.dumps` shape.
//! Test: `manifest_has_no_trusty_mpm_dependency` (the leaf rule);
//! `send_guarded_refuses_typed_and_sends_on_empty` (the send contract);
//! `parse_pane_matches_python_goldens` (parity).
//
// See #9171

pub mod classify;
mod error;
mod fake;
mod input;
mod pyjson;
mod pystr;
pub mod quiet;
pub mod self_ctx;
mod sha1;
mod tmux;

pub use classify::input_state_for_send;
pub use error::{SelfCtxError, SendError, TmuxError};
pub use fake::FakeTmux;
pub use input::{InputState, UnknownInputState, classify_input, send_guarded};
pub use pyjson::to_python_json;
pub use tmux::{PaneId, SystemTmux, Tmux};
