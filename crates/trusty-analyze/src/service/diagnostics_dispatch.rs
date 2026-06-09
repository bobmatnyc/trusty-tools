//! Blocking diagnostics dispatch logic for `GET /indexes/{id}/diagnostics`.
//!
//! Why: the dispatch logic grew with Phase 1 (project-scoped tool support) to
//! the point where keeping it in `service/mod.rs` would push that file past
//! its 500-line-cap allowlist budget. Extracting it here keeps each file
//! focused: `mod.rs` owns the axum handler wiring; this module owns the
//! "how to run tools against an index corpus" logic.
//! What: exports `run_diagnostics_blocking`, which is called from
//! `diagnostics_for_index` under `tokio::task::spawn_blocking`. The function
//! splits tools into file-scoped (scratch dir) and project-scoped (real disk)
//! buckets and dispatches each correctly.
//! Test: `run_diagnostics_blocking_skips_unknown_languages`,
//! `run_diagnostics_blocking_respects_language_filter`, and
//! `run_diagnostics_blocking_project_scoped_skips_when_no_root` in
//! `service/mod.rs` exercise this via the public interface.

use std::collections::HashMap;
use std::path::Path;

use crate::core::ToolDiagnostic;

/// Abs-to-rel mapping: given the absolute on-disk path of a diagnostic and
/// the `(rel, real)` pairs for the current language bucket, return the
/// index-relative path to use for `diag.file`, or `None` if no match.
///
/// Why: project-scoped tools (Roslyn) emit absolute paths; the caller needs
/// to map them back to the index-relative paths that the HTTP response uses.
/// What: tries exact equality first, then a component-anchored suffix match.
/// The suffix branches strip the leading `/` from absolute paths before
/// building the format string to avoid the `//…` double-slash trap: when
/// `real` = `/home/user/proj/src/Foo.cs`, naively forming `"/{real}"` yields
/// `"//home/user/proj/src/Foo.cs"` which no absolute path ever ends with.
/// Without the strip, both suffix branches are logically dead when `real` is
/// absolute (which it always is — it is `Path::new(root).join(rel)`), so any
/// case where exact equality fails (symlink resolution, case differences on a
/// case-insensitive FS like macOS) silently drops all diagnostics.
/// Test: `abs_to_rel_exact`, `abs_to_rel_suffix_match`, and
/// `abs_to_rel_case_insensitive_returns_none` below.
fn abs_to_rel<'a>(abs_diag_file: &str, rel_real_pairs: &'a [(String, String)]) -> Option<&'a str> {
    for (rel, real) in rel_real_pairs {
        // Exact match — the common production path.
        if abs_diag_file == real.as_str() || abs_diag_file == rel.as_str() {
            return Some(rel.as_str());
        }
        // Component-anchored suffix match. We strip the leading `/` from
        // absolute `real` and `rel` before interpolating into the format
        // string so we get `"/suffix"` not `"//suffix"`.
        let real_suffix = real.trim_start_matches('/');
        let rel_suffix = rel.trim_start_matches('/');
        if abs_diag_file.ends_with(&format!("/{real_suffix}"))
            || abs_diag_file.ends_with(&format!("/{rel_suffix}"))
            || real.ends_with(&format!("/{abs_diag_file}"))
            || rel.ends_with(&format!("/{abs_diag_file}"))
        {
            return Some(rel.as_str());
        }
    }
    None
}

/// Blocking core of the diagnostics endpoint, using the process-wide global
/// tool registry.
///
/// Why: the production call site uses the global (lazily-discovered) registry
/// of whatever tools are installed on the host. This wrapper keeps the
/// existing call signature unchanged so no callers need updating.
/// What: delegates to `run_diagnostics_blocking_with_registry` with
/// `global_registry()`.
/// Test: `run_diagnostics_blocking_skips_unknown_languages`,
/// `run_diagnostics_blocking_respects_language_filter`.
pub fn run_diagnostics_blocking(
    by_file: HashMap<String, String>,
    language_filter: Option<String>,
    tool_filter: Option<Vec<String>>,
    root_path: Option<String>,
) -> Vec<ToolDiagnostic> {
    use crate::core::global_registry;
    run_diagnostics_blocking_with_registry(
        by_file,
        language_filter,
        tool_filter,
        root_path,
        global_registry(),
    )
}

/// Registry-parameterized version of the blocking diagnostics dispatch.
///
/// Why: tests need to inject a synthetic `ToolRegistry` (containing fake
/// project-scoped tools that count `run_project` invocations) to assert the
/// skip-when-no-root contract is actually enforced rather than just not
/// panicking.
/// What: same dispatch logic as the production path, but accepts any
/// `&ToolRegistry` instead of always using `global_registry()`.
///   1. Groups `by_file` entries by language (honouring `language_filter`).
///   2. For each language, splits available tools into project-scoped and
///      file-scoped buckets (honouring `tool_filter` by name in both).
///   3. File-scoped tools: write each file to a unique numeric subdirectory
///      under the scratch tempdir (keyed by loop index), call
///      `tool.run(scratch_path, content)`, rewrite `diag.file` back to the
///      index-relative path. The per-file subdir prevents basename collisions:
///      two files `src/a/main.rs` and `src/b/main.rs` have the same basename
///      but different indices, so they never overwrite each other.
///   4. Project-scoped tools: only if `root_path` is `Some`. Build real
///      on-disk paths by joining `root` with the rel path; keep only those
///      that `exist()`. Call `tool.run_project(&real_paths)`. Map each
///      `diag.file` (absolute) back to the index-relative rel via
///      `abs_to_rel`. Drop diagnostics that don't map to any indexed file.
///      When `root_path` is `None`, log at info and skip (graceful degradation).
///   5. Returns the merged `Vec<ToolDiagnostic>`.
///
/// Test: `run_diagnostics_blocking_project_scoped_skips_when_no_root` uses
/// this directly with a `FakeProjectScopedTool` registry.
/// `run_diagnostics_blocking_with_registry_two_files_same_basename` (below)
/// proves the per-file subdir isolation prevents basename collisions.
pub fn run_diagnostics_blocking_with_registry(
    by_file: HashMap<String, String>,
    language_filter: Option<String>,
    tool_filter: Option<Vec<String>>,
    root_path: Option<String>,
    registry: &crate::core::tool_registry::ToolRegistry,
) -> Vec<ToolDiagnostic> {
    use crate::lang::LanguageDetector;

    let scratch = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("failed to create scratch dir for diagnostics: {e}");
            return Vec::new();
        }
    };

    // Group by language, applying the language filter.
    let mut by_lang: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (file, content) in by_file {
        let Some(lang) = LanguageDetector::detect_file(&file) else {
            continue;
        };
        if let Some(want) = &language_filter {
            if &lang != want {
                continue;
            }
        }
        by_lang.entry(lang).or_default().push((file, content));
    }

    let mut out = Vec::new();

    for (lang, file_pairs) in &by_lang {
        let tools = registry.tools_for(lang);
        if tools.is_empty() {
            continue;
        }

        // Split tools into project-scoped and file-scoped, honouring tool_filter.
        let mut proj_tools: Vec<std::sync::Arc<dyn crate::core::tools::StaticTool>> = Vec::new();
        let mut file_tools: Vec<std::sync::Arc<dyn crate::core::tools::StaticTool>> = Vec::new();
        for tool in tools {
            if let Some(names) = &tool_filter {
                if !names.iter().any(|n| n == tool.name()) {
                    continue;
                }
            }
            if tool.is_project_scoped() {
                proj_tools.push(tool);
            } else {
                file_tools.push(tool);
            }
        }

        // --- FILE-SCOPED tools (original scratch-dir behavior) ---
        if !file_tools.is_empty() {
            // Use an incrementing counter so each file gets a unique scratch
            // subdir. This prevents basename collisions: `src/a/main.rs` and
            // `src/b/main.rs` both have basename `main.rs`, so without a
            // unique subdir the second write overwrites the first and its
            // diagnostics are silently lost. The fix mirrors commit 16cd8eac
            // (closes #976) that applied the same isolation to
            // `run_diagnostics_blocking` in `handlers/analysis.rs`.
            for (idx, (rel_file, content)) in file_pairs.iter().enumerate() {
                let name = Path::new(rel_file)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "chunk.txt".to_string());
                // Unique numeric subdir prevents basename collisions.
                let file_dir = scratch.path().join(idx.to_string());
                if let Err(e) = std::fs::create_dir_all(&file_dir) {
                    tracing::warn!("failed to create scratch subdir for {name}: {e}");
                    continue;
                }
                let path = file_dir.join(&name);
                if let Err(e) = std::fs::write(&path, content) {
                    tracing::warn!("failed to write scratch file {name}: {e}");
                    continue;
                }
                for tool in &file_tools {
                    let result = tool.run(&path, content);
                    match result {
                        Ok(mut diags) => {
                            for d in &mut diags {
                                d.file = rel_file.clone();
                            }
                            out.extend(diags);
                        }
                        Err(e) => tracing::warn!("diagnostics for {rel_file} failed: {e:#}"),
                    }
                }
            }
        }

        // --- PROJECT-SCOPED tools (real on-disk paths) ---
        if proj_tools.is_empty() {
            continue;
        }
        let root = match &root_path {
            Some(r) => r,
            None => {
                // Raise to info so operators see why C# diagnostics return
                // zero without setting RUST_LOG=debug. The usual cause is that
                // the index was not fetched with ?details=true, so root_path
                // was not included in the corpus response — an operational
                // misconfiguration, not a normal code path.
                tracing::info!(
                    "project-scoped tools available for {lang} but root_path is None; \
                     skipping (index was not fetched with ?details=true — \
                     C# diagnostics will be empty until root_path is available)"
                );
                continue;
            }
        };

        // Build real paths and keep only those that exist on disk.
        let rel_real_pairs: Vec<(String, String)> = file_pairs
            .iter()
            .filter_map(|(rel, _)| {
                let real = Path::new(root).join(rel);
                if real.exists() {
                    Some((rel.clone(), real.to_string_lossy().into_owned()))
                } else {
                    None
                }
            })
            .collect();

        if rel_real_pairs.is_empty() {
            tracing::debug!(
                "project-scoped tools for {lang}: no files exist under root {root}; skipping"
            );
            continue;
        }

        let real_paths: Vec<std::path::PathBuf> = rel_real_pairs
            .iter()
            .map(|(_, real)| std::path::PathBuf::from(real))
            .collect();

        for tool in &proj_tools {
            match tool.run_project(&real_paths) {
                Ok(diags) => {
                    for mut diag in diags {
                        match abs_to_rel(&diag.file, &rel_real_pairs) {
                            Some(rel) => {
                                diag.file = rel.to_string();
                                out.push(diag);
                            }
                            None => {
                                // Diagnostic for a file outside the indexed set — drop.
                                tracing::debug!(
                                    "dropping project-scoped diag for unmapped file: {}",
                                    diag.file
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("project-scoped diagnostics ({}) failed: {e:#}", tool.name())
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: two files with identical basenames in different index directories
    /// must each produce diagnostics independently; the basename-collision bug
    /// (writing `scratch/main.rs` twice) silently drops the first file's
    /// diagnostics. This test FAILS against `scratch.path().join(&name)` (the
    /// old code) and PASSES after the per-file `scratch/<idx>/name` fix.
    ///
    /// What: injects a `FakeFileScopedTool` that records every `(path, content)`
    /// it receives. Passes two same-basename Rust files. Asserts: (a) the fake
    /// tool was called twice, (b) the two paths are distinct, (c) neither
    /// rel_file mapping is lost (both appear in the output).
    ///
    /// Test: this test itself. Does not require any external linter.
    #[test]
    fn run_diagnostics_blocking_with_registry_two_files_same_basename() {
        use crate::core::tool_registry::ToolRegistry;
        use crate::core::tools::{StaticTool, ToolDiagnostic};
        use std::path::{Path, PathBuf};
        use std::sync::{Arc, Mutex};

        /// A fake file-scoped tool that records the (path, content) passed to
        /// each `run` call and returns a single dummy diagnostic so the caller
        /// can assert both files' diagnostics survive the pipeline.
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
                // Return one diagnostic so the caller can assert it maps back
                // to the correct index-relative path.
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

        let diags = run_diagnostics_blocking_with_registry(
            by_file, None, // language_filter
            None, // tool_filter
            None, // root_path
            &registry,
        );

        let recorded = calls.lock().unwrap();
        // Both files must have been sent to the tool.
        assert_eq!(
            recorded.len(),
            2,
            "expected 2 tool invocations (one per file), got {}; \
             basename collision likely dropped one",
            recorded.len()
        );
        // The two scratch paths must be distinct — collision means same path.
        let path0 = &recorded[0].0;
        let path1 = &recorded[1].0;
        assert_ne!(
            path0, path1,
            "the two files were written to the same scratch path ({path0:?}); \
             per-file subdir isolation is broken"
        );
        // Both diagnostics must survive back-mapping (neither was lost).
        assert_eq!(
            diags.len(),
            2,
            "expected 2 diagnostics in output (one per file), got {}; \
             one file's diagnostics were silently dropped",
            diags.len()
        );
        // Verify both index-relative paths appear in the output.
        let files: Vec<&str> = diags.iter().map(|d| d.file.as_str()).collect();
        assert!(
            files.contains(&"src/a/main.rs"),
            "src/a/main.rs missing from output: {files:?}"
        );
        assert!(
            files.contains(&"src/b/main.rs"),
            "src/b/main.rs missing from output: {files:?}"
        );
    }

    #[test]
    fn abs_to_rel_exact_match() {
        // Exact equality on the `real` path (the most common production case).
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
        // The suffix branch must work even when `real` is an absolute path.
        // Previously, forming `"/{real}"` produced `"//home/…"` — an
        // impossible match for any absolute Roslyn-emitted path. With the
        // `trim_start_matches('/')` fix, the format string becomes
        // `"/home/user/proj/src/Bar.cs"` and suffix matching works correctly.
        let pairs = vec![(
            "src/Bar.cs".to_string(),
            "/home/user/proj/src/Bar.cs".to_string(),
        )];
        // A path that shares the full suffix of `real` (e.g. a different
        // mount-point prefix) should match via the component-anchored suffix.
        // real_suffix = "home/user/proj/src/Bar.cs"
        // format = "/home/user/proj/src/Bar.cs"
        // abs ends_with "/home/user/proj/src/Bar.cs" → true
        assert_eq!(
            abs_to_rel("/symlink-root/home/user/proj/src/Bar.cs", &pairs),
            Some("src/Bar.cs"),
        );
        // A path with no component overlap at all must not match.
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
        // When the diagnostic file is already a relative path matching `rel`.
        let pairs = vec![(
            "src/Qux.cs".to_string(),
            "/home/user/proj/src/Qux.cs".to_string(),
        )];
        assert_eq!(abs_to_rel("src/Qux.cs", &pairs), Some("src/Qux.cs"));
    }
}
