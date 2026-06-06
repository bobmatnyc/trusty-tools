//! MCP (Model Context Protocol) registry and prompt injection.
//!
//! Why: trusty-agents itself runs as an LLM-driven agent harness. To let coordinating
//! agents (ctrl, PM, research, observe) discover and call external MCP tools
//! (mcp-vector-search, granola-notes, …) we need a single declarative registry
//! whose contents can be listed in the agent's system prompt so the model knows
//! the tools exist.
//! What: This module exposes `GlobalConfig` (formerly `McpConfig`; renamed in
//! #245 once it grew to host the `[github]` ticketing section alongside the
//! MCP registry), loaded from `~/.trusty-agents/config.toml` (created with sane
//! defaults on first use). Project-memory recall is no longer routed through
//! an external MCP binary — `ctrl` queries the in-process redb+usearch store
//! directly (#275).
//! Test: `mcp::config::tests::*` cover load/create/render.

pub mod config;

pub use config::GlobalConfig;
#[allow(unused_imports)]
pub use config::{LocalInferenceConfig, McpSection, McpService, McpTool};
