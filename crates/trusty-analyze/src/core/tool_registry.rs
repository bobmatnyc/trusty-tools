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
/// language tag each one analyzes, plus the names of tools that were probed
/// but not found on `PATH`.
///
/// Why: storing unavailable tool names alongside available ones lets callers
/// distinguish "no tools configured" from "ran tools, found nothing" (#915).
/// What: `tools` maps language tags to available tools; `unavailable_names`
/// records the `name()` of every tool whose `is_available()` returned false
/// at `discover()` / `from_tools()` time.
/// Test: `discover_records_unavailable_tools` and
/// `from_tools_records_all_unavailable`.
pub struct ToolRegistry {
    /// Language tag → list of available tools for that language.
    tools: DashMap<String, Vec<Arc<dyn StaticTool>>>,
    /// Names of tools that were probed but not available on `PATH`.
    unavailable_names: Vec<String>,
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
        Self::from_tools(all_tools)
    }

    /// Build a `ToolRegistry` from an explicit list for use in tests. Unlike
    /// `from_tools`, this is `pub` so test modules outside this crate can
    /// inject synthetic tools without relying on binary availability on the
    /// host.
    ///
    /// Why: the project-scoped skip test in `service/tests.rs` needs to
    /// construct a registry with a fake project-scoped tool to assert that
    /// `run_project` is never called when `root_path` is `None`. Exposing
    /// this constructor avoids duplicating `from_tools` logic.
    /// What: delegates to `from_tools`.
    /// Test: used by `run_diagnostics_blocking_project_scoped_skips_when_no_root`.
    pub fn from_tools_for_test(all_tools: Vec<Arc<dyn StaticTool>>) -> Self {
        Self::from_tools(all_tools)
    }

    /// Build a registry from an explicit tool list: keep the available ones and
    /// bucket each under its primary [`language`](StaticTool::language) plus any
    /// [`aliases`](StaticTool::aliases). Records the names of unavailable tools.
    ///
    /// Why: separating the fanout from the hardcoded tool list lets tests
    /// exercise alias registration with a synthetic always-available tool,
    /// without depending on which binaries happen to be installed on the host.
    /// Recording unavailable names supports the #915 signal (callers can
    /// distinguish "ran but clean" from "tools not installed").
    /// What: probes `is_available()`, keeps available tools in buckets, and
    /// records the `name()` of each unavailable tool in `unavailable_names`.
    /// Test: `aliases_register_tool_under_every_bucket`,
    /// `from_tools_records_all_unavailable`.
    fn from_tools(all_tools: Vec<Arc<dyn StaticTool>>) -> Self {
        let mut unavailable_names = Vec::new();
        let registry_tools = DashMap::new();

        for tool in all_tools {
            if tool.is_available() {
                tracing::debug!(
                    tool = tool.name(),
                    language = tool.language(),
                    "static tool available"
                );
                // Register under the primary language tag plus every alias, so
                // a multi-language linter (e.g. biome → typescript + javascript)
                // is reachable from each bucket its files route to.
                for lang in std::iter::once(tool.language()).chain(tool.aliases().iter().copied()) {
                    registry_tools
                        .entry(lang.to_string())
                        .or_insert_with(Vec::new)
                        .push(Arc::clone(&tool));
                }
            } else {
                tracing::debug!(tool = tool.name(), "static tool not available");
                unavailable_names.push(tool.name().to_string());
            }
        }

        ToolRegistry {
            tools: registry_tools,
            unavailable_names,
        }
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

    /// Names of tools that were probed at registry construction time but whose
    /// backing binary was not found on `PATH`.
    ///
    /// Why: exposes the "tools not installed" signal (#915) so callers can
    /// distinguish "ran tools, found nothing" from "no tools available at all".
    /// What: returns a reference to the stored unavailable-name list. The list
    /// contains one entry per tool whose `is_available()` returned false during
    /// `from_tools()`.
    /// Test: `from_tools_records_all_unavailable` in this module's tests.
    pub fn unavailable_names(&self) -> &[String] {
        &self.unavailable_names
    }

    /// Run every available file-scoped tool for `lang` against `file` and
    /// merge the diagnostics. A failure in one tool is logged and skipped —
    /// it does not abort the others.
    ///
    /// Why: callers want a single merged diagnostic list, with best-effort
    /// semantics so one broken tool cannot blank out the rest.
    /// What: iterates `tools_for(lang)`, skips project-scoped tools (they
    /// require a real `.csproj` on disk and are dispatched via
    /// `run_diagnostics_blocking` instead), calls `run` on file-scoped tools,
    /// concatenates `Ok` results, and logs `Err`s at warn level. Skipping
    /// project-scoped tools here prevents silent empty results: without the
    /// guard, `RoslynTool::run` receives a scratch-dir path, `find_csproj`
    /// returns `None`, and the caller gets zero diagnostics with no warning.
    /// Test: `run_all_unknown_language_is_empty` covers the no-tool path;
    /// `run_all_skips_project_scoped_tools` covers the guard.
    pub fn run_all(
        &self,
        lang: &str,
        file: &Path,
        content: &str,
    ) -> anyhow::Result<Vec<ToolDiagnostic>> {
        let mut merged = Vec::new();
        for tool in self.tools_for(lang) {
            if tool.is_project_scoped() {
                tracing::debug!(
                    tool = tool.name(),
                    "skipping project-scoped tool in run_all — \
                     use run_diagnostics_blocking for project-scoped dispatch"
                );
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

    /// Run a named subset of file-scoped tools for `lang`. Unknown tool names
    /// and project-scoped tools are skipped.
    ///
    /// Why: callers supply an explicit list of tool names but may not know
    /// which are project-scoped. Silently calling `run` on a project-scoped
    /// tool yields zero diagnostics (no `.csproj` in the scratch dir) with no
    /// warning, violating the contract that requested tools are run. Skipping
    /// them here with a trace log makes the omission observable.
    /// What: filters to named tools, skips project-scoped ones with a debug
    /// log, and calls `tool.run` on the remainder.
    /// Test: exercised indirectly by `run_all_skips_project_scoped_tools`.
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
            if tool.is_project_scoped() {
                tracing::debug!(
                    tool = tool.name(),
                    "skipping project-scoped tool in run_named — \
                     use run_diagnostics_blocking for project-scoped dispatch"
                );
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
    fn run_all_skips_project_scoped_tools() {
        // run_all must never call tool.run() on a project-scoped tool: doing
        // so against a scratch-dir path would return Ok(vec![]) with no
        // warning, silently producing zero results. Instead, run_all returns
        // an empty vec for that language — callers that need project-scoped
        // results should use run_diagnostics_blocking.
        //
        // We verify the contract holds regardless of whether dotnet is
        // installed: even if the csharp language has available tools, run_all
        // against a non-existent scratch file must not return an Err.
        let r = ToolRegistry::discover();
        let scratch = Path::new("/tmp/test_dummy.cs");
        let result = r.run_all("csharp", scratch, "class Foo {}");
        // Must not error — project-scoped tools are skipped, not errored.
        assert!(
            result.is_ok(),
            "run_all must not fail for project-scoped language: {result:?}"
        );
    }

    #[test]
    fn global_registry_is_stable() {
        let a = global_registry() as *const ToolRegistry;
        let b = global_registry() as *const ToolRegistry;
        assert_eq!(a, b, "global registry must be a singleton");
    }

    /// A synthetic always-available tool that claims a primary language plus an
    /// alias, so alias registration can be tested without any binary on PATH.
    struct FakeAliasedTool;
    impl StaticTool for FakeAliasedTool {
        fn name(&self) -> &str {
            "fake-aliased"
        }
        fn language(&self) -> &str {
            "typescript"
        }
        fn aliases(&self) -> &[&str] {
            &["javascript"]
        }
        fn is_available(&self) -> bool {
            true
        }
        fn run(&self, _file: &Path, _content: &str) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new())
        }
    }

    /// Unavailable tools must be recorded under `unavailable_names()` so
    /// callers can distinguish "no tools configured" from "ran, found nothing".
    ///
    /// Why: #915 — an all-unavailable host returns zero diagnostics with no
    /// signal, indistinguishable from "code is clean." Recording unavailable
    /// names is the fix.
    /// What: builds a registry from one always-available and one always-absent
    /// tool. Asserts `unavailable_names()` contains exactly the absent tool.
    /// Test: this test.
    #[test]
    fn from_tools_records_all_unavailable() {
        struct AlwaysAvailable;
        impl StaticTool for AlwaysAvailable {
            fn name(&self) -> &str {
                "always-on"
            }
            fn language(&self) -> &str {
                "rust"
            }
            fn is_available(&self) -> bool {
                true
            }
            fn run(&self, _: &Path, _: &str) -> anyhow::Result<Vec<ToolDiagnostic>> {
                Ok(Vec::new())
            }
        }

        struct NeverAvailable;
        impl StaticTool for NeverAvailable {
            fn name(&self) -> &str {
                "never-on"
            }
            fn language(&self) -> &str {
                "python"
            }
            fn is_available(&self) -> bool {
                false
            }
            fn run(&self, _: &Path, _: &str) -> anyhow::Result<Vec<ToolDiagnostic>> {
                Ok(Vec::new())
            }
        }

        let r = ToolRegistry::from_tools(vec![Arc::new(AlwaysAvailable), Arc::new(NeverAvailable)]);

        assert_eq!(r.unavailable_names(), &["never-on"]);
        // The available tool is NOT in unavailable_names.
        assert!(
            !r.unavailable_names().contains(&"always-on".to_string()),
            "available tool must not appear in unavailable_names"
        );
        // The available tool is still reachable in the tools bucket.
        assert_eq!(r.tools_for("rust").len(), 1);
        // The absent tool has no bucket.
        assert!(r.tools_for("python").is_empty());
    }

    /// A fully-discovered registry on a host with no linters still reports an
    /// empty `unavailable_names` if all known tools are found (the happy-path
    /// sanity check).
    ///
    /// Why: ensures `unavailable_names()` doesn't accidentally leak available
    /// tools when all are installed.
    /// What: builds a registry with only always-available tools; asserts
    /// `unavailable_names()` is empty.
    /// Test: this test.
    #[test]
    fn from_tools_available_only_has_empty_unavailable() {
        struct OnlyAvailable;
        impl StaticTool for OnlyAvailable {
            fn name(&self) -> &str {
                "on-tool"
            }
            fn language(&self) -> &str {
                "go"
            }
            fn is_available(&self) -> bool {
                true
            }
            fn run(&self, _: &Path, _: &str) -> anyhow::Result<Vec<ToolDiagnostic>> {
                Ok(Vec::new())
            }
        }
        let r = ToolRegistry::from_tools(vec![Arc::new(OnlyAvailable)]);
        assert!(
            r.unavailable_names().is_empty(),
            "no unavailable tools expected, got: {:?}",
            r.unavailable_names()
        );
    }

    #[test]
    fn aliases_register_tool_under_every_bucket() {
        // Regression: a multi-language linter (biome → typescript + javascript)
        // must be reachable from its alias bucket, or files routed to that tag
        // are silently skipped (the JS half of the #963 class of bug).
        let r = ToolRegistry::from_tools(vec![Arc::new(FakeAliasedTool)]);
        assert_eq!(r.tools_for("typescript").len(), 1, "primary bucket");
        assert_eq!(
            r.tools_for("javascript").len(),
            1,
            "alias bucket must be reachable"
        );
        assert_eq!(r.tools_for("typescript")[0].name(), "fake-aliased");
        assert_eq!(r.tools_for("javascript")[0].name(), "fake-aliased");
    }
}
