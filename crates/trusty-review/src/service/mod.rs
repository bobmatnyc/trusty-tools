//! Shared state and the review operations, over plain types.
//!
//! Why: the review pipeline needs its dependency set — config, LLM, verifier,
//! search, analyze, dedup — built once and shared by every caller that runs
//! more than one review in a process. [`AppState`] is that set.
//!
//! #6290 (ADR-0032, review lane): this module used to wrap those operations in
//! a long-lived daemon. It no longer does. There is no listener, no socket, no
//! launchd unit and no `serve` subcommand — review runs per invocation, and
//! `trusty-review run` is the entry point that covers what `review.run` served.
//! What survives is the part that was never transport: `handle_review`,
//! `handle_health` and `handle_status` as plain async functions, which the MCP
//! stdio tools call directly.
//!
//! The module keeps its name and its `http-server` feature gate. Both are
//! misnomers now — it serves nothing and has not pulled axum since #6277 — and
//! renaming either is a breaking manifest change for every library consumer, in
//! exchange for no behaviour. The rename waits for a change that is already
//! breaking them.
//!
//! What: exports [`AppState`] and the three operations from [`handlers`], plus
//! the inference probe they share.
//!
//! GitHub webhooks do NOT arrive here. #5181 retired `POST /pr/github/webhook`;
//! `trusty-console` terminates the GitHub request and relays it over a separate
//! UDS socket to `crate::webhook_listener`, a process console spawns on demand
//! and SIGTERMs (ADR-0034).
//!
//! Test: `handlers_tests.rs` and `handlers_status_tests.rs` call each operation
//! directly.

pub mod handlers;
pub mod inference_probe;

pub use handlers::AppState;
