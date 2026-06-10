//! Tests for `diagnostics_dispatch` — extracted to keep the production module
//! under the 500-line cap (#610).
//!
//! Why: the dispatch module grew with #915 (tools_run / tools_unavailable
//! signal tests) to the point where inline tests would push it past 500 lines.
//! Extracting here follows the `analysis.rs` / `analysis_tests.rs` pattern.
//! What: exercises `run_diagnostics_blocking_with_registry` via fake tools that
//! do not require any binary on PATH.
//! Test: `cargo test -p trusty-analyze` runs all tests in this module.

use std::collections::HashMap;

use super::{abs_to_rel, run_diagnostics_blocking_with_registry};

/// Why: two files with identical basenames in different index directories
/// must each produce diagnostics independently; the basename-collision bug
/// (writing `scratch/main.rs` twice) silently drops the first file's
/// diagnostics. This test FAILS against `scratch.path().join(&name)` (the
/// old code) and PASSES after the per-file `scratch/<idx>/name` fix.
///
/// What: injects a `FakeFileScopedTool` that records every `(path, content)`
/// it receives. Passes two same-basename Rust files. Asserts: (a) the fake
/// tool was called twice, (b) the two paths are distinct, (c) neither
/// rel_file mapping is lost (both appear in the output), and (d) the tool
/// name appears in `tools_run`.
///
/// Test: this test itself. Does not require any external linter.
#[test]
fn run_diagnostics_blocking_with_registry_two_files_same_basename() {
    use crate::core::tool_registry::ToolRegistry;
    use crate::core::tools::{StaticTool, ToolDiagnostic};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeFileScopedTool {
        calls: Arc<Mutex<Vec<(PathBuf, String)>>>,
    }
    impl StaticTool for FakeFileScopedTool {
        fn name(&self) -> &str {
            "fake-file-scoped"
        }
        fn language(&self) -> &str {
            "rust"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn is_project_scoped(&self) -> bool {
            false
        }
        fn run(&self, file: &Path, content: &str) -> anyhow::Result<Vec<ToolDiagnostic>> {
            self.calls
                .lock()
                .unwrap()
                .push((file.to_path_buf(), content.to_string()));
            Ok(vec![ToolDiagnostic {
                file: file.to_string_lossy().into_owned(),
                line: 1,
                col: 1,
                message: "fake".into(),
                severity: crate::core::tools::Severity::Warning,
                tool: "fake-file-scoped".into(),
                code: None,
            }])
        }
        fn run_project(&self, _files: &[PathBuf]) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new())
        }
    }

    let calls = Arc::new(Mutex::new(Vec::<(PathBuf, String)>::new()));
    let tool = FakeFileScopedTool {
        calls: Arc::clone(&calls),
    };
    let registry = ToolRegistry::from_tools_for_test(vec![Arc::new(tool)]);

    let mut by_file = HashMap::new();
    by_file.insert("src/a/main.rs".to_string(), "fn a() {}".to_string());
    by_file.insert("src/b/main.rs".to_string(), "fn b() {}".to_string());

    let report = run_diagnostics_blocking_with_registry(by_file, None, None, None, &registry);

    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded.len(),
        2,
        "expected 2 tool invocations (one per file), got {}; \
         basename collision likely dropped one",
        recorded.len()
    );
    let path0 = &recorded[0].0;
    let path1 = &recorded[1].0;
    assert_ne!(
        path0, path1,
        "the two files were written to the same scratch path ({path0:?}); \
         per-file subdir isolation is broken"
    );
    assert_eq!(
        report.diagnostics.len(),
        2,
        "expected 2 diagnostics in output (one per file), got {}; \
         one file's diagnostics were silently dropped",
        report.diagnostics.len()
    );
    let files: Vec<&str> = report.diagnostics.iter().map(|d| d.file.as_str()).collect();
    assert!(
        files.contains(&"src/a/main.rs"),
        "src/a/main.rs missing from output: {files:?}"
    );
    assert!(
        files.contains(&"src/b/main.rs"),
        "src/b/main.rs missing from output: {files:?}"
    );
    assert!(
        report.tools_run.contains(&"fake-file-scoped".to_string()),
        "expected fake-file-scoped in tools_run: {:?}",
        report.tools_run
    );
}

/// Why: #915 — when no tool binary is on PATH, the report must list those
/// tools under `tools_unavailable`, not silently return empty diagnostics
/// that look identical to "code is clean."
/// What: builds a registry with one unavailable tool; runs dispatch; asserts
/// `tools_unavailable` contains the tool name and `tools_run` is empty.
/// Test: this test.
#[test]
fn report_marks_unavailable_tool() {
    use crate::core::tool_registry::ToolRegistry;
    use crate::core::tools::{StaticTool, ToolDiagnostic};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct NeverAvailableTool;
    impl StaticTool for NeverAvailableTool {
        fn name(&self) -> &str {
            "absent-linter"
        }
        fn language(&self) -> &str {
            "rust"
        }
        fn is_available(&self) -> bool {
            false
        }
        fn run(&self, _: &Path, _: &str) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new())
        }
        fn run_project(&self, _: &[PathBuf]) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new())
        }
    }

    let registry = ToolRegistry::from_tools_for_test(vec![Arc::new(NeverAvailableTool)]);
    let mut by_file = HashMap::new();
    by_file.insert("src/main.rs".to_string(), "fn main() {}".to_string());
    let report = run_diagnostics_blocking_with_registry(by_file, None, None, None, &registry);

    assert!(report.diagnostics.is_empty(), "no diagnostics expected");
    assert!(report.tools_run.is_empty(), "no tools should run");
    assert!(
        report
            .tools_unavailable
            .contains(&"absent-linter".to_string()),
        "absent-linter must appear in tools_unavailable: {:?}",
        report.tools_unavailable
    );
}

/// Why: a genuinely clean run (tool available, no findings) must show the
/// tool in `tools_run` and an empty `tools_unavailable`, so callers can
/// tell the difference from #915's "no tools installed" scenario.
/// What: builds a registry with a no-findings fake tool; asserts `tools_run`
/// contains the tool name and `tools_unavailable` is empty.
/// Test: this test.
#[test]
fn report_clean_run_populates_tools_run() {
    use crate::core::tool_registry::ToolRegistry;
    use crate::core::tools::{StaticTool, ToolDiagnostic};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct CleanTool;
    impl StaticTool for CleanTool {
        fn name(&self) -> &str {
            "clean-linter"
        }
        fn language(&self) -> &str {
            "python"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn run(&self, _: &Path, _: &str) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new()) // no findings
        }
        fn run_project(&self, _: &[PathBuf]) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new())
        }
    }

    let registry = ToolRegistry::from_tools_for_test(vec![Arc::new(CleanTool)]);
    let mut by_file = HashMap::new();
    by_file.insert("app.py".to_string(), "x = 1".to_string());
    let report = run_diagnostics_blocking_with_registry(by_file, None, None, None, &registry);

    assert!(report.diagnostics.is_empty(), "no findings expected");
    assert!(
        report.tools_unavailable.is_empty(),
        "tools_unavailable must be empty when tool is installed: {:?}",
        report.tools_unavailable
    );
    assert!(
        report.tools_run.contains(&"clean-linter".to_string()),
        "clean-linter must appear in tools_run: {:?}",
        report.tools_run
    );
}

#[test]
fn abs_to_rel_exact_match() {
    let pairs = vec![(
        "src/Foo.cs".to_string(),
        "/home/user/proj/src/Foo.cs".to_string(),
    )];
    assert_eq!(
        abs_to_rel("/home/user/proj/src/Foo.cs", &pairs),
        Some("src/Foo.cs")
    );
}

#[test]
fn abs_to_rel_suffix_match_absolute_real() {
    let pairs = vec![(
        "src/Bar.cs".to_string(),
        "/home/user/proj/src/Bar.cs".to_string(),
    )];
    assert_eq!(
        abs_to_rel("/symlink-root/home/user/proj/src/Bar.cs", &pairs),
        Some("src/Bar.cs"),
    );
    assert_eq!(abs_to_rel("/completely/different/Qux.cs", &pairs), None);
}

#[test]
fn abs_to_rel_no_match_returns_none() {
    let pairs = vec![(
        "src/Baz.cs".to_string(),
        "/home/user/proj/src/Baz.cs".to_string(),
    )];
    assert_eq!(abs_to_rel("/completely/different/path.cs", &pairs), None);
}

#[test]
fn abs_to_rel_rel_exact_match() {
    let pairs = vec![(
        "src/Qux.cs".to_string(),
        "/home/user/proj/src/Qux.cs".to_string(),
    )];
    assert_eq!(abs_to_rel("src/Qux.cs", &pairs), Some("src/Qux.cs"));
}
