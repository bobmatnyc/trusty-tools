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

/// Managed session subsystem: records, persistence, lifecycle manager.
///
/// Why: the session-manager MVP tracks every agent session the daemon spawns so
/// operators can inspect, control, and recover sessions across daemon restarts.
/// What: re-exports `SessionManager`, `SessionRecord`, `ManagedSessionId`,
/// `ManagedSessionState`, and the tmux/store seams from its submodules.
/// Test: each submodule carries unit tests; `tests/session_manager_mvp.rs`
/// exercises the integration path.
pub mod session_manager;

/// Runtime adapters that launch an agent runtime inside a tmux session.
///
/// Why: the session manager must swap runtime backends (Claude Code CLI today,
/// trusty-code tomorrow) without changing its own code.
/// What: defines the `RuntimeAdapter` trait and the `ClaudeCodeAdapter`.
/// Test: `runtime::claude_code::tests`.
pub mod runtime;

/// Activity monitoring for managed sessions (content-hash cached classification).
///
/// Why: the dashboard and circuit-breaker logic need a real-time, token-cheap
/// view of what each session is doing.
/// What: re-exports the cache and monitor types.
/// Test: `activity::*::tests`.
pub mod activity;

/// Workspace provisioner: clones a repo into an isolated session workspace.
///
/// Why: agent sessions must never collide with an operator's live checkout; each
/// session gets a clean, isolated workspace under `~/.trusty-mpm/workspaces/`.
/// What: re-exports `WorkspaceProvisioner`, `GitBackend`, `PreparedWorkspace`.
/// Test: `provisioner::workspace::tests`.
pub mod provisioner;

/// Agent/skill catalog synchronization from the claude-mpm repository.
///
/// Why: syncing the agent/skill catalog from the authoritative claude-mpm repo
/// keeps the local catalog current without manual re-porting.
/// What: re-exports `CatalogSync`, `CatalogError`, `CatalogSyncResult`.
/// Test: `content::catalog_sync::tests`.
pub mod content;

/// Driver autonomy subsystem: T1–T4 tier policy + session↔artifact correlation.
///
/// Why: the calling agentic process that drives trusty-mpm must decide, for every
/// `pending_decision` a managed session surfaces, whether to auto-accept the
/// proposed default or escalate to a human. This module provides the structured,
/// non-LLM policy (T1–T4 tiers, hard guardrails) and the session↔artifact
/// correlation that defines a session's scope boundary.
/// What: re-exports `evaluate_autonomy_tier`, `AutonomyTier`, `AutonomyDecision`,
/// `GuardrailSignals`, `SessionCorrelation`, and `ScopeCheck`.
/// Test: pure unit tests in `driver::policy::tests` and `driver::correlation::tests`.
pub mod driver;

/// Unattended supervisor: 24/7 fleet observer + auto-resumer (#1206).
///
/// Why: the session manager normally needs a live calling agentic process to keep
/// a fleet moving. For overnight / unattended operation an always-on supervisor
/// auto-resumes enduring (`stopped`) sessions, observes session health without a
/// caller, surfaces `pending_decision`s for a human, and survives reboots under
/// launchd/systemd. It is a PASSIVE observer — it never makes autonomy decisions.
/// What: re-exports `Supervisor`, `SupervisorConfig`, `FleetMetrics`, and the
/// per-tick `run_tick` sweep; the `/metrics` + `/health` HTTP server is gated
/// behind the `daemon` feature (axum lives there).
/// Test: `supervisor::tests` covers config parsing, metrics derivation, the
/// N-session fleet sweep, and the HTTP handlers.
pub mod supervisor;

/// Project registry: models, on-disk persistence, and lifecycle management (#1519).
///
/// Why: operators and driver skills need to reference repositories by name rather
/// than supplying full URLs on every session spawn. The registry persists known
/// projects to `~/.trusty-mpm/projects.json`, seeds from `config.yaml`'s
/// `projects:` list at startup, and auto-registers projects from session history.
/// What: re-exports `Project`, `ProjectRegistry`, `derive_name_from_url`, and
/// `ProjectStoreError` from the three submodules (record, store, registry).
/// Test: each submodule carries inline unit tests run by `cargo test -p trusty-mpm`.
pub mod project;

/// Shared deterministic project-config edit model (DOC-35 §6, #2120).
///
/// Why: `tm projects config <name> set/unset/tags` (CLI) and the TUI config
/// form (`tui::project_ctl`) are both thin clients over `PATCH
/// /api/v1/projects/{name}`; this module is the ONE place that turns a
/// deterministic field edit into the wire-shape `PatchProjectArgs`, so the two
/// front ends cannot drift on what a given edit means on the wire.
/// What: re-exports `ConfigField`, `ClearableField`, `ConfigEdit`,
/// `build_patch_args`, `merge_patch_args`, and the shared
/// `config_edit_cases`/`assert_matches_case` test-case table.
/// Test: inline unit tests plus the CLI (`bin/tm/tests_projects.rs`) and TUI
/// (`tui::project_ctl::state::modals::tests`) consumers — see the module doc.
pub mod project_config;

/// Deliverable/Milestone data model, central stores, and status state machine
/// (DOC-35 §10, epic #2108; #2378 CRUD API + #2380 transition enforcement).
///
/// Why: the L3 substrate needs a deterministic ledger of what work exists, its
/// tier, and its lifecycle state — bookkeeping `tm manager` (#2109) reasons over
/// but must not own. Central sibling stores to `projects.json` (§10.7, §13 Q5).
/// What: re-exports `Deliverable`/`Milestone` records, the `DeliverableStatus`
/// state machine (§10.3), the on-disk `store`, and the `DeliverableManager`.
/// Test: each submodule carries inline unit tests run by `cargo test -p trusty-mpm`.
pub mod deliverable;

/// SESSCTL alpha-1 session control plane (epic #1590, WI-1).
///
/// Why: the daemon's two parallel session paths (project sessions + managed
/// sessions) are unified here under one actor+registry system with a common
/// session ID convention (`<project-id>-<N>`), a common event model, and two
/// pluggable execution backends (`StreamJsonBackend` default, `TmuxBackend`
/// for `--tmux`).
/// What: re-exports `SessionBackend` trait, `ControlSessionId`, `SessionEvent`,
/// `SessionState`, `SessionRegistry`, `SessionActorHandle`, and `RunParams`.
/// Test: each submodule carries inline unit tests; see `control::*` for details.
pub mod control;

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

/// Slack remote-management bot (DOC-20 adapter, #1294).
///
/// Why: Slack is a peer control surface to the Telegram bot and the sessions TUI
/// (DOC-18 §ONB-3) — an operator drives the managed fleet from Slack through the
/// SAME chat-core nucleus (`CommandExecutor`), with NO duplicated session logic.
/// What: a thin Socket-Mode adapter that parses Slack slash commands / messages
/// into `TrustyCommand`, dispatches via `CommandExecutor`, renders results via
/// `SlackFormatter`, and routes free text to the action-capable coordinator.
/// Test: envelope parsing, command conversion, and result formatting are
/// unit-tested; enabled by the `slack` feature.
#[cfg(feature = "slack")]
pub mod slack;
