//! Per-project MCP state a session carries in its ENVIRONMENT (#4181, ADR-0042).
//!
//! Why: a single shared MCP declaration in user scope cannot carry a per-project
//! argument, so the two pins tm used to write into a workspace `.mcp.json` move
//! to environment variables the spawn exports. `trusty-memory` reads
//! `TRUSTY_MEMORY_PALACE` (its highest-precedence palace override, #1605);
//! `trusty-search serve` reads `TRUSTY_INDEX` (#5394 — before that, the `Serve`
//! subcommand parsed its own `--index` field and never consulted the global
//! `env`-backed one, so the variable was accepted and ignored by exactly the one
//! path that needed it). Claude Code passes its own environment to every stdio
//! MCP server it spawns unmodified, so a variable set here arrives at the server.
//!
//! Both pins stay gated on the SAME `[mcp] trusty_memory` / `[mcp] trusty_search`
//! manifest toggles that used to gate the injectors, which is why
//! [`resolve_conditional_mcp_toggles`] lives on after the trust derivation it was
//! written for is gone: neither `runtime::claude_code` nor `bin/tm/commands/launch`
//! has a `HarnessPlan` in scope, so each re-reads the manifest here.
//! Test: `mcp_session_env_tests.rs`.

use std::path::Path;

/// Re-resolve whether the effective harness manifest currently enables the
/// `trusty-memory` / `trusty-search` integrations for `project_dir`.
///
/// Why: the spawn paths that must gate the two env exports
/// ([`session_mcp_env`]) run in call chains that never held the in-memory
/// `HarnessPlan` — `runtime::claude_code::prepare_managed_config` is reached by
/// `spawn_resume` with no `prepare_session*` anywhere in its chain, and
/// `bin/tm/commands/launch` composes its command line after `prepare_session`
/// has already returned. This pure re-derivation resolves the IDENTICAL manifest
/// layering `prepare_session_inner` uses (`ManifestSources::resolve` +
/// `resolve_manifest` + `HarnessPlan::from_manifest`, project > user > catalog >
/// default), so the two can never diverge. It reads files, it never writes, and
/// running it again after `prepare_session_inner` is idempotent.
/// What: returns `(trusty_memory_enabled, trusty_search_enabled)`.
/// Test: `resolve_conditional_mcp_toggles_defaults_to_both_on`,
/// `resolve_conditional_mcp_toggles_honors_project_manifest_toggle`.
pub fn resolve_conditional_mcp_toggles(
    fw: &crate::core::paths::FrameworkPaths,
    project_dir: &Path,
) -> (bool, bool) {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let sources = crate::core::manifest::ManifestSources::resolve(project_dir, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = crate::core::manifest::HarnessPlan::from_manifest(&manifest, fw, &catalog_root);
    (plan.inject_trusty_memory, plan.inject_trusty_search)
}

/// Build the per-project MCP environment assignments a session's `claude`
/// process must carry.
///
/// Why (#4181): these two variables replace the `env.TRUSTY_MEMORY_PALACE` block
/// and the `serve --index <id>` argument the deleted injectors wrote into the
/// workspace `.mcp.json`. Without them the shared user-scope declarations are
/// argless and both pins regress — #1605's wrong-palace and #1373's wrong-index
/// bugs return.
/// What: resolves the manifest toggles for `project_dir`, then
/// * `TRUSTY_MEMORY_PALACE` = [`crate::core::session_launch::resolve_palace_slug`]
///   when `[mcp] trusty_memory` is on and a slug can be derived;
/// * `TRUSTY_INDEX` = [`crate::core::session_launch::register_project_index`]
///   when `[mcp] trusty_search` is on and the daemon CONFIRMED the index (#5091 —
///   an unconfirmed id 404s every later search, so it is withheld).
///
/// A disabled toggle, an underivable slug, or an unconfirmed index each omit
/// their variable rather than exporting an empty one, leaving the server on its
/// own cwd-based derivation. This performs the same best-effort daemon
/// find-or-create `prepare_session_inner` used to, so it is a network-touching
/// call, not a pure one — callers run it once per spawn, off the command-string
/// builders.
/// Test: `session_mcp_env_exports_palace_when_memory_enabled`,
/// `session_mcp_env_omits_palace_when_memory_disabled`,
/// `session_mcp_env_omits_index_when_search_disabled`.
pub fn session_mcp_env(project_dir: &Path, git_remote: Option<&str>) -> Vec<(String, String)> {
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(project_dir);
    session_mcp_env_with(&fw, project_dir, git_remote)
}

/// Hermetic core of [`session_mcp_env`], taking the framework layout explicitly.
///
/// Why: [`crate::core::paths::FrameworkPaths::for_managed_workspace`] reads the
/// operator's home, which unit tests must not depend on — the same
/// hermetic-core split `ensure_managed_config_dir` / `_with_root` already uses.
/// What: see [`session_mcp_env`].
/// Test: see [`session_mcp_env`].
pub fn session_mcp_env_with(
    fw: &crate::core::paths::FrameworkPaths,
    project_dir: &Path,
    git_remote: Option<&str>,
) -> Vec<(String, String)> {
    let (memory_enabled, search_enabled) = resolve_conditional_mcp_toggles(fw, project_dir);
    let mut env = Vec::new();

    if memory_enabled {
        if let Some(slug) =
            crate::core::session_launch::resolve_palace_slug(project_dir, git_remote)
        {
            env.push((trusty_common::PALACE_OVERRIDE_ENV.to_string(), slug));
        }
    } else {
        tracing::debug!("manifest disables trusty-memory: no palace pin exported");
    }

    if search_enabled {
        if let Some(index_id) = crate::core::session_launch::register_project_index(project_dir) {
            env.push((SEARCH_INDEX_ENV.to_string(), index_id));
        }
    } else {
        tracing::debug!("manifest disables trusty-search: no index pin exported");
    }

    env
}

/// The variable `trusty-search`'s global `--index` flag reads (#5394).
pub const SEARCH_INDEX_ENV: &str = "TRUSTY_INDEX";

#[cfg(test)]
#[path = "mcp_session_env_tests.rs"]
mod tests;
