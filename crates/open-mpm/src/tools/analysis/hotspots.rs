//! `get_complexity_hotspots` — top-N highest-cognitive-complexity functions (#373).
//!
//! Why: A focused tool for "where should I look first?" — returns just the
//! pain points, much smaller payload than `analyze_project`.
//! What: Walks the project, collects every function across every file,
//! sorts by cognitive complexity desc, returns top N.
//! Test: `hotspots_returns_top_n_sorted`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::analyze_project::analyze_directory;
use crate::tools::traits::{ToolExecutor, ToolResult};

pub struct GetComplexityHotspotsTool;

#[async_trait]
impl ToolExecutor for GetComplexityHotspotsTool {
    fn name(&self) -> &str {
        "get_complexity_hotspots"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "get_complexity_hotspots",
                "description": "Return the top-N functions across the project sorted by cognitive complexity (highest first). Use this to triage refactoring targets without paying for a full project analysis.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "top_n": {"type": "integer", "description": "Number of hotspots to return. Default 10.", "default": 10},
                        "root": {"type": "string", "description": "Root directory. Defaults to project root."}
                    },
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let top_n = args.get("top_n").and_then(Value::as_u64).unwrap_or(10) as usize;
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
        let mut all: Vec<(String, super::metrics::FunctionMetrics)> = Vec::new();
        for a in analyses {
            for f in a.functions {
                all.push((a.file.clone(), f));
            }
        }
        all.sort_by_key(|b| std::cmp::Reverse(b.1.cognitive_complexity));
        let hotspots: Vec<Value> = all
            .into_iter()
            .take(top_n)
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
        ToolResult::ok(json!({"hotspots": hotspots}).to_string())
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
    async fn hotspots_returns_top_n_sorted() {
        let dir = tempdir().unwrap();
        write_tmp(
            dir.path(),
            "a.rs",
            r#"
fn easy(x: i32) -> i32 { x }
fn hard(x: i32) -> i32 {
    if x > 0 { if x > 1 { if x > 2 { 3 } else { 2 } } else { 1 } } else { 0 }
}
"#,
        );
        let t = GetComplexityHotspotsTool;
        let r = t
            .execute(json!({"top_n": 1, "root": dir.path().to_string_lossy()}))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        let hs = v["hotspots"].as_array().unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0]["function"], "hard");
    }
}
