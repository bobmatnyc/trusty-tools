//! Thin JSON-RPC client the `tcode` CLI binary drives against its own
//! spawned `tcode serve --stdio` child (#2060, vision spec §4.4 "CLI as
//! Thin Client" / Axiom 3 "API-First").
//!
//! Why: prior to this ticket, `tcode run-task` ran the PM->engineer
//! `AgentLoop` IN-PROCESS inside the CLI binary — the only way to observe
//! progress was the final printed report. The vision spec's Axiom 3 requires
//! the CLI to hold NO orchestration logic: every session/task operation goes
//! through the same JSON-RPC surface `tcode serve` already exposes, so a
//! human, a script, and a future TUI all see identical behaviour. This
//! module is the client half of that contract — the daemon (`crate::serve`,
//! `crate::session`, `crate::task`) already defines the server half.
//! What: [`stdio::StdioRpcClient`] spawns an EPHEMERAL `tcode serve --stdio`
//! child per CLI invocation (the "simplest robust M1 approach" the ticket
//! specifies — no persistent daemon discovery/lifecycle needed yet) and
//! speaks JSON-RPC 2.0 NDJSON over its stdio pipes, reusing
//! `trusty_common::mcp::{Request, Response}` for the wire envelope.
//! [`render`] holds pure, unit-tested formatting functions (session table,
//! one live-event line, transcript human view) shared by every CLI
//! subcommand handler in the binary (`src/main.rs`/`src/cli/`), which own
//! only argument parsing, the client lifecycle, and `process::exit` — never
//! business logic.
//! Test: `cli_client::stdio::tests` (line classification, id monotonicity —
//! offline, no subprocess), `cli_client::render::tests` (formatting).
//! End-to-end, real-subprocess coverage lives in `tests/cli_e2e.rs`, which
//! drives the actual `tcode` binary as a black box.

pub mod render;
pub mod stdio;

pub use stdio::{RpcClientError, StdioRpcClient};
