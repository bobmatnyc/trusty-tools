//! # trusty-mpm
//!
//! Why: the original eight sub-crates (`trusty-mpm-{core,client,mcp,daemon,
//! cli,tui,telegram,gui}`) were published separately, which required a complex
//! `[patch.crates-io]` dance for cross-crate development and meant that a
//! simple `cargo install trusty-mpm` did not exist. Consolidating them into
//! one crate with feature-gated `[[bin]]` targets gives users a single install
//! target and eliminates the inter-crate publish coordination overhead.
//!
//! What: re-exports the two library surfaces — `core` (domain types + traits)
//! and `client` (daemon HTTP client + command model) — as top-level modules.
//! The heavier components (`mcp`, `daemon`, `tui`, `telegram`) are declared as
//! conditional modules gated behind their respective features; they are only
//! compiled when the feature is enabled.
//!
//! Test: `cargo test -p trusty-mpm` exercises the core serde round-trips,
//! client URL construction, MCP dispatch, and the daemon's in-process API.
//! Run `cargo test -p trusty-mpm --features daemon,mcp,tui,telegram` for the
//! full suite.

// ── Always-on library modules ────────────────────────────────────────────────

/// Service manifest schema, discovery engine, and status types for `tm services`.
///
/// Why: agents need a canonical, scriptable interface for service discovery that
/// replaces ad-hoc `lsof`/`curl`/`pgrep` patterns hardcoded in prompts. This
/// module provides the stable contract (`ServicesManifest`) and the runtime
/// probe engine (`Discoverer`).
/// What: re-exports `ServicesManifest`, `Discoverer`, `ServiceStatus`,
/// `HealthState`, and the manifest validation / tilde-expansion helpers.
/// Test: `cargo test -p trusty-mpm` exercises the manifest and discoverer unit
/// test suites; the integration smoke test requires a live trusty-search daemon.
pub mod services;

/// Domain types, artifact model, and IPC protocol.
///
/// Why: shared types used by every trusty-mpm component. Centralizing them
/// prevents protocol drift between the daemon and its clients.
/// What: agents, skills, hooks, session state, and the IPC envelope exchanged
/// over the daemon's local socket / HTTP API.
/// Test: serde round-trips and frontmatter parser covered by `cargo test -p trusty-mpm`.
pub mod core;

/// Shared daemon HTTP client, command model, and executor.
///
/// Why: the Telegram bot, the TUI, and the CLI all spoke to the daemon through
/// three independent HTTP layers. This module is the single shared seam.
/// What: [`client::DaemonClient`] is the one HTTP transport, [`client::TrustyCommand`]
/// the one command model, [`client::CommandExecutor`] the one dispatcher.
/// Test: URL construction and the command model are covered by the unit suite.
pub mod client;

// ── Feature-gated modules ────────────────────────────────────────────────────

/// MCP server: six orchestration tools exposed to Claude Code sessions.
///
/// Why: Claude Code sessions need to list sibling sessions, request agent
/// delegations, protect their context window, and inspect circuit-breaker state.
/// MCP is the protocol Claude Code already speaks.
/// What: defines the `OrchestratorBackend` trait, the tool catalog, and
/// the `dispatch` entry point.
/// Test: mock-backend dispatch tests in `mcp` module, enabled by the `mcp` feature.
#[cfg(feature = "mcp")]
pub mod mcp;

/// Daemon library: HTTP API, hook relay, session registry, watcher.
///
/// Why: the daemon's HTTP API and shared state need to be reachable from
/// integration tests (e.g. the Telegram test suite) without a live daemon
/// process.
/// What: re-exports `api::router`, `state::DaemonState`, and the `run_http` /
/// `run_mcp` entry points.
/// Test: in-process e2e suite in `tests/e2e/`; enabled by the `daemon` feature.
#[cfg(feature = "daemon")]
pub mod daemon;

/// ratatui coordinator dashboard.
///
/// Why: operators need one conversational surface with visibility into every
/// active Claude Code session.
/// What: a ratatui app that polls the daemon and renders the coordinator chat
/// and health panels.
/// Test: rendering and client are unit-tested; enabled by the `tui` feature.
#[cfg(feature = "tui")]
pub mod tui;

/// Telegram remote-management bot.
///
/// Why: remote management lets an operator drive the daemon from a phone.
/// What: teloxide adapter that wires `TelegramCommand` to `CommandExecutor`,
/// renders results via `TelegramFormatter`, and runs the push-alert loop.
/// Test: command conversion, formatting, and authorization are unit-tested;
/// enabled by the `telegram` feature.
#[cfg(feature = "telegram")]
pub mod telegram;
