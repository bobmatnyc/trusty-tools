//! `write_files` tool — create or overwrite MANY files in a single call.
//!
//! Why: `write_file` is one-file-per-call, and on every bake-off run the file
//! count equalled the write-turn count 1:1 — an 8-file scaffold cost 8 turns,
//! the single biggest driver of tcode's high agent-turn count. PR #2666 lets the
//! loop execute several tool calls per turn, but the model does not always batch;
//! a dedicated `write_files` tool decouples turn-count from file-count
//! STRUCTURALLY — one call writes N files regardless of whether the model
//! batches. (#2681.)
//! What: `WriteFilesTool` takes a `files` array of `{path, content}` objects,
//! writes each (creating parent dirs) scoped to `working_dir`, and reports a
//! per-file result. A failure on one file does not abort the others — every
//! file's outcome is reported so the model can retry only what failed. Every
//! successfully written path is batched into ONE best-effort mid-task
//! incremental trusty-search index update (issue: mid-task incremental
//! re-indexing) so `search_code` sees the whole batch within the same task.
//! Test: See `#[cfg(test)]` below — covers a multi-file write, partial failure,
//! empty array, and path traversal.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::fs::{FsError, scoped_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Canonical name of the batch-write tool.
///
/// Why: Referenced by the registry wiring and by tests that assert the tool is
/// registered; a shared constant avoids stringly-typed drift.
/// What: The function name exposed to the LLM.
/// Test: `crate::tools::fs_registry_tests` (batch-write registration).
pub const WRITE_FILES_TOOL_NAME: &str = "write_files";

/// `ToolExecutor` that creates or overwrites a batch of files in one call.
///
/// Why: Structurally decouples turn-count from file-count so an N-file scaffold
/// is one turn, not N (the dominant bake-off turn sink).
/// What: Implements `ToolExecutor` with `name = "write_files"`; scopes every
/// write to `working_dir`; rejects traversal per-file without aborting siblings.
/// Test: `cargo test -p trusty-code -- tools::fs::write_files`.
pub struct WriteFilesTool {
    working_dir: PathBuf,
}

impl WriteFilesTool {
    /// Construct a new `WriteFilesTool` scoped to `working_dir`.
    ///
    /// Why: The working directory is the security boundary; set once at
    /// construction so the LLM cannot widen it per-call.
    /// What: Stores `working_dir`.
    /// Test: `write_files_writes_all`, et al.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// Write a single `{path, content}` entry, creating parent dirs as needed.
    ///
    /// Why: Reuses the same scoped-write contract as `WriteFileTool` so batch and
    /// single writes behave identically per-file.
    /// What: Scopes path, creates parent dirs, writes bytes.
    /// Test: Exercised by all `WriteFilesTool` unit tests.
    fn write_one(&self, path: &str, content: &str) -> Result<(), FsError> {
        let scoped = scoped_path(&self.working_dir, std::path::Path::new(path))?;
        if let Some(parent) = scoped.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FsError::io(parent, e))?;
        }
        std::fs::write(&scoped, content).map_err(|e| FsError::io(&scoped, e))?;
        Ok(())
    }
}

#[async_trait]
impl ToolExecutor for WriteFilesTool {
    fn name(&self) -> &str {
        WRITE_FILES_TOOL_NAME
    }

    /// OpenAI function-call schema for `write_files`.
    ///
    /// Why: The LLM constructs its call from this schema; the `files` array
    /// mirrors the `execute` contract exactly.
    /// What: JSON object with a required `files` array of `{path, content}`.
    /// Test: `schema_requires_files_array`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": WRITE_FILES_TOOL_NAME,
                "description": "Create or overwrite MULTIPLE files in a single call. Prefer this over several separate write_file calls when scaffolding independent files — it writes them all in one turn. Parent directories are created automatically. Each path must be inside the working directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "files": {
                            "type": "array",
                            "description": "The files to write. Each element is an object with a path and its full content.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": {
                                        "type": "string",
                                        "description": "Relative or absolute path (must be inside the working directory)."
                                    },
                                    "content": {
                                        "type": "string",
                                        "description": "Full text content to write to the file."
                                    }
                                },
                                "required": ["path", "content"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["files"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a `write_files` tool call.
    ///
    /// Why: Writes every file in the batch, reporting each outcome so a single
    /// bad entry does not lose the successful writes.
    /// What: Parses `{files:[{path, content}, …]}`, writes each, and returns a
    /// per-file summary. Returns an error only when `files` is missing/not an
    /// array, or when EVERY write failed. Batches every SUCCESSFULLY written
    /// path into ONE best-effort mid-task incremental trusty-search index
    /// update (issue: mid-task incremental re-indexing) so `search_code` can
    /// see the whole batch within the same task — this never affects the
    /// returned `ToolResult`; see
    /// [`trusty_common::search_index::index_files_best_effort`]'s fail-open
    /// contract.
    /// Test: `write_files_writes_all`, `write_files_partial_failure`.
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(files) = args.get("files").and_then(Value::as_array) else {
            return ToolResult::err("write_files: missing required argument 'files' (an array)");
        };
        if files.is_empty() {
            return ToolResult::err("write_files: 'files' array is empty — nothing to write");
        }

        let mut lines = Vec::with_capacity(files.len());
        let mut ok_count = 0usize;
        let mut written_paths = Vec::with_capacity(files.len());
        for (i, entry) in files.iter().enumerate() {
            let path = entry.get("path").and_then(Value::as_str);
            let content = entry.get("content").and_then(Value::as_str);
            match (path, content) {
                (Some(p), Some(c)) => match self.write_one(p, c) {
                    Ok(()) => {
                        ok_count += 1;
                        lines.push(format!("wrote {p}"));
                        written_paths.push(PathBuf::from(p));
                    }
                    Err(e) => lines.push(format!("FAILED {p}: {e}")),
                },
                _ => lines.push(format!(
                    "FAILED entry {i}: each element needs both 'path' and 'content'"
                )),
            }
        }

        if !written_paths.is_empty() {
            trusty_common::search_index::index_files_best_effort(&self.working_dir, &written_paths);
        }

        let summary = lines.join("\n");
        if ok_count == 0 {
            // Every write failed — surface as a recoverable error so the model
            // retries rather than treating the batch as done.
            ToolResult::err(summary)
        } else {
            ToolResult::ok(format!(
                "{summary}\n({ok_count}/{} files written)",
                files.len()
            ))
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

    /// `write_files` writes every file in the batch in a single call.
    ///
    /// Why: This is the structural turn-count/file-count decoupling — one call,
    /// N files (#2681).
    /// What: Writes three files (one nested), asserts all three land on disk with
    /// the right content and the summary reports 3/3.
    /// Test: This test.
    #[tokio::test]
    async fn write_files_writes_all() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = WriteFilesTool::new(tmp.path());
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.py", "content": "# a"},
                    {"path": "pkg/b.py", "content": "# b"},
                    {"path": "src/main.rs", "content": "fn main() {}"}
                ]
            }))
            .await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert_eq!(fs::read_to_string(tmp.path().join("a.py")).unwrap(), "# a");
        assert_eq!(
            fs::read_to_string(tmp.path().join("pkg/b.py")).unwrap(),
            "# b"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
        assert!(result.content().contains("3/3"), "{}", result.content());
    }

    /// A single bad entry does not abort the successful writes.
    ///
    /// Why: Partial failure must preserve the good writes and report the bad one,
    /// mirroring the batched-tool-dispatch contract.
    /// What: Batches a valid file and a traversal-escaping file; asserts the valid
    /// one is written and the summary flags the failure.
    /// Test: This test.
    #[tokio::test]
    async fn write_files_partial_failure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = WriteFilesTool::new(tmp.path());
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "good.py", "content": "ok"},
                    {"path": "../../evil.sh", "content": "harm"}
                ]
            }))
            .await;
        // At least one succeeded → overall success with a per-file summary.
        assert!(!result.is_error(), "{}", result.content());
        assert_eq!(
            fs::read_to_string(tmp.path().join("good.py")).unwrap(),
            "ok"
        );
        assert!(result.content().contains("wrote good.py"));
        assert!(
            result.content().contains("FAILED"),
            "must flag the failed entry: {}",
            result.content()
        );
    }

    /// An entirely-failing batch surfaces as an error.
    ///
    /// Why: If nothing was written, the model must retry, not treat it as done.
    /// What: Batches a single traversal-escaping file; asserts an error result.
    /// Test: This test.
    #[tokio::test]
    async fn write_files_all_fail_is_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = WriteFilesTool::new(tmp.path());
        let result = tool
            .execute(json!({"files": [{"path": "../../evil.sh", "content": "x"}]}))
            .await;
        assert!(result.is_error());
        assert!(result.content().contains("FAILED"));
    }

    /// An empty `files` array is a recoverable error.
    ///
    /// Why: Nothing to write is a caller mistake worth surfacing.
    /// What: `execute({files: []})` returns an error.
    /// Test: This test.
    #[tokio::test]
    async fn write_files_empty_array_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = WriteFilesTool::new(tmp.path());
        let result = tool.execute(json!({"files": []})).await;
        assert!(result.is_error());
        assert!(result.content().contains("empty"));
    }

    /// The schema requires the `files` array.
    ///
    /// Why: The LLM must always provide the batch.
    /// What: Parses `schema()` and checks `required` contains "files".
    /// Test: This test.
    #[test]
    fn schema_requires_files_array() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = WriteFilesTool::new(tmp.path());
        let schema = tool.schema();
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("files")),
            "schema must list 'files' as required"
        );
    }
}
