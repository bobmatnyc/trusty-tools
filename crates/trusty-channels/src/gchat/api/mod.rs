//! Google Chat API layer: service-account tokens, Chat message create, and
//! Pub/Sub pull/acknowledge.
//!
//! Why: keep HTTP and credential concerns apart from the route, egress gate
//! and question ledger that S2 of #9448 builds on top, matching the
//! `slack::api` / `telegram::api` split.
//! What: [`auth`] (key file + token cache), [`client`] (the three calls),
//! [`events`] (typed Chat events from Pub/Sub), [`constants`], [`error`].
//! Test: unit tests in each module plus `tests/gchat_http.rs`.

pub mod auth;
pub mod client;
pub mod constants;
pub mod error;
pub mod events;
