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
//! - `wake` — stage-two filter matching and the wake prompt.
//!
//! #11632: the Gmail polling engine (`poll`) and the wake dispatcher were cut
//! with the agent loop that answered a wake; what is left is the config, the
//! event store and the filters.
//! Test: See each submodule's own test coverage.

pub mod config;
pub mod store;
pub mod wake;
