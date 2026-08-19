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
        fn run_project(
            &self,
            _files: &[PathBuf],
            _deadline: Option<std::time::Instant>,
        ) -> anyhow::Result<Vec<ToolDiagnostic>> {
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

    let report = run_diagnostics_blocking_with_registry(by_file, None, None, None, None, &registry);

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
        fn run_project(
            &self,
            _: &[PathBuf],
            _: Option<std::time::Instant>,
        ) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new())
        }
    }

    let registry = ToolRegistry::from_tools_for_test(vec![Arc::new(NeverAvailableTool)]);
    let mut by_file = HashMap::new();
    by_file.insert("src/main.rs".to_string(), "fn main() {}".to_string());
    let report = run_diagnostics_blocking_with_registry(by_file, None, None, None, None, &registry);

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
        fn run_project(
            &self,
            _: &[PathBuf],
            _: Option<std::time::Instant>,
        ) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new())
        }
    }

    let registry = ToolRegistry::from_tools_for_test(vec![Arc::new(CleanTool)]);
    let mut by_file = HashMap::new();
    by_file.insert("app.py".to_string(), "x = 1".to_string());
    let report = run_diagnostics_blocking_with_registry(by_file, None, None, None, None, &registry);

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

/// A file-scoped tool that sleeps `delay` on every invocation and counts calls.
///
/// Why: the deadline tests need dispatch work that is slow and observable
/// without installing any linter.
struct SlowTool {
    delay: std::time::Duration,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::core::tools::StaticTool for SlowTool {
    fn name(&self) -> &str {
        "slow-linter"
    }
    fn language(&self) -> &str {
        "rust"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn run(
        &self,
        _f: &std::path::Path,
        _c: &str,
    ) -> anyhow::Result<Vec<crate::core::tools::ToolDiagnostic>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(self.delay);
        Ok(Vec::new())
    }
}

/// Why: #6018 — before the per-request deadline, the dispatch looped every
/// unique file spawning one subprocess per file-scoped tool with nothing able
/// to stop it. A 4097-file index ran past ten minutes and the client got a
/// transport-level abandon: zero bytes, no status line. The fix must stop the
/// fan-out mid-corpus AND say what it skipped, because a silently truncated
/// diagnostics list is indistinguishable from a clean corpus.
///
/// What: registers a fake tool that sleeps 40 ms per file, hands the dispatch
/// 40 files (1.6 s of work) and a 120 ms deadline. Asserts (a) the call
/// returns in well under the unbounded 1.6 s, (b) fewer than 40 files were
/// analyzed, (c) `cutoff` is `Some` — the report admits it is partial —
/// (d) `files_analyzed + files_skipped` accounts for every file, and (e)
/// `tools_skipped` names the tool that did not finish.
///
/// Test: this test. It cannot pass against the pre-#6018 dispatch: that
/// signature has no `deadline` parameter and `DiagnosticsReport` has no
/// `cutoff` field, and the unbounded loop would run all 40 files.
#[test]
fn dispatch_stops_at_deadline_and_reports_cutoff() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const FILES: usize = 40;
    const PER_FILE: Duration = Duration::from_millis(40);
    const BUDGET: Duration = Duration::from_millis(120);

    let calls = Arc::new(AtomicUsize::new(0));
    let registry =
        crate::core::tool_registry::ToolRegistry::from_tools_for_test(vec![Arc::new(SlowTool {
            delay: PER_FILE,
            calls: Arc::clone(&calls),
        })]);

    let by_file: HashMap<String, String> = (0..FILES)
        .map(|i| (format!("src/f{i}.rs"), "fn f() {}".to_string()))
        .collect();

    let started = Instant::now();
    let report = run_diagnostics_blocking_with_registry(
        by_file,
        None,
        None,
        None,
        Some(started + BUDGET),
        &registry,
    );
    let elapsed = started.elapsed();

    let unbounded = PER_FILE * FILES as u32;
    assert!(
        elapsed < unbounded / 2,
        "dispatch took {elapsed:?}; the deadline did not stop the fan-out \
         (unbounded run is ~{unbounded:?})"
    );
    let ran = calls.load(Ordering::SeqCst);
    assert!(
        ran < FILES,
        "every one of {FILES} files was still analyzed ({ran}); no work was cut off"
    );

    let cutoff = report
        .cutoff
        .expect("a truncated run must report a cutoff, not read as a complete one");
    assert_eq!(
        cutoff.files_analyzed + cutoff.files_skipped,
        FILES,
        "cutoff must account for every file: {cutoff:?}"
    );
    assert!(
        cutoff.files_skipped > 0,
        "files_skipped must be non-zero on a cut-off run: {cutoff:?}"
    );
    assert!(
        cutoff.tools_skipped.contains(&"slow-linter".to_string()),
        "the unfinished tool must be named: {:?}",
        cutoff.tools_skipped
    );
}

/// Why: the cutoff field must stay `None` on a run that finished, or every
/// normal response would read as partial and callers would learn to ignore it.
/// What: same fake tool with a negligible delay and no deadline; asserts all
/// files ran and `cutoff` is `None`.
/// Test: this test.
#[test]
fn dispatch_without_deadline_reports_no_cutoff() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let calls = Arc::new(AtomicUsize::new(0));
    let registry =
        crate::core::tool_registry::ToolRegistry::from_tools_for_test(vec![Arc::new(SlowTool {
            delay: std::time::Duration::ZERO,
            calls: Arc::clone(&calls),
        })]);

    let by_file: HashMap<String, String> = (0..5)
        .map(|i| (format!("src/f{i}.rs"), "fn f() {}".to_string()))
        .collect();

    let report = run_diagnostics_blocking_with_registry(by_file, None, None, None, None, &registry);

    assert_eq!(
        calls.load(Ordering::SeqCst),
        5,
        "all files must be analyzed"
    );
    assert!(
        report.cutoff.is_none(),
        "a complete run must not report a cutoff: {:?}",
        report.cutoff
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
