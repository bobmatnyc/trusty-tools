//! `StaticTool` plugin trait and `ToolDiagnostic` types.
//!
//! Why: trusty-analyzer's tree-sitter adapters give a uniform structural
//! baseline, but real-world linters (clippy, ruff, biome, ...) catch issues a
//! grammar walk never will. This module defines the minimal surface every
//! external static-analysis tool exposes so the orchestration layer can probe,
//! select, and run them without knowing which binary backs each one.
//!
//! What: a `Severity` enum, a `ToolDiagnostic` record (one finding), and the
//! `StaticTool` trait (`name`/`language`/`is_available`/`run`).
//!
//! Test: `severity_serializes_lowercase` pins the wire format; the per-tool
//! impl tests in `tool_impls` exercise `run` against captured fixtures.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Severity of a single tool diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A definite error — code is wrong or will not compile.
    Error,
    /// A likely problem worth fixing.
    Warning,
    /// Informational note.
    Info,
    /// A low-priority hint or style suggestion.
    Hint,
}

/// One finding emitted by a `StaticTool`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDiagnostic {
    /// Tool that produced this diagnostic, e.g. `"clippy"`.
    pub tool: String,
    /// Repo-relative or absolute file path the finding applies to.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub col: u32,
    /// Severity bucket.
    pub severity: Severity,
    /// Tool-specific rule code, e.g. `"clippy::needless_return"`.
    pub code: Option<String>,
    /// Human-readable description of the finding.
    pub message: String,
}

/// Result envelope for the `run_diagnostics` endpoint and `run_diagnostics_blocking` function.
///
/// Why: callers cannot distinguish "no linters installed" from "code is clean"
/// when the result is only `Vec<ToolDiagnostic>`. Recording which tools were
/// actually invoked vs. which were absent from `PATH` lets callers surface
/// "tools unavailable" as a distinct signal rather than silently empty results.
/// (#915)
///
/// What: carries the full diagnostics list together with two disjoint name
/// lists — `tools_run` (invoked successfully) and `tools_unavailable` (known
/// but not on `PATH`). Both lists contain deduplicated tool names. A fully
/// clean run has `tools_run` non-empty and `tools_unavailable` empty; a fully
/// unconfigured host has `tools_run` empty and `tools_unavailable` non-empty.
///
/// Test: `diagnostics_report_marks_unavailable_tool` and
/// `diagnostics_report_clean_run` in `diagnostics_dispatch` tests.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticsReport {
    /// Tool names that were discovered on `PATH` and invoked during this run.
    pub tools_run: Vec<String>,
    /// Tool names that are known to the registry but whose binary was not
    /// found on `PATH` at daemon startup and therefore could not be invoked.
    pub tools_unavailable: Vec<String>,
    /// All findings emitted by the tools that ran.
    pub diagnostics: Vec<ToolDiagnostic>,
    /// `Some` when the per-request deadline stopped the dispatch early, in
    /// which case `diagnostics` is a PARTIAL result. `None` means the dispatch
    /// ran to completion. See [`DeadlineCutoff`] (#6018).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<DeadlineCutoff>,
}

/// What a per-request deadline cut off, so a partial result never reads as a
/// complete one.
///
/// Why: stopping the dispatch at a deadline is a new failure branch, and a
/// truncated diagnostics list is indistinguishable from a clean run unless the
/// response says which work never happened. A caller that sees `files_skipped:
/// 3900` knows to narrow the request (`?language=`, `?tools=`) rather than
/// concluding the corpus is clean (#6018).
/// What: records the deadline that expired plus the exact work not done — files
/// whose file-scoped tools never ran, and project-scoped tools never invoked.
/// `files_analyzed` is the completed half of the same count.
/// Test: `dispatch_stops_at_deadline_and_reports_cutoff` and
/// `dispatch_without_deadline_reports_no_cutoff` in
/// `service/diagnostics_dispatch_tests.rs`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeadlineCutoff {
    /// Wall-clock budget the dispatch was given, in seconds.
    pub deadline_secs: u64,
    /// Files whose file-scoped tool runs completed before the deadline.
    pub files_analyzed: usize,
    /// Files never reached because the deadline expired first.
    pub files_skipped: usize,
    /// Names of tools that were selected but never invoked (or invoked for
    /// only part of the corpus) because the deadline expired.
    pub tools_skipped: Vec<String>,
}

impl DiagnosticsReport {
    /// Construct an empty report with no tools run and no diagnostics.
    ///
    /// Why: callers that encounter an early-exit condition (empty corpus,
    /// scratch-dir failure) need a well-formed empty report rather than
    /// having to construct one manually.
    /// What: returns a `DiagnosticsReport` with all vectors empty.
    /// Test: exercised implicitly by all failure-path tests.
    pub fn empty() -> Self {
        Self {
            tools_run: Vec::new(),
            tools_unavailable: Vec::new(),
            diagnostics: Vec::new(),
            cutoff: None,
        }
    }
}

/// Plugin interface for an external static-analysis tool.
///
/// Implementations shell out to a binary (clippy, ruff, ...), parse its
/// output, and normalize findings into `ToolDiagnostic`s. Implementations must
/// never panic: a missing binary, non-zero exit, or malformed output should
/// surface as `Ok(vec![])` or an `Err`, never an `unwrap`.
pub trait StaticTool: Send + Sync {
    /// Stable tool identifier, e.g. `"clippy"`.
    fn name(&self) -> &str;

    /// Language tag this tool analyzes, matching `LanguageDetector` tags
    /// (`"rust"`, `"python"`, `"typescript"`, ...).
    fn language(&self) -> &str;

    /// Additional language buckets this tool should also be registered under,
    /// beyond its primary [`language`](Self::language).
    ///
    /// Why: a single binary often lints several `LanguageDetector` tags — e.g.
    /// biome lints both `"typescript"` and `"javascript"`. Without this, files
    /// routed to the secondary tag find an empty bucket and are silently
    /// skipped (the JS half of the #963 class of bug). Defaults to no aliases.
    /// What: each returned tag gets its own registry entry pointing at this
    /// tool, in addition to `language()`.
    /// Test: `aliases_register_tool_under_every_bucket` and
    /// `biome_covers_typescript_and_javascript`.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Cheap availability probe — confirms the backing binary is on `PATH`.
    fn is_available(&self) -> bool;

    /// Run the tool against `file` (whose content is `content`) and return
    /// normalized diagnostics. Returns `Ok(vec![])` when the tool runs cleanly
    /// with no findings; returns `Err` only for unexpected failures.
    fn run(&self, file: &Path, content: &str) -> anyhow::Result<Vec<ToolDiagnostic>>;

    /// Returns true when this tool needs to build the whole compilation unit
    /// (project / solution / package) rather than analyze files in isolation.
    ///
    /// Why: some tools (e.g. `RoslynTool`) can only produce meaningful output
    /// by invoking the compiler with the full project graph. Dispatching them
    /// file-by-file in a scratch dir yields nothing because the `.csproj` is
    /// absent. The dispatcher calls `run_project` for these tools instead of
    /// the per-file `run` path.
    /// What: defaults to `false`; override to `true` in tools that require
    /// a real project tree (currently only `RoslynTool`).
    /// Test: `RoslynTool::is_project_scoped` test in `tool_impls/csharp.rs`.
    fn is_project_scoped(&self) -> bool {
        false
    }

    /// Run the tool across a set of real on-disk files, building the project
    /// once and returning diagnostics for all of them.
    ///
    /// Why: project-scoped tools build the whole compilation unit once and
    /// filter to the provided files, so dispatch must hand them real on-disk
    /// paths and call `run_project` (not per-file `run()`). The default
    /// implementation falls back to calling `run` per file so the other ten
    /// tools are unaffected.
    ///
    /// `deadline` is the wall-clock instant the whole request must be done by.
    /// An implementation MUST cap each subprocess it spawns at the remaining
    /// budget and MUST stop starting new ones once the instant has passed
    /// (#6018). Without it a build-class tool ignores the request deadline
    /// entirely: `spawn_blocking` cannot be cancelled, so a `cargo clippy` or
    /// `dotnet build` capped only by `build_tool_timeout` (300 s each, one per
    /// project root) keeps running long after the client received its 504, and
    /// a client retry stacks a second build behind the first on the same
    /// toolchain lock. `None` means no budget.
    ///
    /// What: default iterates `files`, checks `deadline` before each, calls
    /// `self.run(f, "")`, and merges the results. Per-file errors are logged at
    /// warn level and skipped (log-and-continue) so one failing file does not
    /// abort the rest — matching the dispatcher's own behavior for file-scoped
    /// tools. Each `run` is already capped at `TOOL_TIMEOUT` (30 s), so the
    /// default only needs the between-files check, not a per-spawn cap.
    /// Test: `ClippyTool`'s override is proven by
    /// `run_project_stops_between_roots_when_budget_expires` and
    /// `run_project_caps_each_invocation_to_the_remaining_budget`;
    /// `RoslynTool`'s is in `tool_impls/csharp/mod.rs`.
    fn run_project(
        &self,
        files: &[PathBuf],
        deadline: Option<Instant>,
    ) -> anyhow::Result<Vec<ToolDiagnostic>> {
        let mut out = Vec::new();
        for f in files {
            // #6018: never start another subprocess past the request deadline.
            if deadline.is_some_and(|d| Instant::now() >= d) {
                tracing::debug!(
                    tool = self.name(),
                    "run_project default: deadline passed, skipping remaining files"
                );
                break;
            }
            match self.run(f, "") {
                Ok(diags) => out.extend(diags),
                Err(e) => {
                    tracing::warn!(
                        tool = self.name(),
                        file = %f.display(),
                        "run_project default: per-file run failed, skipping: {e:#}"
                    );
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Severity::Error).expect("serialize"),
            "\"error\""
        );
        assert_eq!(
            serde_json::to_string(&Severity::Hint).expect("serialize"),
            "\"hint\""
        );
    }

    #[test]
    fn diagnostic_round_trips() {
        let d = ToolDiagnostic {
            tool: "clippy".into(),
            file: "src/main.rs".into(),
            line: 12,
            col: 4,
            severity: Severity::Warning,
            code: Some("clippy::needless_return".into()),
            message: "unneeded return statement".into(),
        };
        let json = serde_json::to_string(&d).expect("serialize");
        let back: ToolDiagnostic = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.tool, "clippy");
        assert_eq!(back.line, 12);
        assert_eq!(back.severity, Severity::Warning);
    }
}
