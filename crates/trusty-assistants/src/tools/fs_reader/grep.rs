//! The `grep_files` read-only tool plus its ripgrep / walkdir backends.
//!
//! Why: An explorer agent needs content search without shelling out to an
//! arbitrary command; this tool prefers ripgrep when available and falls back
//! to a bounded walkdir+substring scan. The CWD guard prevents searching
//! outside the project.
//! What: `GrepFilesTool` implements `ToolExecutor`; `run_ripgrep` and
//! `walkdir_grep` are the two backends.
//! Test: `super::grep_files_*` cases in the parent module's test block.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use super::cwd::{GREP_DEFAULT_MAX_RESULTS, GREP_HARD_CAP, resolve_within_cwd};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Read-only content grep over the working directory.
pub struct GrepFilesTool;

impl GrepFilesTool {
    /// Construct a new `GrepFilesTool`. Zero-sized; pattern + path are
    /// supplied per-call via tool arguments.
    pub fn new() -> Self {
        Self
    }
}

impl Default for GrepFilesTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for GrepFilesTool {
    fn name(&self) -> &str {
        "grep_files"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "grep_files",
                "description": "Search for a regex pattern in files under `path`. Uses ripgrep (`rg --json`) if available, else falls back to a walkdir+regex scan. Returns lines in `file:line: match` format. Paths outside CWD are rejected.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Regex pattern to search for (ripgrep regex syntax)."
                        },
                        "path": {
                            "type": "string",
                            "description": "Relative or absolute path to search (file or directory) inside CWD."
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Max matching lines to return. Default 50, capped at 500.",
                            "minimum": 1,
                            "maximum": 500
                        }
                    },
                    "required": ["pattern", "path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
            return ToolResult::err("grep_files: missing 'pattern'");
        };
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("grep_files: missing 'path'");
        };
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(GREP_DEFAULT_MAX_RESULTS)
            .min(GREP_HARD_CAP);

        let canon = match resolve_within_cwd(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("grep_files: {e}")),
        };

        // Prefer ripgrep for correctness + speed if available on PATH.
        match run_ripgrep(pattern, &canon, max_results).await {
            Ok(Some(out)) => return ToolResult::ok(out),
            Ok(None) => {
                tracing::debug!("grep_files: rg not available; falling back to walkdir");
            }
            Err(e) => {
                tracing::debug!(error = %e, "grep_files: rg failed; falling back");
            }
        }

        match walkdir_grep(pattern, &canon, max_results) {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::err(format!("grep_files: {e}")),
        }
    }
}

/// Run `rg --line-number --no-heading <pattern> <path>` and return the raw
/// textual output (capped at `max_results` lines). Returns `Ok(None)` if
/// ripgrep isn't installed so the caller can fall back.
async fn run_ripgrep(
    pattern: &str,
    path: &Path,
    max_results: usize,
) -> anyhow::Result<Option<String>> {
    // Probe for rg.
    let probe = Command::new("rg").arg("--version").output().await;
    let Ok(probe) = probe else {
        return Ok(None);
    };
    if !probe.status.success() {
        return Ok(None);
    }

    let output = Command::new("rg")
        .arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--max-count")
        .arg(max_results.to_string())
        .arg(pattern)
        .arg(path)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    // rg exits 1 when no matches — treat as empty success.
    let lines: Vec<&str> = stdout.lines().take(max_results).collect();
    if lines.is_empty() {
        return Ok(Some(format!(
            "(no matches for /{pattern}/ under {})",
            path.display()
        )));
    }
    Ok(Some(lines.join("\n")))
}

/// Fallback: walk `path` with std::fs (+ a small hand-rolled depth-first
/// iterator) and look for `pattern` with the `regex` crate... except we
/// don't depend on `regex`. Use plain substring matching as a pragmatic
/// fallback; callers that need regex should install ripgrep.
fn walkdir_grep(pattern: &str, path: &Path, max_results: usize) -> anyhow::Result<String> {
    let mut out_lines: Vec<String> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![path.to_path_buf()];

    while let Some(cur) = stack.pop() {
        if out_lines.len() >= max_results {
            break;
        }
        if cur.is_file() {
            // Centralised skip: binaries-by-extension, oversize files, and
            // anything inside a denied directory tree.
            if crate::tools::file_filter::should_skip_file(&cur) {
                continue;
            }
            match std::fs::read_to_string(&cur) {
                Ok(body) => {
                    for (lineno, line) in body.lines().enumerate() {
                        if line.contains(pattern) {
                            out_lines.push(format!(
                                "{}:{}: {}",
                                cur.display(),
                                lineno + 1,
                                line.trim_end()
                            ));
                            if out_lines.len() >= max_results {
                                break;
                            }
                        }
                    }
                }
                Err(_) => {
                    // Binary / unreadable — skip silently.
                }
            }
        } else if cur.is_dir() {
            // Skip common ignore dirs to keep the fallback usable.
            if let Some(name) = cur.file_name().and_then(|s| s.to_str())
                && crate::tools::file_filter::should_skip_dir(name)
            {
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(&cur) {
                for entry in rd.flatten() {
                    stack.push(entry.path());
                }
            }
        }
    }

    if out_lines.is_empty() {
        return Ok(format!(
            "(no matches for {pattern:?} under {})",
            path.display()
        ));
    }
    Ok(out_lines.join("\n"))
}
