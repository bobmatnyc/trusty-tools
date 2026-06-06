//! `analyze_file` tool — per-file structural metrics + smells (#373).
//!
//! Why: Gives the LLM a single call to drop into a file and learn its
//! complexity hotspots, parameter counts, and code smells without re-reading
//! the source. Built on tree-sitter via the shared `ast_walker`.
//! What: `AnalyzeFileTool` implementing `ToolExecutor`. Public
//! `analyze_file_path` returns structured `FileAnalysis` so other tools
//! (`analyze_project`, `find_smells`, `get_complexity_hotspots`) can reuse
//! the work without re-implementing it.
//! Test: `analyze_file_returns_function_metrics` against an inline fixture.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ast_walker::{compute_function_complexity, count_parameters_for_function};
use super::metrics::{FileMetrics, FunctionMetrics, SmellType, complexity_grade};
use crate::tools::traits::{ToolExecutor, ToolResult};

use trusty_common::symgraph::symbol::{Symbol, SymbolKind, detect_language, extract_symbols};

/// Combined per-file analysis result reused by the other analysis tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAnalysis {
    pub file: String,
    pub language: String,
    pub functions: Vec<FunctionMetrics>,
    pub file_metrics: FileMetrics,
}

/// Read + parse + analyse a single file.
///
/// Why: Shared by every analysis tool that operates on individual files
/// (`AnalyzeFileTool`, `AnalyzeProjectTool`, `FindSmellsTool`,
/// `GetComplexityHotspotsTool`).
/// What: Detects language, extracts symbols, computes per-function complexity
/// metrics, derives file-level metrics, and tags smells.
/// Test: `analyze_file_returns_function_metrics`.
pub fn analyze_file_path(path: &Path) -> Result<FileAnalysis> {
    let (lang, lang_tag) = detect_language(path)
        .with_context(|| format!("unsupported file extension: {}", path.display()))?;
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let symbols = extract_symbols(&source, lang, path);

    // Function-level metrics.
    let mut functions = Vec::new();
    let mut imports = 0usize;
    let mut total_func_loc = 0usize;
    for sym in &symbols {
        match sym.kind {
            SymbolKind::Function | SymbolKind::Method => {
                functions.push(function_metrics_for(sym, lang_tag));
                total_func_loc += sym.end_line.saturating_sub(sym.start_line) + 1;
            }
            SymbolKind::Import => imports += 1,
            _ => {}
        }
    }

    let total_functions = functions.len();
    let max_cyclo = functions
        .iter()
        .map(|f| f.cyclomatic_complexity)
        .max()
        .unwrap_or(0);
    let avg_cyclo = if total_functions == 0 {
        0.0
    } else {
        let sum: u64 = functions
            .iter()
            .map(|f| u64::from(f.cyclomatic_complexity))
            .sum();
        sum as f64 / total_functions as f64
    };

    // Coupling: efferent = number of imports in this file. Afferent is filled
    // in by callers that have the project-wide import map (analyze_project).
    let coupling_efferent = imports;
    let coupling_afferent = 0usize;
    let denom = (coupling_afferent + coupling_efferent) as f64;
    let instability = if denom > 0.0 {
        coupling_efferent as f64 / denom
    } else {
        0.0
    };

    let god_class = total_functions > 20 && total_func_loc > 300;

    let file_metrics = FileMetrics {
        total_functions,
        avg_cyclomatic: round2(avg_cyclo),
        max_cyclomatic: max_cyclo,
        coupling_afferent,
        coupling_efferent,
        instability: round2(instability),
        god_class,
    };

    Ok(FileAnalysis {
        file: path.display().to_string(),
        language: lang_tag.to_string(),
        functions,
        file_metrics,
    })
}

/// Round a float to 2 decimal places for stable JSON output.
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Compute per-function metrics + smells from one Symbol.
fn function_metrics_for(sym: &Symbol, language: &str) -> FunctionMetrics {
    let comp = compute_function_complexity(&sym.source, language);
    let param_count = count_parameters_for_function(&sym.source, language);
    let loc = sym.end_line.saturating_sub(sym.start_line) + 1;

    let mut smells: Vec<String> = Vec::new();
    if loc > 50 {
        smells.push(SmellType::LongMethod.as_str().to_string());
    }
    if comp.max_nesting > 4 {
        smells.push(SmellType::DeepNesting.as_str().to_string());
    }
    if param_count > 5 {
        smells.push(SmellType::LongParameterList.as_str().to_string());
    }
    if comp.cyclomatic > 10 {
        smells.push(SmellType::ComplexMethod.as_str().to_string());
    }

    FunctionMetrics {
        name: sym.name.clone(),
        start_line: sym.start_line,
        end_line: sym.end_line,
        cyclomatic_complexity: comp.cyclomatic,
        cognitive_complexity: comp.cognitive,
        max_nesting_depth: comp.max_nesting,
        parameter_count: param_count,
        lines_of_code: loc,
        complexity_grade: complexity_grade(comp.cyclomatic).to_string(),
        smells,
    }
}

/// Convert a `FileAnalysis` into the canonical JSON shape returned to the LLM.
pub fn file_analysis_to_json(a: &FileAnalysis) -> Value {
    json!({
        "file": a.file,
        "language": a.language,
        "functions": a.functions.iter().map(|f| json!({
            "name": f.name,
            "start_line": f.start_line,
            "end_line": f.end_line,
            "cyclomatic_complexity": f.cyclomatic_complexity,
            "cognitive_complexity": f.cognitive_complexity,
            "max_nesting_depth": f.max_nesting_depth,
            "parameter_count": f.parameter_count,
            "lines_of_code": f.lines_of_code,
            "complexity_grade": f.complexity_grade,
            "smells": f.smells,
        })).collect::<Vec<_>>(),
        "file_metrics": {
            "total_functions": a.file_metrics.total_functions,
            "avg_cyclomatic": a.file_metrics.avg_cyclomatic,
            "max_cyclomatic": a.file_metrics.max_cyclomatic,
            "coupling_afferent": a.file_metrics.coupling_afferent,
            "coupling_efferent": a.file_metrics.coupling_efferent,
            "instability": a.file_metrics.instability,
            "god_class": a.file_metrics.god_class,
        }
    })
}

/// `analyze_file` — return structural metrics for one file.
pub struct AnalyzeFileTool;

#[async_trait]
impl ToolExecutor for AnalyzeFileTool {
    fn name(&self) -> &str {
        "analyze_file"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "analyze_file",
                "description": "Compute per-function complexity metrics (cyclomatic, cognitive, nesting depth, parameter count, LOC), file-level metrics (total functions, coupling, instability, god-class), and detect code smells (LongMethod, DeepNesting, LongParameterList, ComplexMethod) for a single source file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string", "description": "Path to a source file (.rs/.py/.ts/.tsx/.js/.jsx/.go/.java/.c/.cpp)."}
                    },
                    "required": ["file_path"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file_path) = args.get("file_path").and_then(Value::as_str) else {
            return ToolResult::err("analyze_file: missing 'file_path'");
        };
        let path = PathBuf::from(file_path);
        match analyze_file_path(&path) {
            Ok(a) => ToolResult::ok(file_analysis_to_json(&a).to_string()),
            Err(e) => ToolResult::err(format!("analyze_file: {e}")),
        }
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
    async fn analyze_file_returns_function_metrics() {
        let dir = tempdir().unwrap();
        let p = write_tmp(
            dir.path(),
            "x.rs",
            r#"
fn easy(x: i32) -> i32 { x + 1 }

fn complex(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32) -> i32 {
    let mut total = 0;
    if a > 0 {
        for i in 0..b {
            if i > c {
                total += i;
            } else if i > d {
                total += d;
            } else {
                total += 1;
            }
        }
    }
    if e > 0 && f > 0 {
        total += g;
    }
    total
}
"#,
        );
        let t = AnalyzeFileTool;
        let r = t.execute(json!({"file_path": p.to_string_lossy()})).await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        assert_eq!(v["language"], "rust");
        let funcs = v["functions"].as_array().unwrap();
        assert_eq!(funcs.len(), 2);

        // The complex function should have higher cyclomatic and trigger smells.
        let complex = funcs.iter().find(|f| f["name"] == "complex").unwrap();
        assert!(complex["cyclomatic_complexity"].as_u64().unwrap() >= 4);
        assert!(complex["parameter_count"].as_u64().unwrap() >= 7);
        let smells = complex["smells"].as_array().unwrap();
        let names: Vec<&str> = smells.iter().filter_map(|s| s.as_str()).collect();
        assert!(names.contains(&"LongParameterList"));
    }

    #[tokio::test]
    async fn analyze_file_returns_file_metrics() {
        let dir = tempdir().unwrap();
        let p = write_tmp(
            dir.path(),
            "y.rs",
            "use std::fs; use std::io;\nfn a() {}\nfn b() {}\n",
        );
        let a = analyze_file_path(&p).unwrap();
        assert_eq!(a.file_metrics.total_functions, 2);
        assert_eq!(a.file_metrics.coupling_efferent, 2);
    }
}
