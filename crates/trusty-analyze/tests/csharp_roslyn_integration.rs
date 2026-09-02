//! `RoslynTool` end-to-end against a real `dotnet build` (#1008).
//!
//! Why: `RoslynTool::run`/`run_project` compile- and unit-test clean without
//! the .NET SDK (the SARIF *parser* is fixture-tested in
//! `core::tool_impls::csharp::sarif`), but nothing exercised the real
//! `dotnet build --no-restore --no-incremental -p:ErrorLog=...` spawn before
//! this file: no CI workflow installed the SDK, so a regression in that
//! invocation, the temp-file SARIF plumbing, or the file-matching filter
//! could land with every unit test green.
//! What: builds `testdata/csharp/Fixture.csproj` (a project with a
//! deliberate `CS0029` type-mismatch error) through the real `RoslynTool` and
//! asserts an `Error`-severity diagnostic comes back. When `dotnet` is
//! missing this fails loudly under `CI=true` instead of silently skipping,
//! so the coverage this issue restores cannot silently lapse again; a local,
//! non-CI run without the SDK skips with a printed reason.
//! Test: this file — `roslyn_tool_reports_real_build_error`.

use std::path::PathBuf;

use trusty_analyze::core::tool_impls::RoslynTool;
use trusty_analyze::core::tools::{Severity, StaticTool};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/csharp")
}

#[test]
fn roslyn_tool_reports_real_build_error() {
    if which::which("dotnet").is_err() {
        // #1008: a missing SDK under CI=true means the CI job forgot
        // actions/setup-dotnet — fail loudly rather than let this coverage
        // silently lapse again. Outside CI (a developer machine without the
        // SDK), skip with a printed reason instead.
        let running_in_ci = std::env::var("CI").as_deref() == Ok("true");
        assert!(
            !running_in_ci,
            "dotnet SDK not found on PATH under CI=true — the C# adapter's \
             real dispatch path (#1008) would go uncovered again; install \
             actions/setup-dotnet in the job that runs this test"
        );
        eprintln!(
            "roslyn_tool_reports_real_build_error: dotnet SDK not found on PATH; \
             skipping (local, non-CI run)"
        );
        return;
    }

    let cs_file = fixture_dir().join("Fixture.cs");
    assert!(
        cs_file.exists(),
        "fixture file missing: {}",
        cs_file.display()
    );

    let diags = RoslynTool
        .run_project(&[cs_file], None)
        .expect("run_project must not error");

    assert!(
        !diags.is_empty(),
        "expected at least one Roslyn diagnostic from the intentional \
         CS0029 type error in Fixture.cs, got none — the real dotnet build \
         path produced no SARIF output"
    );
    assert!(
        diags.iter().any(|d| d.severity == Severity::Error),
        "expected at least one Error-severity diagnostic, got: {diags:?}"
    );
}
