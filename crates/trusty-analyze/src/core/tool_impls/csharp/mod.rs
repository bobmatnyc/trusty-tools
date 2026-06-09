//! `RoslynTool` — C#/.NET diagnostics via the .NET SDK Roslyn analyzers.
//!
//! Why: Roslyn is the canonical C# compiler and analyzer host; it emits
//! SARIF via MSBuild's ErrorLog property, covering both compiler errors and
//! .NET analyzer rules (CA*, IDE*, CS*).
//! What: `mod.rs` contains `RoslynTool` and its `StaticTool` impl plus the
//! compiler-invocation helper (`build_project_diags`). SARIF parsing and
//! file-matching helpers live in the sibling `sarif` module to stay under the
//! 500-line cap.
//! Test: `parse_roslyn_sarif_extracts_result` in `sarif.rs`; tool-level tests
//! are in the `tests` block below.

pub mod sarif;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{build_tool_timeout, run_command_with_timeout};
use crate::core::tools::{StaticTool, ToolDiagnostic};
use sarif::{parse_roslyn_sarif, roslyn_file_matches};

/// C#/.NET static-analysis tool backed by the Roslyn compiler (dotnet SDK).
///
/// Why: provides IDE-grade diagnostics (CA rules, style enforcement, compiler
/// errors) without requiring a separate linter binary — only the .NET SDK.
/// What: shells out to `dotnet build` with SARIF output enabled, parses the
/// result, and filters to the target file.
/// Test: unit tests exercise the parser against a captured fixture; the
/// `run` method itself is not tested with a live dotnet invocation.
pub struct RoslynTool;

impl StaticTool for RoslynTool {
    fn name(&self) -> &str {
        "roslyn"
    }

    fn language(&self) -> &str {
        "csharp"
    }

    /// Returns true when the `dotnet` SDK binary is on `PATH`.
    ///
    /// Why: avoids wasted invocations on machines without the .NET SDK.
    /// What: delegates to `which::which("dotnet")`.
    /// Test: always evaluates at runtime; not directly unit-tested.
    fn is_available(&self) -> bool {
        which::which("dotnet").is_ok()
    }

    /// Run Roslyn analyzers on `file` via `dotnet build` and return findings.
    ///
    /// Why: MSBuild's `-p:ErrorLog` redirects diagnostics into SARIF; this is
    /// the only stable way to capture Roslyn analyzer output without a custom
    /// tool host.
    /// What: walks parent dirs for a `.csproj`, delegates to
    /// `build_project_diags` for the actual build, then filters the results
    /// to the single target `file`.
    /// Test: the SARIF parser is unit-tested independently; the full `run`
    /// path is side-effect-only (spawns dotnet) and is not invoked in unit
    /// tests.
    fn run(&self, file: &Path, _content: &str) -> Result<Vec<ToolDiagnostic>> {
        let dir = file.parent().unwrap_or_else(|| Path::new("."));

        let csproj = match find_csproj(dir) {
            Some(p) => p,
            None => return Ok(Vec::new()),
        };

        let all_diags = build_project_diags(&csproj);

        let target = file.to_string_lossy().into_owned();
        let filtered = all_diags
            .into_iter()
            .filter(|d| roslyn_file_matches(&d.file, &target))
            .collect();

        Ok(filtered)
    }

    /// Returns true: Roslyn must build the whole project to produce diagnostics.
    ///
    /// Why: `dotnet build` operates at the `.csproj` level; analyzing a single
    /// file in a scratch dir produces no findings because there is no project
    /// file. Marking this as project-scoped tells the dispatcher to call
    /// `run_project` and hand real on-disk paths.
    /// What: always returns `true`.
    /// Test: `roslyn_tool_is_project_scoped` in unit tests below.
    fn is_project_scoped(&self) -> bool {
        true
    }

    /// Run Roslyn against all C# files that share a `.csproj`, building each
    /// project once.
    ///
    /// Why: the dispatcher calls this instead of per-file `run()` when
    /// `is_project_scoped()` is true, so Roslyn only needs one `dotnet build`
    /// per distinct `.csproj` rather than one per file. This is the fix for
    /// issue #916 — previously files were analyzed in scratch dirs where no
    /// `.csproj` exists, yielding zero results.
    /// What: groups `files` by their enclosing `.csproj` (via `find_csproj`);
    /// for each distinct `.csproj` calls `build_project_diags` once; retains
    /// only diagnostics whose `.file` matches at least one of the input files
    /// for that project. Files with no resolvable `.csproj` are silently
    /// skipped.
    /// Test: `run_project_filters_diagnostics_to_requested_files` exercises
    /// the filtering logic without invoking dotnet.
    fn run_project(&self, files: &[PathBuf]) -> Result<Vec<ToolDiagnostic>> {
        let mut by_csproj: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for f in files {
            let dir = f.parent().unwrap_or_else(|| Path::new("."));
            if let Some(proj) = find_csproj(dir) {
                by_csproj.entry(proj).or_default().push(f.clone());
            } else {
                tracing::debug!(
                    "run_project: no .csproj found for {}; skipping",
                    f.display()
                );
            }
        }

        let mut out = Vec::new();
        for (csproj, proj_files) in &by_csproj {
            let all_diags = build_project_diags(csproj);
            for diag in all_diags {
                let matches_any = proj_files
                    .iter()
                    .any(|pf| roslyn_file_matches(&diag.file, &pf.to_string_lossy()));
                if matches_any {
                    out.push(diag);
                }
            }
        }

        Ok(out)
    }
}

/// Build a project by running `dotnet build` and return ALL diagnostics.
///
/// Why: both `run()` (single-file) and `run_project()` (multi-file) need the
/// same build+parse logic; extracting it here avoids duplication and lets each
/// caller apply its own filter on top.
/// What: creates a temp SARIF file, runs a best-effort restore followed by
/// `dotnet build --no-restore --no-incremental` with the analyzer flags,
/// reads the SARIF, and returns the unfiltered `Vec<ToolDiagnostic>`. Uses
/// `build_tool_timeout()` (default 300 s) instead of the 30 s file-tool cap.
/// Test: not directly tested (spawns dotnet); the SARIF parsing it delegates
/// to is covered by `parse_roslyn_sarif_extracts_result` in `sarif.rs`.
fn build_project_diags(csproj: &Path) -> Vec<ToolDiagnostic> {
    let dir = csproj.parent().unwrap_or_else(|| Path::new("."));

    let tmp = match tempfile::Builder::new().suffix(".sarif").tempfile() {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!("failed to create temp sarif file: {e}");
            return Vec::new();
        }
    };
    // Close write handle (retain deletion guard) so MSBuild can open the
    // file: on Windows an open NamedTempFile handle is exclusive.
    let sarif_path = tmp.into_temp_path();

    let tmp_path = sarif_path.to_string_lossy().into_owned();
    let csproj_path = csproj.to_string_lossy().into_owned();

    // Best-effort restore — ignore errors (offline or already restored).
    // Uses build_tool_timeout() (default 300 s) rather than the 30 s
    // per-file cap: on a cold NuGet cache (fresh CI runner or first
    // checkout), restore can take several minutes to download packages.
    // Timing out silently here causes the subsequent --no-restore build
    // to fail with missing packages and zero diagnostics, which is a
    // silent false-negative that makes Roslyn appear broken.
    let _ = run_command_with_timeout(
        "dotnet",
        &["restore", &csproj_path],
        dir,
        build_tool_timeout(),
    );

    // The %2C escapes the comma so MSBuild parses the -p: value correctly.
    let errorlog_arg = format!("-p:ErrorLog={}%2Cversion=2.1", tmp_path);
    // `--no-incremental` is REQUIRED: an up-to-date incremental build skips
    // the CoreCompile target, so the Roslyn analyzers never re-run and the
    // ErrorLog is left empty. Forcing a recompile is the only way to get
    // analyzer diagnostics on every invocation.
    let build_res = run_command_with_timeout(
        "dotnet",
        &[
            "build",
            &csproj_path,
            "--no-restore",
            "--no-incremental",
            &errorlog_arg,
            "-p:EnableNETAnalyzers=true",
            "-p:AnalysisLevel=latest-all",
            "-p:EnforceCodeStyleInBuild=true",
        ],
        dir,
        build_tool_timeout(),
    );

    if let Err(e) = build_res {
        // Spawn failure or timeout: log and fall through. A partial SARIF may
        // still have been written before the process died, and a non-zero
        // compiler exit (which run_command_with_timeout reports as Ok, not
        // Err) already produces a complete SARIF we want to parse.
        tracing::debug!("dotnet build invocation failed: {e}");
    }

    let sarif = match std::fs::read_to_string(&tmp_path) {
        Ok(s) => s,
        Err(e) => {
            // An empty SARIF (no findings) is the common clean case; a genuine
            // read error is rarer but worth a breadcrumb so a silent-empty
            // result is distinguishable from a genuinely clean build.
            tracing::debug!("failed to read SARIF output {tmp_path}: {e}");
            String::new()
        }
    };
    parse_roslyn_sarif(&sarif)
}

/// Walk parent directories upward from `start` to find the nearest `.csproj`.
///
/// Why: `dotnet build` requires a project file; a single file cannot be built
/// in isolation. The walk is bounded so a file with no project above it cannot
/// trigger a full filesystem-root scan or pick an unrelated project.
/// What: iterates ancestors of `start` (at most `MAX_ASCENT` levels), returns
/// the lexicographically-first `.csproj` in the nearest directory that has one;
/// stops ascending at a `.git` boundary so it never crosses into a parent repo.
/// Sorting makes the choice deterministic when a directory holds several
/// project files (e.g. a library and its adjacent test project) — `read_dir`
/// order is filesystem-dependent.
/// Test: not directly tested; exercised indirectly through `run`.
fn find_csproj(start: &Path) -> Option<PathBuf> {
    const MAX_ASCENT: usize = 24;
    let mut current = start;
    for _ in 0..MAX_ASCENT {
        if let Ok(entries) = std::fs::read_dir(current) {
            let mut csprojs: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".csproj"))
                .map(|e| e.path())
                .collect();
            if !csprojs.is_empty() {
                csprojs.sort();
                return csprojs.into_iter().next();
            }
        }
        if current.join(".git").exists() {
            return None;
        }
        match current.parent() {
            Some(p) => current = p,
            None => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::Severity;

    #[test]
    fn roslyn_tool_is_project_scoped() {
        // RoslynTool must self-identify as project-scoped so the dispatcher
        // calls run_project instead of the per-file run path.
        assert!(RoslynTool.is_project_scoped());
    }

    #[test]
    fn run_project_filters_diagnostics_to_requested_files() {
        // Simulate the filtering logic in run_project without invoking dotnet.
        // Given a synthetic Vec<ToolDiagnostic>, verify the file-match filter
        // keeps only diagnostics for the requested paths.
        let diags: Vec<ToolDiagnostic> = vec![
            ToolDiagnostic {
                tool: "roslyn".into(),
                file: "/abs/Proj/Foo.cs".into(),
                line: 1,
                col: 1,
                severity: Severity::Warning,
                code: Some("CA1".into()),
                message: "foo".into(),
            },
            ToolDiagnostic {
                tool: "roslyn".into(),
                file: "/abs/Proj/Bar.cs".into(),
                line: 2,
                col: 1,
                severity: Severity::Error,
                code: Some("CA2".into()),
                message: "bar".into(),
            },
            ToolDiagnostic {
                tool: "roslyn".into(),
                file: "/abs/Proj/Baz.cs".into(),
                line: 3,
                col: 1,
                severity: Severity::Hint,
                code: Some("CA3".into()),
                message: "baz".into(),
            },
        ];

        let requested: Vec<PathBuf> = vec![
            PathBuf::from("/abs/Proj/Foo.cs"),
            PathBuf::from("/abs/Proj/Bar.cs"),
        ];

        let kept: Vec<_> = diags
            .iter()
            .filter(|d| {
                requested
                    .iter()
                    .any(|pf| roslyn_file_matches(&d.file, &pf.to_string_lossy()))
            })
            .collect();

        assert_eq!(kept.len(), 2, "expected Foo.cs and Bar.cs diagnostics");
        let codes: Vec<_> = kept.iter().filter_map(|d| d.code.as_deref()).collect();
        assert!(codes.contains(&"CA1"), "Foo.cs diagnostic missing");
        assert!(codes.contains(&"CA2"), "Bar.cs diagnostic missing");
        assert!(!codes.contains(&"CA3"), "Baz.cs should be excluded");
    }
}
