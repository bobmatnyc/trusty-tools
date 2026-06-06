//! Agent plugin injection point.
//!
//! Why: The ctrl loop previously hard-coded per-persona tool registration
//!      (`if persona_name == "cto-assistant" { register(cto_db_tools()) }`).
//!      That coupled `trusty-agents` directly to every agent's tool surface and
//!      forced sensitive agent logic (HR/budget queries) to live inside the
//!      host binary. `AgentPlugin` decouples this: external agent crates
//!      construct their tool list and hand it to `trusty-agents` as a plugin,
//!      and the ctrl loop matches `plugin.persona_name` against the active
//!      persona at session start.
//! What: A named bundle of `ToolExecutor`s associated with a specific
//!       persona name. The ctrl loop iterates over the injected plugin list
//!       and, for the active persona, registers each plugin's tools into
//!       the session's `ToolRegistry`. Plugins are constructed once at
//!       process start (in `main.rs`) and threaded through to ctrl.
//! Test: Exercised via `crates/cto-assistant` integration — the cto-assistant
//!       crate constructs an `AgentPlugin` and `main.rs` injects it; the
//!       cto-assistant persona then sees its four CTO DB tools registered.

use std::sync::OnceLock;

pub use trusty_agents_common::AgentPlugin;

/// Process-global registry of injected agent plugins.
///
/// Why: Threading a `Vec<AgentPlugin>` through every public ctrl function
///      (`run_pm_task_with_persona`, `run_pm_task_with_history`, …) would
///      cascade through dozens of call sites and the workflow engine. A
///      `OnceLock` set once at process startup by `main.rs` (before any
///      ctrl task runs) gives the ctrl loop read-only access to the
///      registered plugins with zero signature churn. The "set once" shape
///      is enforced by `OnceLock`; tests can `init_for_tests` with their
///      own list because `OnceLock::set` is idempotent only on the first
///      call (tests that need to re-init use `set_if_unset` semantics).
/// What: `PLUGINS` stores the injected `Vec<AgentPlugin>`. `set` is a
///       one-shot installer called by `main.rs`. `get_for_persona` returns
///       the plugins (if any) that target a given persona name.
/// Test: Exercised by `agent_plugin_lookup_returns_matching_plugin`.
static PLUGINS: OnceLock<Vec<AgentPlugin>> = OnceLock::new();

/// Install the process-wide plugin list. Idempotent: subsequent calls are
/// silently ignored, matching `OnceLock::set`'s "first writer wins" rule.
///
/// Why: `main.rs` constructs the list once at startup before launching ctrl
///      tasks. Workflow tests that don't care about plugins simply never
///      call this; their `get_for_persona` lookups return an empty slice.
/// What: Stores `plugins` in the `PLUGINS` `OnceLock`. Returns `Ok(())` on
///       the first call, `Err(plugins)` on later calls (caller can decide
///       to ignore or panic). Most callers treat the error as a no-op.
/// Test: Implicitly via the bin's startup path and the lookup test below.
pub fn install_plugins(plugins: Vec<AgentPlugin>) -> Result<(), Vec<AgentPlugin>> {
    PLUGINS.set(plugins)
}

/// Look up the injected plugins targeting a given persona name.
///
/// Why: The ctrl loop calls this when building a session's `ToolRegistry`
///      to add per-persona tools without hard-coding any persona's name.
/// What: Returns an iterator over `&AgentPlugin`s whose `persona_name`
///       matches. Yields nothing when `install_plugins` was never called.
/// Test: `agent_plugin_lookup_returns_matching_plugin`.
pub fn plugins_for_persona(persona_name: &str) -> impl Iterator<Item = &'static AgentPlugin> {
    PLUGINS
        .get()
        .into_iter()
        .flat_map(|v| v.iter())
        .filter(move |p| p.persona_name == persona_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the persona lookup returns only matching plugins.
    ///
    /// Why: The ctrl loop relies on this filter to scope tool injection
    ///      to the active persona. A bug here would either leak sensitive
    ///      tools to the wrong persona or hide them from the right one.
    /// What: Tries to install a two-plugin list (best-effort — another
    ///       test in the same binary may have set the OnceLock first).
    ///       Regardless of who installs, asserts the lookup respects the
    ///       persona filter on whatever list ended up installed.
    /// Test: `cargo test -p trusty-agents agent_plugin_lookup_returns_matching_plugin`.
    #[test]
    fn agent_plugin_lookup_returns_matching_plugin() {
        // Best-effort install; another test may have won the OnceLock race.
        let _ = install_plugins(vec![
            AgentPlugin::new("alpha", vec![]),
            AgentPlugin::new("beta", vec![]),
        ]);
        // Whoever installed, the filter still has to be persona-scoped.
        let installed = PLUGINS.get().cloned().unwrap_or_default();
        let expected_alpha = installed
            .iter()
            .filter(|p| p.persona_name == "alpha")
            .count();
        assert_eq!(
            plugins_for_persona("alpha").count(),
            expected_alpha,
            "lookup must match exactly the alpha-targeted plugins"
        );
        assert_eq!(
            plugins_for_persona("nonexistent-persona-zzz").count(),
            0,
            "unknown persona must yield zero plugins"
        );
    }
}
