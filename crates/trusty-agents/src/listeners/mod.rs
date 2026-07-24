//! Eventstream listeners — inbound event API connections (#3820, DOC-54
//! SPEC-AGENTS-04/06).
//!
//! Why: Listeners are the counterpart to MCP tools, not a variant of them.
//! An agent ACTS on the world via MCP tools; an agent REACTS to the world
//! via listeners — direct API connections to upstream event providers
//! (Gmail today; Calendar/Slack are documented, deferred connectors) that
//! surface events onto the harness. A listener is never registered as an
//! MCP tool and never invoked by the model; it runs in the harness runtime
//! and delivers events to bound agents. This module is the first real
//! listener implementation landing on top of DOC-54's spec.
//! What:
//! - `config` — declarative shapes for `config.toml`'s `[[listeners]]`
//!   (stage-one, harness-level) and `agent.toml`'s `[[listeners]]`
//!   (stage-two, per-agent binding).
//! - `store` — the append-only JSONL event log + per-event-type
//!   include/exclude filter state under `~/.trusty-agents/events/`.
//! - `poll` — the Gmail `history.list` polling engine: cursor persistence,
//!   dedup, exponential backoff, 410-GONE re-baseline.
//! - `wake` — stage-two filter matching + the agent-wake dispatch path,
//!   reusing `ctrl::pm_task::run_pm_task_with_persona` (the same entry point
//!   the `/agent` REPL command uses) so a wake reaction is indistinguishable
//!   from an ordinary chat turn in the agent's history.
//! Test: See each submodule's own test coverage. End-to-end (real Gmail
//! account) verification is manual — see the PR body's proof plan.

pub mod config;
pub mod poll;
pub mod store;
pub mod wake;
