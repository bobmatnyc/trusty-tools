//! Google Chat channel: the API layer for a Chat app reached over Pub/Sub.
//!
//! Why: the trusty bot runs on a Mac with no public URL, so it receives Chat
//! events by pulling a Pub/Sub subscription and posts replies through the Chat
//! REST API as a service account (#9448). This module holds that API layer;
//! the `gchat-mcp` binary, route, egress gate and question ledger land in S2.
//! What: [`api`] — see its module doc for the parts.
//! Test: `cargo test -p trusty-channels` (`tests/gchat_http.rs` and the unit
//! tests under `api`).

pub mod api;
