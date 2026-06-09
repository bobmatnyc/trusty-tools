//! `read_file` tool — read file contents, optionally limited to a line range.
//!
//! Why: AI agents need to inspect existing files before editing them. A structured
//! `ToolExecutor` with explicit size and line-range parameters is safer than
//! passing raw `cat` arguments to a shell tool.
//! What: `ReadFileTool` reads up to `MAX_FILE_BYTES` (1 MiB) from the path,
//! optionally slicing to `[start_line, end_line]` (1-based, inclusive). All paths
//! are scoped to the provided working directory; traversal attempts are rejected.
//! Test: See `#[cfg(test)]` below — covers round-trip, line range, size cap,
//! missing file, and path traversal.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::fs::{FsError, scoped_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Maximum file size allowed by `ReadFileTool` (1 MiB).
///
/// Why: LLM context windows are finite; a 1 GiB file would freeze the loop.
/// Rejecting early gives the agent a clear error rather than an OOM or timeout.
/// What: Constant used both for the runtime guard and exposed via the schema
/// description.
/// Test: `read_returns_error_on_oversized_file`.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

/// `ToolExecutor` that reads the contents of a file.
///
/// Why: Provides agents with a safe, sandboxed way to read files inside the
/// working directory. Construction requires an explicit `working_dir` so the
/// scoping contract is compile-time visible.
/// What: Implements `ToolExecutor` with `name = "read_file"`. Reads the whole
/// file (up to `MAX_FILE_BYTES`) and optionally slices to a 1-based line range.
/// Test: `cargo test -p trusty-code -- tools::fs::read`.
pub struct ReadFileTool {
    working_dir: PathBuf,
}

impl ReadFileTool {
    /// Construct a new `ReadFileTool` scoped to `working_dir`.
    ///
    /// Why: The working directory is the security boundary; it must be set at
    /// construction time so it cannot be changed per-call by the LLM.
    /// What: Stores `working_dir` for use in every `execute` call.
    /// Test: `read_round_trips_file`, et al.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// Read file contents with optional line-range slicing.
    ///
    /// Why: Centralises the IO path so `execute` stays clean.
    /// What: Reads the file, enforces size cap, applies optional `[start, end]`
    /// line range (1-based, inclusive), and returns the text.
    /// Test: Exercised by all `ReadFileTool` unit tests.
    fn read_inner(
        &self,
        path: &std::path::Path,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String, FsError> {
        let scoped = scoped_path(&self.working_dir, path)?;

        let meta = std::fs::metadata(&scoped).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FsError::NotFound(scoped.clone())
            } else {
                FsError::io(&scoped, e)
            }
        })?;

        if meta.len() > MAX_FILE_BYTES {
            return Err(FsError::FileTooLarge {
                path: scoped,
                bytes: meta.len(),
                max: MAX_FILE_BYTES,
            });
        }

        let content = std::fs::read_to_string(&scoped).map_err(|e| FsError::io(&scoped, e))?;

        // Apply optional line range (1-based inclusive).
        match (start_line, end_line) {
            (None, None) => Ok(content),
            (start, end) => {
                let start_idx = start.unwrap_or(1).saturating_sub(1); // 0-based
                let lines: Vec<&str> = content.lines().collect();
                let end_idx = end.map(|e| e.min(lines.len())).unwrap_or(lines.len());
                let slice = lines[start_idx.min(lines.len())..end_idx].join("\n");
                Ok(slice)
            }
        }
    }
}

#[async_trait]
impl ToolExecutor for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    /// OpenAI function-call schema for `read_file`.
    ///
    /// Why: The LLM uses this schema to construct its tool call. Parameters
    /// mirror the `execute` argument contract exactly.
    /// What: JSON object with `path` (required), `start_line` and `end_line`
    /// (optional, 1-based integers).
    /// Test: `cargo test -p trusty-code -- tools::fs::read::tests::schema_has_required_path`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the contents of a file. Limited to 1 MiB. Optional start_line/end_line (1-based, inclusive) restrict the returned text.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative or absolute path to the file (must be inside the working directory)."
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "First line to return (1-based, inclusive). Defaults to 1."
                        },
                        "end_line": {
                            "type": "integer",
                            "description": "Last line to return (1-based, inclusive). Defaults to end-of-file."
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a `read_file` tool call.
    ///
    /// Why: Reads a file within the working directory, enforcing the size cap
    /// and optional line range.
    /// What: Parses `{path, start_line?, end_line?}` from `args`, calls
    /// `read_inner`, and converts the result into a `ToolResult`.
    /// Test: `read_round_trips_file`, `read_honors_line_range`, etc.
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(path_str) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("read_file: missing required argument 'path'");
        };

        let start_line = args
            .get("start_line")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let end_line = args
            .get("end_line")
            .and_then(Value::as_u64)
            .map(|n| n as usize);

        match self.read_inner(std::path::Path::new(path_str), start_line, end_line) {
            Ok(content) => ToolResult::ok(content),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::tools::traits::ToolExecutor;

    fn make_tool(tmp: &tempfile::TempDir) -> ReadFileTool {
        ReadFileTool::new(tmp.path())
    }

    /// `read_file` round-trips a file written to the working directory.
    ///
    /// Why: Basic contract — write then read must return the same bytes.
    /// What: Writes `hello.txt`, calls `execute({path:"hello.txt"})`, asserts content.
    /// Test: This test.
    #[tokio::test]
    async fn read_round_trips_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("hello.txt"), "line one\nline two\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool.execute(json!({"path": "hello.txt"})).await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert!(result.content().contains("line one"));
        assert!(result.content().contains("line two"));
    }

    /// `read_file` with a line range returns only the requested lines.
    ///
    /// Why: Agents often need a specific section of a large file.
    /// What: Writes a three-line file, reads only line 2, asserts only line 2 returned.
    /// Test: This test.
    #[tokio::test]
    async fn read_honors_line_range() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("multi.txt"), "a\nb\nc\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({"path": "multi.txt", "start_line": 2, "end_line": 2}))
            .await;
        assert!(!result.is_error());
        assert_eq!(result.content(), "b");
    }

    /// `read_file` returns an error for a file exceeding the size cap.
    ///
    /// Why: The size cap must be enforced to protect the LLM loop.
    /// What: Creates a file larger than `MAX_FILE_BYTES`, expects an error
    /// containing "too large".
    /// Test: This test.
    #[tokio::test]
    async fn read_returns_error_on_oversized_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let big = vec![b'x'; (MAX_FILE_BYTES + 1) as usize];
        fs::write(tmp.path().join("big.bin"), &big).expect("write");
        let tool = make_tool(&tmp);
        let result = tool.execute(json!({"path": "big.bin"})).await;
        assert!(result.is_error());
        assert!(
            result.content().contains("too large"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `read_file` returns an error for a missing file.
    ///
    /// Why: Missing-file errors must be surfaced as `ToolResult::Error`, not panic.
    /// What: `execute({path:"does_not_exist.txt"})` on a non-existent file.
    /// Test: This test.
    #[tokio::test]
    async fn read_returns_error_on_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let result = tool.execute(json!({"path": "does_not_exist.txt"})).await;
        assert!(result.is_error());
        assert!(
            result.content().contains("not found"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `read_file` rejects a path that escapes the working directory.
    ///
    /// Why: Path traversal must be blocked regardless of the LLM's arguments.
    /// What: `execute({path:"../../etc/passwd"})` must return `ToolResult::Error`.
    /// Test: This test.
    #[tokio::test]
    async fn path_traversal_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let result = tool.execute(json!({"path": "../../etc/passwd"})).await;
        assert!(result.is_error());
        assert!(
            result.content().contains("escapes"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// The schema has `path` in its required list.
    ///
    /// Why: The LLM uses the schema to construct calls; a missing `required`
    /// entry would cause the LLM to omit `path` and the call to fail.
    /// What: Parses `schema()` and checks `parameters.required` contains "path".
    /// Test: This test.
    #[test]
    fn schema_has_required_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let schema = tool.schema();
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("path")),
            "schema must list 'path' as required"
        );
    }
}
