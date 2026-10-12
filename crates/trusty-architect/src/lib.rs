//! Leaf library for the trusty-mpm Architect.
//!
//! Why: the Architect's poller and launcher live in Python scripts today, and
//! `tm fleet init` must call their Rust port. A crate that `tm` calls cannot
//! depend on `trusty-mpm`, so this one is a leaf: std, thiserror and
//! tracing only.
//! What: the [`Tmux`] trait with a real implementation ([`SystemTmux`]) and a
//! recording fake ([`FakeTmux`]), exact pane ids ([`PaneId`]), and the input-box
//! state contract ([`InputState`]) that [`send_guarded`] enforces before it
//! sends keys.
//! Test: `manifest_has_no_trusty_mpm_dependency` (the leaf rule);
//! `send_guarded_refuses_typed_and_sends_on_empty` (the send contract).
//
// See #9171

mod error;
mod fake;
mod input;
mod tmux;

pub use error::{SendError, TmuxError};
pub use fake::FakeTmux;
pub use input::{InputState, UnknownInputState, send_guarded};
pub use tmux::{PaneId, SystemTmux, Tmux};
