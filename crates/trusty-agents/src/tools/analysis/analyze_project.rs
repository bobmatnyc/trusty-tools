//! `analyze_project` tool — project-wide rollup of `analyze_file` (#373).
//!
//! Why: One call gives the LLM a whole-project complexity / smell snapshot
//! plus the top hotspots, so it can decide where to dig in next without
//! issuing N `analyze_file` calls.
//! What: Walks supported source files under `root` (default = project
//! root), aggregates `FileAnalysis`, computes a health score.
//! Test: `analyze_project_handles_inline_dir` runs against a tempdir
//! containing two Rust files.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use walkdir::WalkDir;

use super::analyze_file::{FileAnalysis, analyze_file_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Source file extensions analyzed by the project walker.
///
/// Why: Mirrors the FileWatcher set so analysis stays consistent with what
/// the indexer would index.
/// What: Static slice; callers extend by editing here.
pub const ANALYZED_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "h", "cpp", "cc", "cxx", "hpp", "hxx",
];

/// Walk `root` and return paths of every supported source file.
///
/// Why: Multiple analysis tools (`analyze_project`, `find_smells`,
/// `get_complexity_hotspots`, `check_circular_dependencies`) need the same
/// set, so the walk lives in one place.
/// What: Skips `target/`, `.git/`, `node_modules/`, hidden directories, and
/// non-source extensions.
/// Test: Indirectly via `analyze_project_handles_inline_dir`.
pub fn collect_source_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // Skip hidden dirs and well-known noise.
            !(e.depth() > 0
                && (name.starts_with('.')
                    || name == "target"
                    || name == "node_modules"
                    || name == "build"
                    || name == "dist"))
        })
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
            continue;
        };
        if ANALYZED_EXTENSIONS.contains(&ext) {
            out.push(path.to_path_buf());
        }
    }
    out
}

/// Aggregate every file under `root` into a project-level summary.
///
/// Why: Shared by `AnalyzeProjectTool` and `GetComplexityHotspotsTool` —
/// both need the per-file analyses; the project tool also computes a
/// rolled-up health score.
/// What: Calls `analyze_file_path` per file (silently skipping unparseable
/// ones), returns the vector for the caller to roll up.
/// Test: `analyze_project_handles_inline_dir`.
pub fn analyze_directory(root: &Path) -> Vec<FileAnalysis> {
    collect_source_files(root)
        .into_iter()
        .filter_map(|p| analyze_file_path(&p).ok())
        .collect()
}

/// `analyze_project` — project-wide complexity rollup.
pub struct AnalyzeProjectTool;

#[async_trait]
impl ToolExecutor for AnalyzeProjectTool {
    fn name(&self) -> &str {
        "analyze_project"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "analyze_project",
                "description": "Walk every supported source file under `root` (defaults to project root), compute per-file metrics, and aggregate into a project-level summary: total files / functions, complexity grade distribution (A/B/C/D/F), smell counts by type, top-10 cognitive-complexity hotspots, and an overall health score (0-100).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Root directory to walk. Defaults to current working directory."}
                    },
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let root: PathBuf = match args.get("root").and_then(Value::as_str) {
            Some(s) => PathBuf::from(s),
            None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        };

        // tree-sitter parsing is CPU-bound; move it off the async runtime
        // so the reactor stays responsive (#376 A2).
        let root_clone = root.clone();
        let analyses =
            match tokio::task::spawn_blocking(move || analyze_directory(&root_clone)).await {
                Ok(v) => v,
                Err(e) => {
                    return ToolResult::ok(
                        json!({
                            "error": format!("analyze_directory task panicked: {e}"),
                        })
                        .to_string(),
                    );
                }
            };
        let total_files = analyses.len();
        let mut total_functions = 0usize;
        let mut grade_dist = std::collections::BTreeMap::<String, usize>::new();
        let mut smell_counts = std::collections::BTreeMap::<String, usize>::new();
        let mut all_funcs: Vec<(String, &super::metrics::FunctionMetrics)> = Vec::new();

        for a in &analyses {
            total_functions += a.functions.len();
            for f in &a.functions {
                *grade_dist.entry(f.complexity_grade.clone()).or_insert(0) += 1;
                for s in &f.smells {
                    *smell_counts.entry(s.clone()).or_insert(0) += 1;
                }
                all_funcs.push((a.file.clone(), f));
            }
            if a.file_metrics.god_class {
                *smell_counts.entry("GodClass".to_string()).or_insert(0) += 1;
            }
        }

        // Top 10 hotspots by cognitive complexity.
        all_funcs.sort_by_key(|b| std::cmp::Reverse(b.1.cognitive_complexity));
        let hotspots: Vec<Value> = all_funcs
            .iter()
            .take(10)
            .map(|(file, f)| {
                json!({
                    "file": file,
                    "function": f.name,
                    "start_line": f.start_line,
                    "end_line": f.end_line,
                    "cyclomatic": f.cyclomatic_complexity,
                    "cognitive": f.cognitive_complexity,
                    "grade": f.complexity_grade,
                })
            })
            .collect();

        // Health score: 100 - (smells_per_100_funcs * 2) - (pct_DF * 3), clamped [0,100].
        let total_smells: usize = smell_counts.values().sum();
        let smells_per_100 = if total_functions == 0 {
            0.0
        } else {
            (total_smells as f64 / total_functions as f64) * 100.0
        };
        let df_count =
            grade_dist.get("D").copied().unwrap_or(0) + grade_dist.get("F").copied().unwrap_or(0);
        let pct_df = if total_functions == 0 {
            0.0
        } else {
            (df_count as f64 / total_functions as f64) * 100.0
        };
        let raw = 100.0 - (smells_per_100 * 2.0) - (pct_df * 3.0);
        let health = raw.clamp(0.0, 100.0).round() as i64;

        let out = json!({
            "summary": {
                "root": root.display().to_string(),
                "total_files": total_files,
                "total_functions": total_functions,
            },
            "distribution": grade_dist,
            "smell_counts": smell_counts,
            "hotspots": hotspots,
            "health_score": health
        });
        ToolResult::ok(out.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn write_tmp(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[tokio::test]
    async fn analyze_project_runs_against_real_src() {
        // Smoke test against the trusty-agents project's own src/ — validates
        // that the tool handles real-world Rust code without crashing.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let src = PathBuf::from(&manifest_dir).join("src");
        if !src.exists() {
            return; // Don't fail in non-cargo contexts.
        }
        let t = AnalyzeProjectTool;
        let r = t.execute(json!({"root": src.to_string_lossy()})).await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        let total_files = v["summary"]["total_files"].as_u64().unwrap();
        let total_funcs = v["summary"]["total_functions"].as_u64().unwrap();
        assert!(total_files > 10, "expected many files, got {total_files}");
        assert!(
            total_funcs > 100,
            "expected many functions, got {total_funcs}"
        );
        // Health score must be in [0, 100].
        let h = v["health_score"].as_i64().unwrap();
        assert!((0..=100).contains(&h), "health score out of range: {h}");
        eprintln!(
            "trusty-agents/src/ analysis: {} files, {} functions, health_score={}",
            total_files, total_funcs, h
        );
    }

    #[tokio::test]
    async fn analyze_project_handles_inline_dir() {
        let dir = tempdir().unwrap();
        write_tmp(dir.path(), "a.rs", "fn a(x: i32) -> i32 { x + 1 }\n");
        write_tmp(
            dir.path(),
            "b.rs",
            "fn b(x: i32) -> i32 { if x > 0 { 1 } else { 0 } }\n",
        );
        let t = AnalyzeProjectTool;
        let r = t
            .execute(json!({"root": dir.path().to_string_lossy()}))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        assert_eq!(v["summary"]["total_files"], 2);
        assert_eq!(v["summary"]["total_functions"], 2);
        assert!(v["health_score"].as_i64().unwrap() >= 0);
    }
}
