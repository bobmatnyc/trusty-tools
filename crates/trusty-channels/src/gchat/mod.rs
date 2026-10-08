//! Google Chat channel: a Chat app reached over Pub/Sub, with reviewed
//! routes, an egress gate and a question ledger.
//!
//! Why: the trusty bot runs on a Mac with no public URL, so it receives Chat
//! events by pulling a Pub/Sub subscription and posts as a service account
//! (#9448). A session may message only a recipient a reviewed route names,
//! and a reply must resolve exactly the question it answers.
//! What: [`api`] — the HTTP layer (S1). [`routes`] and [`load_gate`] — the
//! per-project `routes.toml` and its git gate. [`channel::GchatChannel`] —
//! the handle S2b's binary holds; [`egress`] is its only send path and
//! [`inbound`] binds replies. [`state`] — learned spaces, the ledger and the
//! audit log.
//! Test: `tests/gchat_http.rs` and the unit tests under `src/gchat/tests/`.

pub mod api;
pub mod channel;
pub mod egress;
pub mod error;
pub mod inbound;
pub mod load_gate;
pub mod routes;
pub mod state;

#[cfg(test)]
mod tests;

pub use channel::{ChannelHealth, GchatChannel, LoadStatus, RouteHealth};
pub use egress::{SentNotice, SentQuestion};
pub use error::{InboundError, RouteError, SendError, StateError};
pub use inbound::{BatchReport, InboundOutcome};
pub use routes::{MessageKind, Route, RouteTable};
