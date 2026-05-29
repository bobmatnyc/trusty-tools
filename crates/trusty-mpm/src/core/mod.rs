//! # trusty-mpm-core
//!
//! Why: Shared types used by every trusty-mpm crate (daemon, CLI, TUI, Telegram).
//! Centralizing them prevents protocol drift between the daemon and its clients.
//!
//! What: Defines the artifact model (agents, skills, hooks), session state types,
//! and the IPC protocol envelope exchanged over the daemon's local socket / HTTP API.
//!
//! Test: `cargo test -p trusty-mpm-core` exercises serde round-trips and the
//! claude-mpm frontmatter parser against fixture files.

pub mod agent;
pub mod agent_builder;
pub mod agent_deployer;
pub mod agent_manifest;
pub mod artifact;
pub mod budget;
pub mod bundle;
pub mod circuit;
pub mod claude_config;
pub mod compress;
pub mod connect;
pub mod delegation_authority;
pub mod deterministic_overseer;
pub mod discovery;
pub mod doctor;
pub mod error;
pub mod external_session;
pub mod hook;
pub mod instruction_overrides;
pub mod instruction_pipeline;
pub mod ipc;
pub mod llm_overseer;
pub mod memory;
pub mod names;
pub mod overseer;
pub mod overseer_config;
pub mod paths;
pub mod process;
pub mod project;
pub mod project_discovery;
pub mod session;
pub mod session_launch;
pub mod session_store;
pub mod skill_deployer;
pub mod skill_manifest;
pub mod tmux;

pub use connect::{ResolveResult, SessionSummary, resolve_target};
pub use discovery::{DEFAULT_DAEMON_URL, lock_file_path, resolve_daemon_url};
pub use error::{Error, Result};
