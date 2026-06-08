//! `ToolRegistry` — lazy discovery and dispatch of external static tools.
//!
//! Why: external linters (clippy, ruff, ...) may or may not be installed on a
//! given machine. Probing every tool once at startup and indexing the
//! available ones by language lets callers run "whatever is available for
//! this language" without per-request `which` probes.
//!
//! What: `discover()` probes every known `StaticTool` and keeps the available
//! ones in a `DashMap<language, Vec<Arc<dyn StaticTool>>>`. `tools_for` and
//! `run_all` are the query surface. `global_registry()` exposes a process-wide
//! `OnceLock`-backed instance.
//!
//! Test: `discover_does_not_panic` and `run_all_unknown_language_is_empty`
//! exercise the registry without depending on any tool being installed.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;

use crate::core::tool_impls::{
    BiomeTool, ClangtidyTool, ClippyTool, DetektTool, PhpstanTool, PmdTool, RoslynTool,
    RubocopTool, RuffTool, StaticcheckTool, SwiftlintTool,
};
use crate::core::tools::{StaticTool, ToolDiagnostic};

/// Holds the set of available external static-analysis tools, indexed by the
/// language tag each one analyzes.
pub struct ToolRegistry {
    /// Language tag → list of available tools for that language.
    tools: DashMap<String, Vec<Arc<dyn StaticTool>>>,
}

impl ToolRegistry {
    /// Probe every known tool for availability and build a registry of the
    /// ones whose backing binary is on `PATH`.
    ///
    /// Why: a single startup probe avoids repeated `which` syscalls per
    /// request.
    /// What: instantiates every `StaticTool`, keeps those where
    /// `is_available()` is true, and buckets them by `language()`.
    /// Test: `discover_does_not_panic` ensures construction is total.
    pub fn discover() -> Self {
        let registry = ToolRegistry {
            tools: DashMap::new(),
        };

        let all_tools: Vec<Arc<dyn StaticTool>> = vec![
            Arc::new(ClippyTool),
            Arc::new(RuffTool),
            Arc::new(BiomeTool),
            Arc::new(StaticcheckTool),
            Arc::new(PmdTool),
            Arc::new(RubocopTool),
            Arc::new(PhpstanTool),
            Arc::new(SwiftlintTool),
            Arc::new(DetektTool),
            Arc::new(ClangtidyTool),
            Arc::new(RoslynTool),
        ];

        for tool in all_tools {
            if tool.is_available() {
                tracing::debug!(
                    tool = tool.name(),
                    language = tool.language(),
                    "static tool available"
                );
                registry
                    .tools
                    .entry(tool.language().to_string())
                    .or_default()
                    .push(tool);
            } else {
                tracing::debug!(tool = tool.name(), "static tool not available");
            }
        }

        registry
    }

    /// All available tools registered for `lang`. Empty if none.
    pub fn tools_for(&self, lang: &str) -> Vec<Arc<dyn StaticTool>> {
        self.tools
            .get(lang)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    /// All language tags that have at least one available tool.
    pub fn languages(&self) -> Vec<String> {
        self.tools.iter().map(|e| e.key().clone()).collect()
    }

    /// Run every available tool for `lang` against `file` and merge the
    /// diagnostics. A failure in one tool is logged and skipped — it does not
    /// abort the others.
    ///
    /// Why: callers want a single merged diagnostic list, with best-effort
    /// semantics so one broken tool cannot blank out the rest.
    /// What: iterates `tools_for(lang)`, calls `run`, concatenates `Ok`
    /// results, and logs `Err`s at warn level.
    /// Test: `run_all_unknown_language_is_empty` covers the no-tool path.
    pub fn run_all(
        &self,
        lang: &str,
        file: &Path,
        content: &str,
    ) -> anyhow::Result<Vec<ToolDiagnostic>> {
        let mut merged = Vec::new();
        for tool in self.tools_for(lang) {
            match tool.run(file, content) {
                Ok(diags) => merged.extend(diags),
                Err(e) => {
                    tracing::warn!(tool = tool.name(), "tool run failed: {e:#}");
                }
            }
        }
        Ok(merged)
    }

    /// Run a named subset of tools for `lang`. Unknown tool names are skipped.
    pub fn run_named(
        &self,
        lang: &str,
        names: &[String],
        file: &Path,
        content: &str,
    ) -> anyhow::Result<Vec<ToolDiagnostic>> {
        let mut merged = Vec::new();
        for tool in self.tools_for(lang) {
            if !names.iter().any(|n| n == tool.name()) {
                continue;
            }
            match tool.run(file, content) {
                Ok(diags) => merged.extend(diags),
                Err(e) => {
                    tracing::warn!(tool = tool.name(), "tool run failed: {e:#}");
                }
            }
        }
        Ok(merged)
    }
}

/// Process-wide registry, lazily discovered on first access.
static GLOBAL_REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();

/// Return the process-wide `ToolRegistry`, discovering tools on first call.
pub fn global_registry() -> &'static ToolRegistry {
    GLOBAL_REGISTRY.get_or_init(ToolRegistry::discover)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_does_not_panic() {
        let r = ToolRegistry::discover();
        // Languages list is a subset of the known tool languages — exact
        // contents depend on which binaries are installed on the host.
        for lang in r.languages() {
            assert!(!r.tools_for(&lang).is_empty());
        }
    }

    #[test]
    fn run_all_unknown_language_is_empty() {
        let r = ToolRegistry::discover();
        let diags = r
            .run_all("klingon", Path::new("foo.kl"), "")
            .expect("run_all should not fail");
        assert!(diags.is_empty());
    }

    #[test]
    fn global_registry_is_stable() {
        let a = global_registry() as *const ToolRegistry;
        let b = global_registry() as *const ToolRegistry;
        assert_eq!(a, b, "global registry must be a singleton");
    }
}
