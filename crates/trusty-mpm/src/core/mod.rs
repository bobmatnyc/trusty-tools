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
pub mod agent_reset;
pub mod agent_reset_workspace;
pub mod artifact;
pub mod auto_resume;
pub mod budget;
pub mod bundle;
// DOC-28 cutover bridge: incremental catch-up runtime — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod catchup;
pub mod circuit;
pub mod claude_config;
// DOC-28 cutover bridge: cross-format session discovery — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod claude_mpm_registry;
pub mod claude_mpm_session;

pub mod compress;
pub mod config;
pub mod connect;
pub mod delegation_authority;
pub mod deploy_validate;
pub mod deterministic_overseer;
pub mod discovery;
pub mod doctor;
pub mod error;
pub mod external_session;
pub mod frontmatter;
pub mod gh_account;
pub mod gh_identity;
pub mod git_identity;
pub mod home_trust_seed;
pub mod hook;
pub mod idle_nudge;
pub mod idle_parking;
pub mod instruction_overrides;
pub mod instruction_pipeline;
pub mod ipc;
pub mod llm_overseer;
pub mod managed_config;
pub mod manifest;
pub mod mcp_config;
pub mod mcp_test;
pub mod memory;
pub mod model_inject;
pub mod names;
pub mod oauth_token;
// DOC-28 cutover bridge: unified session finder — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod native_session_finder;
pub mod output_style;
pub mod output_style_deployer;
pub mod overseer;
pub mod overseer_config;
pub mod paths;
pub mod pid_registry;
pub mod process;
pub mod project;
pub mod project_aliases;
pub mod project_discovery;
pub mod protected_dirs;
pub mod provisioning_stage;
pub mod session;
pub mod session_launch;
pub mod session_store;
pub mod skill_deployer;
pub mod skill_manifest;
pub mod skill_source;
pub mod sm;
pub mod stale_skills;
pub mod standalone;
pub mod tmux;
pub mod trusty_tools_config;
pub mod update_check;
pub mod workspace_scan;
pub mod worktree_naming;

pub use connect::{ResolveResult, SessionSummary, resolve_target};
pub use discovery::{
    DEFAULT_CONSOLE_ADDR, DEFAULT_DAEMON_ADDR, DEFAULT_DAEMON_URL, GATEWAY_PATH,
    default_daemon_addr, explicit_url_from_env, lock_file_path, resolve_daemon_url,
    resolve_daemon_url_probing, resolve_daemon_url_via_gateway,
};
pub use error::{Error, Result};
