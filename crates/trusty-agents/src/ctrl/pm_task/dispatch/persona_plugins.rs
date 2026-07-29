//! `[[plugins.python]]` registration for the persona-chat dispatch path (#446).
//!
//! Why: the `PythonToolPlugin` subprocess primitive
//! (`plugins::python_tool` — spawn `python3 <script>`, one NDJSON `tool_call`
//! in / one `tool_result` out, per-call timeout, fail-closed RBAC tier guard)
//! has been fully built and unit-tested since #446, and `AgentConfig` has
//! parsed `[plugins]` into `AgentPluginsConfig` for just as long — but NOTHING
//! in the workspace ever called `PythonToolPlugin::from_config`. An agent or
//! skill package declaring a `[[plugins.python]]` tool therefore had its
//! declaration parsed and then silently dropped: the tool was never
//! registered, never advertised, never callable, and no warning was emitted.
//! This module is that missing seam. It lives in its own file rather than
//! inline in `persona.rs` for the same reason `persona_gate` does — `persona.rs`
//! sits just under the 500-SLOC production cap enforced by
//! `scripts/check_line_cap.sh` — and, like `build_persona_delegate_tool`, it
//! returns/mutates the EXACT value `run_pm_task_with_persona` uses so the unit
//! tests drive the production construction rather than a re-derived copy.
//! What: [`register_python_plugins`] — builds each declared entry via
//! `PythonToolPlugin::from_config`, resolving a package-relative `script` /
//! `schema_file` against the caller's `base_dir`, and registers it into the
//! persona's `ToolRegistry`. Two independent skip paths, both non-fatal:
//! a name already taken by an earlier registration, and a config
//! `from_config` rejects (e.g. an unknown `restricted_tiers` string, which
//! #3236 made fail closed). Registration is NOT a grant — the `[tools].allow`
//! glob filter, the RBAC tier filter and the scope gate in
//! `persona_gate::filter_persona_tool_names_for_tier` all still run afterwards
//! and decide whether the tool reaches the LLM.
//! Test: `persona_python_plugin_registers_callable_tool`,
//! `persona_python_plugin_bad_config_is_skipped`,
//! `persona_python_plugin_does_not_shadow_an_existing_tool`,
//! `persona_python_plugin_empty_list_registers_nothing`; the CALL SITE in
//! `run_pm_task_with_persona` is pinned end-to-end by
//! `persona_python_plugin_is_registered_by_the_dispatch_path`
//! (`tests/persona_python_plugin_wiring.rs`).

use std::path::Path;
use std::sync::Arc;

use crate::plugins::{PythonPluginConfig, PythonToolPlugin};
use crate::tools::ToolRegistry;

/// Register a persona's declared `[[plugins.python]]` entries as callable
/// tools (#446, epic #3052).
///
/// Why: this is the whole fix — see the module doc for why the declaration
/// was inert before. Written as a standalone function taking `&mut
/// ToolRegistry` (rather than building a fresh registry, or returning
/// executors for the caller to register) so the tests can assert on the same
/// registry type the dispatch path arms and then actually DISPATCH through it,
/// proving the tool is callable and not merely present in a list.
/// What: for each entry, in declaration order — skip (WARN) when `base` already
/// holds a tool of that name, so a user-authored plugin can never shadow a
/// native/MCP tool the persona also holds (`ToolRegistry::register` overwrites
/// silently in release and `debug_assert!`s in debug, so an unguarded
/// registration here would be a debug-build panic triggerable from agent TOML);
/// skip (WARN) when `PythonToolPlugin::from_config` rejects the entry, so one
/// typo costs one tool rather than the whole chat turn; otherwise register it.
/// Returns the names actually registered, in order, so the caller logs what it
/// got rather than what was asked for.
/// Test: `persona_python_plugin_registers_callable_tool` (registered AND
/// dispatchable, with a package-relative `script`),
/// `persona_python_plugin_bad_config_is_skipped`,
/// `persona_python_plugin_does_not_shadow_an_existing_tool`,
/// `persona_python_plugin_empty_list_registers_nothing`.
pub(super) fn register_python_plugins(
    registry: &mut ToolRegistry,
    plugins: &[PythonPluginConfig],
    base_dir: &Path,
    persona_name: &str,
) -> Vec<String> {
    let mut registered = Vec::with_capacity(plugins.len());
    for cfg in plugins {
        let plugin_name = cfg.name.clone();
        if registry.contains(&plugin_name) {
            tracing::warn!(
                persona = %persona_name,
                plugin = %plugin_name,
                "skipping [[plugins.python]] entry: a tool of that name is already registered"
            );
            continue;
        }
        match PythonToolPlugin::from_config(cfg.clone(), base_dir) {
            Ok(plugin) => {
                registry.register(Arc::new(plugin));
                registered.push(plugin_name);
            }
            Err(e) => tracing::warn!(
                persona = %persona_name,
                plugin = %plugin_name,
                "skipping [[plugins.python]] entry: {e}"
            ),
        }
    }
    registered
}
