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
pub mod agent_cost;
pub mod agent_deployer;
pub mod agent_manifest;
pub mod agent_metadata;
pub mod agent_reset;
pub mod agent_reset_workspace;
pub mod agent_skill_codeploy;
// #4840: bundled agent assets reached `$CLAUDE_CONFIG_DIR/agents/` only via a
// manual `tm install`; this module makes the refresh+deploy automatic.
pub mod agent_source;
pub mod artifact;
pub mod auto_resume;
pub mod binary_provenance;
pub mod budget;
pub mod bundle;
// Epic #4183: the DEFAULT (bundled-fallback) PM prompt, re-sourced through
// `instruction_package`. Byte-identical to the legacy assembly it replaces; the
// override configurations stay on that legacy path by design.
pub mod bundled_pm_package;
// #4442/#4448: the ONE bundled-agent name roster, shared by `tm doctor`'s
// asset_tier probe (which reports) and the quarantine (which moves).
pub mod bundled_roster;
// DOC-28 cutover bridge: incremental catch-up runtime — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod catchup;
pub mod circuit;
pub mod claude_config;
// Issue #4467: the shared set of Claude Code process-local session markers every
// managed spawn must scrub. An inherited `CLAUDE_CODE_CHILD_SESSION` makes
// Claude Code disable transcript saving, costing the session native
// `--resume`/`--continue`/`/rewind` recovery.
pub mod claude_env_scrub;
// Epic #4183 / #4286: the READER for `CLAUDE.md` named-section instruction
// overrides. Ships before the floor text that advertises the mechanism —
// advertising an override no code reads is issue #381 verbatim.
pub mod claude_md_sections;
// Issue #4754: the WRITER counterpart to `claude_md_sections` — the single
// owner of `CLAUDE.md` section-override edits. Idempotent by construction, and
// it borrows the reader's grammar rather than spelling a second one.
pub mod claude_md_writer;
// Issue #4072: one process-wide lock every `~/.claude.json` read-modify-write
// seeder holds, so concurrent daemon provisioning cannot lose a trust entry.
pub mod claude_json_guard;
// DOC-28 cutover bridge: cross-format session discovery — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod claude_mpm_registry;
pub mod claude_mpm_session;

pub mod compress;
pub mod config;
/// Unrecognised-key reporting for the host-level config files (#5207).
pub mod config_keys;
pub mod connect;
pub mod delegation_authority;
pub mod deploy_validate;
pub mod deterministic_overseer;
pub mod discovery;
/// Working-tree isolation policy for native Agent-tool dispatches (#4480).
pub mod dispatch_isolation;
pub mod doctor;
pub mod doctor_repair;
pub mod error;
pub mod exit_codes;
pub mod external_session;
pub mod frontmatter;
pub mod gh_account;
pub mod gh_identity;
pub mod git_identity;
pub mod harness_root;
pub mod home_trust_seed;
pub mod hook;
pub mod idle_nudge;
pub mod idle_parking;
pub mod instruction_overrides;
// Issue #4184 / epic #4183: the sectioned-JSON instruction package schema. Types
// + validation only; `bundled_pm_package` is its first composing call site.
pub mod instruction_package;
pub mod instruction_pipeline;
// Epic #4183: committed snapshots of the fully composed PM prompt. The
// delivered-prompt diff a content change produces is the review artifact.
pub mod ipc;
pub mod llm_overseer;
pub mod managed_config;
pub mod manifest;
pub mod mcp_config;
pub mod mcp_provenance;
// #4181: per-project MCP pins now travel as spawn environment variables, not as
// arguments injected into a workspace `.mcp.json` (ADR-0042).
pub mod mcp_session_env;
pub mod mcp_test;
pub mod memory;
pub mod memory_import;
pub mod model_inject;
pub mod names;
pub mod oauth_token;
#[cfg(test)]
#[path = "pm_prompt_golden_tests.rs"]
mod pm_prompt_golden_tests;
// DOC-28 cutover bridge: unified session finder — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod native_session_finder;
pub mod output_style;
pub mod output_style_deployer;
pub mod overseer;
pub mod overseer_config;
// #4058: single canonical source for the crate's own `[[bin]]` names, so
// discovery/hooks/statusline/daemon-PID lists can't drift out of sync again.
pub mod own_binary_names;
pub mod paths;
pub mod pid_registry;
pub mod process;
pub mod project;
pub mod project_aliases;
/// The committed, project-level `.trusty-mpm.toml` config surface (#5207).
pub mod project_config;
pub mod project_discovery;
// #4880: the project skill tier redeploys on project-manifest change.
pub mod project_skill_tier;
pub mod project_trust;
pub mod protected_dirs;
pub mod provisioning_stage;
pub mod push_guard;
// `tm reinstall`: the two-hop asset redeploy across every deploy destination,
// and the install-provenance route its `--binary` flag takes.
pub mod binary_reinstall;
pub mod reinstall;
pub mod scaffold_gitignore;
pub mod session;
pub mod session_assets;
pub mod session_launch;
pub mod session_store;
pub mod skill_deploy_tiers;
pub mod skill_deployer;
pub mod skill_drift;
pub mod skill_manifest;
pub mod skill_reconcile;
pub mod skill_repair;
pub mod skill_retire;
pub mod skill_source;
pub mod skill_staleness;
pub mod skill_tiers;
pub mod skill_unmanaged;
pub mod sm;
pub mod spawn_disclaim;
pub mod stack_profile;
pub mod stale_skills;
pub mod standalone;
pub mod stray_mcp;
pub mod tmux;
pub mod trusty_tools_config;
pub mod update_check;
pub mod version_staleness;
pub mod workspace_liveness;
pub mod workspace_scan;
pub mod worktree_index;
pub mod worktree_naming;

pub use connect::{ResolveResult, SessionSummary, resolve_target};
pub use discovery::{
    DEFAULT_CONSOLE_ADDR, DEFAULT_DAEMON_ADDR, DEFAULT_DAEMON_URL, DaemonUrlError,
    EXIT_DAEMON_URL_UNREACHABLE, GATEWAY_PATH, default_daemon_addr, explicit_url_from_env,
    lock_file_path, resolve_daemon_url, resolve_daemon_url_for_cli, resolve_daemon_url_probing,
    resolve_daemon_url_via_gateway,
};
pub use error::{Error, Result};
