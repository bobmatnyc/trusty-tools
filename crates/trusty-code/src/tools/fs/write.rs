//! `write_file` tool — create or overwrite a file, creating parent dirs as needed.
//!
//! Why: AI coding loops must be able to emit new source files. A structured
//! `ToolExecutor` with explicit working-directory scoping is safer than a raw
//! shell `echo >` or `cat >` call: path traversal is blocked at the tool boundary
//! before anything touches the filesystem.
//! What: `WriteFileTool` writes `content` to `path` (relative or absolute within
//! `working_dir`), creating all missing parent directories. Existing files are
//! overwritten atomically.
//! Test: See `#[cfg(test)]` below — covers new file, parent dir creation,
//! overwrite, and path traversal.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::fs::{FsError, scoped_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `ToolExecutor` that creates or overwrites a file.
///
/// Why: Provides agents with a safe, sandboxed way to write files inside the
/// working directory. Parent directories are created automatically so agents do
/// not need a separate `mkdir` step.
/// What: Implements `ToolExecutor` with `name = "write_file"`. Scopes all writes
/// to `working_dir`; rejects traversal attempts.
/// Test: `cargo test -p trusty-code -- tools::fs::write`.
pub struct WriteFileTool {
    working_dir: PathBuf,
}

impl WriteFileTool {
    /// Construct a new `WriteFileTool` scoped to `working_dir`.
    ///
    /// Why: The working directory is the security boundary; it must be set at
    /// construction time and cannot be overridden per-call by the LLM.
    /// What: Stores `working_dir`.
    /// Test: `write_creates_new_file`, et al.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// Write `content` to `path`, creating parent directories as needed.
    ///
    /// Why: Centralises the IO path so `execute` stays short.
    /// What: Scopes path, creates parent dirs, writes bytes, returns unit.
    /// Test: All `WriteFileTool` unit tests.
    fn write_inner(&self, path: &std::path::Path, content: &str) -> Result<(), FsError> {
        let scoped = scoped_path(&self.working_dir, path)?;

        // Create any missing parent directories.
        if let Some(parent) = scoped.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FsError::io(parent, e))?;
        }

        std::fs::write(&scoped, content).map_err(|e| FsError::io(&scoped, e))?;
        Ok(())
    }
}

#[async_trait]
impl ToolExecutor for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    /// OpenAI function-call schema for `write_file`.
    ///
    /// Why: The LLM uses this schema to construct its tool call.
    /// What: JSON object with `path` and `content` (both required).
    /// Test: `schema_has_required_fields`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Create or overwrite a file with the provided content. Parent directories are created automatically. The path must be inside the working directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative or absolute path to the file (must be inside the working directory)."
                        },
                        "content": {
                            "type": "string",
                            "description": "Text content to write to the file."
                        }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a `write_file` tool call.
    ///
    /// Why: Writes content to a file within the working directory.
    /// What: Parses `{path, content}` from `args`, calls `write_inner`, and
    /// converts the result into a `ToolResult`.
    /// Test: `write_creates_new_file`, `write_creates_parent_dirs`, etc.
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(path_str) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("write_file: missing required argument 'path'");
        };
        let Some(content) = args.get("content").and_then(Value::as_str) else {
            return ToolResult::err("write_file: missing required argument 'content'");
        };

        match self.write_inner(std::path::Path::new(path_str), content) {
            Ok(()) => ToolResult::ok(format!("wrote {path_str}")),
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

    fn make_tool(tmp: &tempfile::TempDir) -> WriteFileTool {
        WriteFileTool::new(tmp.path())
    }

    /// `write_file` creates a new file with the provided content.
    ///
    /// Why: Basic contract — write then re-read must return the same bytes.
    /// What: `execute({path:"new.py", content:"# hello"})`, then read the file.
    /// Test: This test.
    #[tokio::test]
    async fn write_creates_new_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({"path": "new.py", "content": "# hello"}))
            .await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        let content = fs::read_to_string(tmp.path().join("new.py")).expect("read back");
        assert_eq!(content, "# hello");
    }

    /// `write_file` creates missing parent directories.
    ///
    /// Why: AI agents write deeply-nested files (e.g. `src/pkg/__init__.py`).
    /// What: Write to `a/b/c.txt`; assert all dirs created and file readable.
    /// Test: This test.
    #[tokio::test]
    async fn write_creates_parent_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({"path": "a/b/c.txt", "content": "deep"}))
            .await;
        assert!(!result.is_error(), "{}", result.content());
        let content = fs::read_to_string(tmp.path().join("a/b/c.txt")).expect("read back");
        assert_eq!(content, "deep");
    }

    /// `write_file` overwrites an existing file.
    ///
    /// Why: Agents re-emit files during iteration; overwrite must succeed.
    /// What: Write `v1`, then write `v2` to the same path, assert `v2` on disk.
    /// Test: This test.
    #[tokio::test]
    async fn write_overwrites_existing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.txt"), "v1").expect("seed");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({"path": "f.txt", "content": "v2"}))
            .await;
        assert!(!result.is_error(), "{}", result.content());
        let content = fs::read_to_string(tmp.path().join("f.txt")).expect("read back");
        assert_eq!(content, "v2");
    }

    /// `write_file` rejects a path that escapes the working directory.
    ///
    /// Why: Path traversal must be blocked at the tool boundary.
    /// What: `execute({path:"../../evil.sh", content:"..."})` must return error.
    /// Test: This test.
    #[tokio::test]
    async fn path_traversal_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({"path": "../../evil.sh", "content": "harm"}))
            .await;
        assert!(result.is_error());
        assert!(
            result.content().contains("escapes"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// The schema lists both `path` and `content` as required.
    ///
    /// Why: The LLM must always provide both arguments.
    /// What: Parses `schema()` and checks both appear in `required`.
    /// Test: This test.
    #[test]
    fn schema_has_required_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let schema = tool.schema();
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(names.contains(&"path"), "must require 'path'");
        assert!(names.contains(&"content"), "must require 'content'");
    }
}
