//! trusty-agents library surface.
//!
//! Why: trusty-agents was originally a binary-only crate. External agent crates
//!      (e.g. `crates/cto-assistant`) need to implement `ToolExecutor` and
//!      return an `AgentPlugin` so the ctrl loop can register their tools
//!      via dependency injection rather than hard-coded `if persona ==`
//!      branches. Exposing the existing module tree through a library
//!      target enables that without duplicating code.
//! What: Declares every top-level module of the crate as `pub mod` so both
//!       the `tagent` and `ompm` binaries (which now consume this lib via
//!       `use trusty_agents::...`) and downstream agent crates can reach the
//!       internals. Also publishes a curated `agent_api` facade that pins
//!       the minimal stable surface external agents should depend on
//!       (`ToolExecutor`, `ToolResult`, `ToolExecutionTier`, `AgentPlugin`,
//!       `ServiceTier`).
//! Test: Compile-tested via `crates/cto-assistant` which depends on
//!       `trusty-agents` as a library and constructs an `AgentPlugin`.

/// Re-export the harness adapter framework from trusty-agents-common.
///
/// Why: Moved to trusty-agents-common in Wave 1 (issue #862, refs #830/#832) so
///      external crates can use `HarnessAdapter`, `AdapterRegistry`, etc. without
///      depending on the full `trusty-agents` binary crate. This shim preserves
///      every existing `crate::adapters::X` reference inside trusty-agents
///      without touching any call site.
/// What: Explicit re-export of all public items from `trusty_agents_common::adapters`,
///       including submodule re-exports so path-qualified references remain valid.
/// Test: `cargo build --workspace` verifies all internal call sites resolve.
pub mod adapters {
    // Submodules (re-exported so path references like `adapters::registry::Foo` resolve).
    pub use trusty_agents_common::adapters::augment;
    pub use trusty_agents_common::adapters::claude_code;
    pub use trusty_agents_common::adapters::claude_mpm;
    pub use trusty_agents_common::adapters::codex;
    pub use trusty_agents_common::adapters::gemini;
    pub use trusty_agents_common::adapters::patterns;
    pub use trusty_agents_common::adapters::registry;
    pub use trusty_agents_common::adapters::shell;
    pub use trusty_agents_common::adapters::traits;
    pub use trusty_agents_common::adapters::trusty_agents_adapter;
    // Flat re-exports of every item promoted to the adapters facade.
    pub use trusty_agents_common::adapters::{
        // trait + value types
        AdapterInfo,
        // registry
        AdapterRegistry,
        // concrete adapters
        AugmentAdapter,
        ClaudeCodeAdapter,
        ClaudeMpmAdapter,
        CodexAdapter,
        DetectionResult,
        GeminiAdapter,
        HarnessAdapter,
        HarnessObservation,
        HarnessState,
        // patterns helpers
        Pattern,
        ShellAdapter,
        TrustyAgentsAdapter,
        any_match,
        best_match,
        last_n_lines,
    };
}

pub mod agents;
pub mod api;
pub mod ast;
pub mod build_info;
pub mod bus;
pub mod cli;
#[allow(dead_code)]
pub mod compress;
pub mod context;
pub mod ctrl;
pub mod ctrl_session;
pub mod debugger;
pub mod docs_index;
pub mod env_compat;
pub mod eval;
pub mod events;
pub mod git;
pub mod identity;
pub mod init;
pub mod inspection;
pub mod intent;
pub mod interaction_log;
pub mod ipc;
pub mod llm;
pub mod local_inference;
pub mod logging;
pub mod mcp;
pub mod memory;
pub mod mistake_log;
pub mod perf;
pub mod plugins;
pub mod process_tracker;
pub mod progress;
pub mod rbac;
pub mod recap;
pub mod registry;
pub mod repl;
pub mod rpc;
pub mod search;
pub mod service;
pub mod session;
pub mod session_record;
/// Re-export the JSON-backed session ledger from trusty-agents-common.
///
/// Why: Moved to trusty-agents-common in Wave 1 (issue #862, refs #830/#832).
///      This shim preserves every existing `crate::session_registry::X`
///      reference inside trusty-agents without touching any call site.
/// What: Explicit re-export of all public items from
///       `trusty_agents_common::session_registry`.
/// Test: `cargo build --workspace` verifies all internal call sites resolve.
pub mod session_registry {
    pub use trusty_agents_common::session_registry::{SessionEntry, SessionsRegistry};
}

pub mod skills;
pub mod slack;
pub mod state_writer;
pub mod subprocess;
pub mod telegram;
pub mod ticketing;
pub mod tm;
pub mod tmux;
pub mod tools;
pub mod update;
pub mod usage;
pub mod workflow;

#[cfg(test)]
pub mod test_env;

pub mod runtime;

/// Re-export `install_plugins` so external launchers can register agent
/// plugins (e.g. `cto-assistant`) before calling `run()`.
///
/// Why: `trusty-agents` cannot depend on `publish = false` agent crates.
///      A private workspace binary (`trusty-agents-local`) wires the plugin
///      registry at startup via this re-export, keeping the published
///      surface free of those crates.
/// What: Forwards to `tools::agent_plugin::install_plugins`. The underlying
///       store is a OnceLock — call exactly once before `run()`.
/// Test: Exercised by `trusty-agents-local`'s startup; locally by the existing
///       agent_plugin unit tests.
pub use tools::agent_plugin::install_plugins;

/// Re-export `run` at the crate root so launchers can call
/// `trusty_agents::run().await` without referencing the `runtime` module.
pub use runtime::run;

/// Re-exports of items that internal modules historically referenced as
/// `crate::AgentConfig` and `crate::default_bundled_config_dir`.
///
/// Why: Those references resolved to the `main.rs` crate root before the
///      library target was added (when the crate was binary-only). Adding
///      `lib.rs` shifted the meaning of `crate::` for every internal module
///      to the library root; without these re-exports, paths in
///      `ctrl/mod.rs`, `inspection/mod.rs`, etc. fail to resolve. Re-exporting
///      here is the minimal change that keeps every internal call site
///      working without a workspace-wide sweep.
/// What: Re-publishes `AgentConfig` (defined in `agents/mod.rs`) at the lib
///       root. `default_bundled_config_dir` lives in `main.rs` and is also
///       re-defined here for lib consumers (the binary still calls it
///       through `trusty_agents::default_bundled_config_dir`).
/// Test: `cargo check --workspace` resolves the previously-broken paths.
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
    if !new_dir.exists() {
        let legacy = PathBuf::from(".open-mpm");
        if legacy.exists() {
            tracing::warn!(
                ".open-mpm config dir detected; migrate to .trusty-agents \
                 (trusty-agents will read from .open-mpm until you rename it)"
            );
            return legacy;
        }
    }
    new_dir
}

/// Curated, stable surface for external agent crates.
///
/// Why: External agent crates (e.g. `cto-assistant`) should depend on a
///      tiny, well-defined slice of `trusty-agents` — not the whole internal
///      module tree. Pinning the surface here lets us refactor internals
///      without breaking downstream agent crates.
/// What: Re-exports `ToolExecutor`, `ToolResult`, `ToolExecutionTier`,
///       `AgentPlugin`, and `ServiceTier`. Adding to this list is a
///       conscious choice; nothing else should be considered "API" for
///       external agents.
/// Test: Used by `crates/cto-assistant/src/lib.rs`.
pub mod agent_api {
    pub use crate::rbac::ServiceTier;
    pub use crate::tools::agent_plugin::AgentPlugin;
    pub use crate::tools::traits::{ToolExecutionTier, ToolExecutor, ToolResult};
}
