//! trusty-assistants library surface: the assistant core.
//!
//! Why: the assistant's homes, OKG stores, channels, listeners, memory and
//! knowledge are library code that a host process drives. #11632 cut the
//! agent harness that used to host them (PM orchestrator, CTRL, REPL,
//! workflows, HTTP API, UI); the `ta` binary now serves only stdio MCP and
//! the `config` verbs (`cli`, `mcp_serve`).
//! What: declares every kept module as `pub mod`, and publishes a curated
//! `agent_api` facade (`ToolExecutor`, `ToolResult`, `ToolExecutionTier`,
//! `AgentPlugin`, `ServiceTier`) for out-of-workspace launchers.
//! Test: compile-tested by the workspace build; the facade's plugin seam is
//! covered by `agent_plugin_lookup_returns_matching_plugin`.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

pub mod agents;
// #4325: per-assistant home directory + OKG store model — "Assistant" is a
// TYPE, `izzie`/`cto-assistant` are INSTANCES of it, each with its own home.
pub mod assistants;
// #7370: chat-thread attachments stored under `<assistant home>/attachments`,
// a sibling of `okg` — files a turn references, never indexed knowledge.
pub mod attachments;
// #4652: last-human-turn timeout — the only signal that can tell a human
// attending an instance from the assistant's own activity (owner decision D3).
pub mod attendance;
pub mod build_info;
pub mod bus;
// #7427: one `ChannelAdapter` per channel provider, behind a registry.
// #7609: the merged channel model (`channels::Channel`, its migrations and the
// precedence rule) is public, like the `listeners` module it supersedes.
// #11632: the inbound half (adapter `receive`, wake routing, dispatch status)
// lost its only callers with the poll loop and the HTTP inbound route; it is
// kept unchanged for the inbound rebuild rather than deleted.
#[allow(dead_code)]
pub mod channels;
// #11632: the `ta` argv dispatch left after the harness cut.
pub mod cli;
pub mod env_compat;
pub mod events;
pub mod identity;
pub mod intent;
// #10746 S11: the agent-side KB glue over trusty-okf, moved from the deleted
// trusty-kb crate (SPEC-OKF-01 §2.7.2).
pub mod kb;
pub mod knowledge;
// #11632: the stored-event lookup and wake prompt lost their callers with the
// poll loop; kept unchanged for the inbound rebuild.
#[allow(dead_code)]
pub mod listeners;
// #11632: only the assistant model-choice part of the cut `llm` module.
pub mod llm;
/// The `[log_drain]` config section (#6537); its scheduler was cut with the
/// daemon (#11632).
pub mod log_drain;
pub mod logging;
pub mod mcp;
// #11632: the stdio MCP server, the one serving surface left after the cut.
pub mod mcp_serve;
pub mod memory;
// #11632: assistant operations that lived under the cut `api::server`. Their
// HTTP-only write paths and the knowledge intake lost their callers with the
// routes; they are kept unchanged rather than rewritten in this cut.
#[allow(dead_code)]
pub mod operations;
pub mod plugins;
pub mod rbac;
// #11632: the project registry the knowledge pipeline reads its sources from.
pub mod registry;
pub mod rpc;
// #11632: only the skill catalog the memory-tool grant check reads.
pub mod skills;
pub mod state_writer;
pub mod stores;
pub mod tls;
pub mod tools;
pub mod untrusted;
pub mod usage;

/// A stand-in trusty-memory daemon on a temp Unix socket, for tests (#6286).
#[cfg(test)]
pub(crate) mod uds_mock;

#[cfg(test)]
pub mod test_env;

/// Re-export `install_plugins` so external launchers can register agent
/// plugins before building a tool registry.
///
/// Why: `trusty-assistants` cannot depend on `publish = false` agent crates. A
///      downstream launcher wires the plugin registry at startup via this
///      re-export, keeping the published surface free of those crates.
/// What: Forwards to `tools::agent_plugin::install_plugins`. The underlying
///       store is a OnceLock — call exactly once.
/// Test: `agent_plugin`'s unit tests; no in-workspace launcher installs plugins.
pub use tools::agent_plugin::install_plugins;

/// Re-publishes `AgentConfig` (defined in `agents/config.rs`) at the lib root,
/// where internal modules historically reference it as `crate::AgentConfig`.
pub use agents::AgentConfig;

/// Default location of the bundled `.trusty-agents/` config directory.
///
/// Why: Several lib modules (`ctrl/mod.rs`, `inspection/mod.rs`) reference
///      this as `crate::default_bundled_config_dir`. It used to live in
///      `main.rs`; promoting it to the lib lets every consumer share one
///      implementation and removes the broken `crate::` path.
/// What: Honours `TAGENT_CONFIG_DIR` (with fallback to deprecated
///       `OPEN_MPM_CONFIG_DIR`, stripping a legacy `/agents` suffix);
///       falls back to `.trusty-agents` relative to the process CWD.
///       If `.trusty-agents` does not exist but `.open-mpm` does, reads
///       from `.open-mpm` with a migration warning.
/// Test: Indirectly via the agent-registry load path and inspection tests.
pub fn default_bundled_config_dir() -> std::path::PathBuf {
    default_bundled_config_dir_checking(std::path::Path::new("."))
}

/// Same resolution as [`default_bundled_config_dir`], but the two
/// existence checks (`.trusty-agents`, `.open-mpm`) are rooted at
/// `search_root` instead of implicitly at the process CWD (issue #3516).
///
/// Why: The only way to unit-test the legacy-migration branch used to be
/// `std::env::set_current_dir` into a scratch tempdir for the duration of
/// the test — but CWD is process-global, exactly like an env var, and
/// `cargo test` runs every test in this crate's lib binary as threads of
/// ONE process. Two OTHER tests in this crate resolve their own
/// COMMON-relative paths against the (possibly-swapped) CWD at call time —
/// `api::server::handlers::projects_config_dir()` (`.trusty-agents/projects`)
/// and `agents_dir()` (`.trusty-agents/agents`) — so a concurrently-running
/// CWD-mutating test could race `get_project_config_falls_back_to_registry_when_toml_missing`
/// and other handler tests that assume CWD is the real repo root, even
/// though those tests hold `HOME_LOCK` (a *different* mutex; CWD isn't an
/// env var and `HOME_LOCK` was never meant to guard it). Rooting the
/// EXISTENCE CHECK at an explicit `search_root` parameter — while still
/// returning the same bare-relative `PathBuf` the production contract
/// documents (callers join it against CWD themselves, unchanged) — lets
/// the migration test point at a tempdir directly, so this function's own
/// tests need no CWD mutation, no lock, and cannot race anything.
/// What: identical branch logic to `default_bundled_config_dir`, except
/// `.exists()` is checked via `search_root.join(...)` rather than the bare
/// relative path.
/// Test: `env_compat::tests::config_dir_migration_returns_legacy_open_mpm_when_new_absent`.
fn default_bundled_config_dir_checking(search_root: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Path, PathBuf};
    let config_dir_str = env_compat::env_var("TAGENT_CONFIG_DIR", "OPEN_MPM_CONFIG_DIR").ok();
    if let Some(s) = config_dir_str.filter(|s| !s.is_empty()) {
        let p = PathBuf::from(s);
        return if p.file_name().and_then(|n| n.to_str()) == Some("agents") {
            p.parent().map(Path::to_path_buf).unwrap_or(p)
        } else {
            p
        };
    }
    // Prefer .trusty-agents; migrate transparently from legacy .open-mpm.
    let new_dir = PathBuf::from(".trusty-agents");
    if !search_root.join(&new_dir).exists() {
        let legacy = PathBuf::from(".open-mpm");
        if search_root.join(&legacy).exists() {
            tracing::warn!(
                ".open-mpm config dir detected; migrate to .trusty-agents \
                 (trusty-assistants will read from .open-mpm until you rename it)"
            );
            return legacy;
        }
    }
    new_dir
}

/// Curated, stable surface for external agent crates.
///
/// Why: An out-of-workspace launcher should depend on a tiny, well-defined
///      slice of `trusty-assistants` — not the whole internal module tree.
/// What: Re-exports `ToolExecutor`, `ToolResult`, `ToolExecutionTier`,
///       `AgentPlugin`, and `ServiceTier`.
/// Test: Compile-tested by the workspace build; the re-exported plugin type
///       is covered by `agent_plugin_lookup_returns_matching_plugin`.
pub mod agent_api {
    pub use crate::rbac::ServiceTier;
    pub use crate::tools::agent_plugin::AgentPlugin;
    pub use crate::tools::traits::{ToolExecutionTier, ToolExecutor, ToolResult};
}
