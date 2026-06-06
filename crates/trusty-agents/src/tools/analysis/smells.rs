//! `find_smells` — list code smells, optionally filtered (#373).
//!
//! Why: The LLM often wants "show me the worst offenders" — this tool
//! returns just the smells, with severity and details, so the model can
//! prioritise without re-running `analyze_file` per file.
//! What: Walks the project (or one file), collects smells with severity
//! and one-line details, optionally filters by `smell_type` and
//! `min_severity`.
//! Test: `find_smells_filters_by_type`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::analyze_file::analyze_file_path;
use super::analyze_project::collect_source_files;
use super::metrics::{Severity, Smell, SmellType};
use crate::tools::traits::{ToolExecutor, ToolResult};

pub struct FindSmellsTool;

#[async_trait]
impl ToolExecutor for FindSmellsTool {
    fn name(&self) -> &str {
        "find_smells"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "find_smells",
                "description": "Detect code smells (LongMethod, DeepNesting, LongParameterList, ComplexMethod, GodClass) across one file or the whole project. Optional filtering by smell_type and minimum severity (info/warning/error).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string", "description": "Single file to scan. If omitted, walks the project."},
                        "smell_type": {"type": "string", "description": "Filter: LongMethod | DeepNesting | LongParameterList | ComplexMethod | GodClass."},
                        "min_severity": {"type": "string", "description": "Filter: info | warning | error. Default: info."}
                    },
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let smell_filter = args
            .get("smell_type")
            .and_then(Value::as_str)
            .and_then(SmellType::from_str);
        let min_sev = args
            .get("min_severity")
            .and_then(Value::as_str)
            .and_then(Severity::from_str)
            .unwrap_or(Severity::Info);

        let files: Vec<PathBuf> = match args.get("file_path").and_then(Value::as_str) {
            Some(p) => vec![PathBuf::from(p)],
            None => {
                let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                collect_source_files(&root)
            }
        };

        let mut smells: Vec<Smell> = Vec::new();
        for path in files {
            let Ok(a) = analyze_file_path(&path) else {
                continue;
            };
            for f in &a.functions {
                for s in &f.smells {
                    let Some(st) = SmellType::from_str(s) else {
                        continue;
                    };
                    if smell_filter.is_some() && Some(st) != smell_filter {
                        continue;
                    }
                    let sev = st.default_severity();
                    if sev < min_sev {
                        continue;
                    }
                    smells.push(Smell {
                        file: a.file.clone(),
                        function: f.name.clone(),
                        start_line: f.start_line,
                        smell_type: s.clone(),
                        severity: sev.as_str().to_string(),
                        details: smell_details(st, f),
                    });
                }
            }
            // File-level smell: GodClass.
            if a.file_metrics.god_class {
                let st = SmellType::GodClass;
                if smell_filter.is_some() && Some(st) != smell_filter {
                    continue;
                }
                let sev = st.default_severity();
                if sev < min_sev {
                    continue;
                }
                smells.push(Smell {
                    file: a.file.clone(),
                    function: String::from("<file>"),
                    start_line: 1,
                    smell_type: st.as_str().to_string(),
                    severity: sev.as_str().to_string(),
                    details: format!("file has {} functions", a.file_metrics.total_functions),
                });
            }
        }

        let out_arr: Vec<Value> = smells
            .into_iter()
            .map(|s| {
                json!({
                    "file": s.file,
                    "function": s.function,
                    "start_line": s.start_line,
                    "smell_type": s.smell_type,
                    "severity": s.severity,
                    "details": s.details,
                })
            })
            .collect();

        ToolResult::ok(json!({"smells": out_arr}).to_string())
    }
}

fn smell_details(st: SmellType, f: &super::metrics::FunctionMetrics) -> String {
    match st {
        SmellType::LongMethod => format!("{} lines of code", f.lines_of_code),
        SmellType::DeepNesting => format!("nesting depth {}", f.max_nesting_depth),
        SmellType::LongParameterList => format!("{} parameters", f.parameter_count),
        SmellType::ComplexMethod => {
            format!(
                "cyclomatic={} cognitive={}",
                f.cyclomatic_complexity, f.cognitive_complexity
            )
        }
        SmellType::GodClass => "file-level smell".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_tmp(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[tokio::test]
    async fn find_smells_filters_by_type() {
        let dir = tempdir().unwrap();
        let p = write_tmp(
            dir.path(),
            "x.rs",
            "fn many_params(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32) -> i32 { a + b + c + d + e + f + g }\n",
        );
        let t = FindSmellsTool;
        let r = t
            .execute(json!({
                "file_path": p.to_string_lossy(),
                "smell_type": "LongParameterList"
            }))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        let smells = v["smells"].as_array().unwrap();
        assert!(!smells.is_empty());
        for s in smells {
            assert_eq!(s["smell_type"], "LongParameterList");
        }
    }
}
