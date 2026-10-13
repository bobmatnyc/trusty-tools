//! Leaf library for the trusty-mpm Architect.
//!
//! Why: the Architect's poller and launcher live in Python scripts today, and
//! `tm fleet init` must call their Rust port. A crate that `tm` calls cannot
//! depend on `trusty-mpm`, so this one is a leaf: std, serde, thiserror,
//! tracing and libc only.
//! What: the [`Tmux`] trait with a real implementation ([`SystemTmux`]) and a
//! recording fake ([`FakeTmux`]), exact pane ids ([`PaneId`]), and the input-box
//! state contract ([`InputState`]) that [`send_guarded`] enforces before it
//! sends keys. Phase A2 adds the pure-logic ports of the live prototype's
//! scripts, held to byte-identical JSON: [`classify`] (`fleet-poll.py`'s inline classifier),
//! [`classify_input`] (`input-state.py`), [`quiet`] (`quiet-sessions.py`) and
//! [`self_ctx`] (`self-ctx.py`), with [`to_python_json`] writing Python's
//! `json.dumps` shape. Phase A3a adds the poller's sensors and state
//! ([`poller`]), a bounded [`SystemTmux`], and the [`SendReading`] /
//! [`Keys`] send contract. Phase A3b-1 adds the wake send path
//! ([`poller::Waker`], [`poller::dispatch_wake`]) on a [`Clock`], with
//! [`ScriptedTmux`] and [`FakeClock`] for tests. Phase A3b-2 adds the poll
//! cycle and loop ([`poller::Runner`]) on the same seams. Phase A4 adds
//! [`verbs`], the `tm architect` commands that replace the scripts (#9822).
//! Test: `manifest_has_no_trusty_mpm_dependency` (the leaf rule);
//! `send_guarded_refuses_typed_and_sends_on_empty` (the send contract);
//! `parse_pane_matches_python_goldens` (parity).
//
// See #9171

pub mod classify;
mod clock;
mod decimal_table;
mod error;
mod fake;
mod input;
mod pointer;
pub mod poller;
mod process;
mod pyjson;
mod pystr;
pub mod quiet;
mod scripted;
pub mod self_ctx;
mod sha1;
mod tmux;
pub mod verbs;

pub use classify::input_state_for_send;
pub use clock::{Clock, FakeClock, SystemClock};
pub use error::{
    ConfigError, CycleError, RunError, SelfCtxError, SendError, StateError, TmuxError, WakeError,
};
pub use fake::FakeTmux;
pub use input::{InputState, SendReading, UnknownInputState, classify_input, send_guarded};
pub use pyjson::to_python_json;
pub use scripted::ScriptedTmux;
pub use tmux::{Keys, NamedKey, PaneId, Permit, SystemTmux, TMUX_TIMEOUT, Tmux};
