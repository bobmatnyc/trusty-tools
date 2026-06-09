//! `edit` tool — exact-unique string replacement within a file.
//!
//! Why: Agents that iteratively refine code need a surgical replace-in-place
//! primitive that is safer than re-writing the whole file from memory. The
//! "unique match" constraint mirrors the Claude Code `Edit` tool contract:
//! if `old_string` appears more than once the replacement is ambiguous, so the
//! agent must provide more context; if it appears zero times the edit would be
//! a silent no-op.
//! What: `EditTool` reads the file, counts occurrences of `old_string`, replaces
//! exactly one occurrence (error on 0 or >1), and writes the modified content
//! back. All paths are scoped to the working directory.
//! Test: See `#[cfg(test)]` below — covers unique replace, zero-match error,
//! multiple-match error, and path traversal.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::fs::{FsError, scoped_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `ToolExecutor` that performs an exact-unique string replacement in a file.
///
/// Why: Surgical in-place edits are the primary mutation primitive for coding
/// agents. The unique-match contract prevents silent partial-writes.
/// What: Implements `ToolExecutor` with `name = "edit"`. Reads the file, counts
/// `old_string` occurrences, errors on 0 or >1, replaces the single match,
/// and writes the result back.
/// Test: `cargo test -p trusty-code -- tools::fs::edit`.
pub struct EditTool {
    working_dir: PathBuf,
}

impl EditTool {
    /// Construct a new `EditTool` scoped to `working_dir`.
    ///
    /// Why: The working directory is the security boundary set at construction.
    /// What: Stores `working_dir`.
    /// Test: `edit_replaces_unique_match`, et al.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// Perform the edit: read file, count occurrences, replace, write back.
    ///
    /// Why: Centralises the IO + matching logic so `execute` stays clean.
    /// What: Returns `Ok(())` on success. Returns `FsError::EditNotFound` when
    /// `old_string` is absent. Returns `FsError::EditAmbiguous` when it appears
    /// more than once.
    /// Test: All `EditTool` unit tests exercise this path.
    fn edit_inner(
        &self,
        path: &std::path::Path,
        old_string: &str,
        new_string: &str,
    ) -> Result<(), FsError> {
        let scoped = scoped_path(&self.working_dir, path)?;

        let content = std::fs::read_to_string(&scoped).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FsError::NotFound(scoped.clone())
            } else {
                FsError::io(&scoped, e)
            }
        })?;

        let count = content.matches(old_string).count();

        match count {
            0 => return Err(FsError::EditNotFound { path: scoped }),
            1 => {}
            n => {
                return Err(FsError::EditAmbiguous {
                    path: scoped,
                    count: n,
                });
            }
        }

        let updated = content.replacen(old_string, new_string, 1);
        std::fs::write(&scoped, updated).map_err(|e| FsError::io(&scoped, e))?;
        Ok(())
    }
}

#[async_trait]
impl ToolExecutor for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    /// OpenAI function-call schema for `edit`.
    ///
    /// Why: The LLM uses this schema to construct its tool call.
    /// What: JSON object with `path`, `old_string`, and `new_string` (all required).
    /// Test: `schema_has_required_fields`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replace an exact unique occurrence of old_string with new_string in a file. Fails if old_string appears zero or more than once — provide more context to disambiguate. The path must be inside the working directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative or absolute path to the file (must be inside the working directory)."
                        },
                        "old_string": {
                            "type": "string",
                            "description": "The exact string to replace. Must appear exactly once in the file."
                        },
                        "new_string": {
                            "type": "string",
                            "description": "The replacement string."
                        }
                    },
                    "required": ["path", "old_string", "new_string"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute an `edit` tool call.
    ///
    /// Why: Applies an exact-unique string replacement within the working directory.
    /// What: Parses `{path, old_string, new_string}` from `args`, calls
    /// `edit_inner`, and converts the result into a `ToolResult`.
    /// Test: `edit_replaces_unique_match`, `edit_errors_on_zero_matches`, etc.
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(path_str) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("edit: missing required argument 'path'");
        };
        let Some(old_string) = args.get("old_string").and_then(Value::as_str) else {
            return ToolResult::err("edit: missing required argument 'old_string'");
        };
        let Some(new_string) = args.get("new_string").and_then(Value::as_str) else {
            return ToolResult::err("edit: missing required argument 'new_string'");
        };

        match self.edit_inner(std::path::Path::new(path_str), old_string, new_string) {
            Ok(()) => ToolResult::ok(format!("edited {path_str}")),
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

    fn make_tool(tmp: &tempfile::TempDir) -> EditTool {
        EditTool::new(tmp.path())
    }

    /// `edit` replaces a unique match and the file is updated on disk.
    ///
    /// Why: Basic contract — edit a file, re-read it, assert the replacement.
    /// What: Write `old`, execute `edit(old → new)`, read back and assert `new`.
    /// Test: This test.
    #[tokio::test]
    async fn edit_replaces_unique_match() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("code.py"), "def foo():\n    pass\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({
                "path": "code.py",
                "old_string": "    pass",
                "new_string": "    return 42"
            }))
            .await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        let updated = fs::read_to_string(tmp.path().join("code.py")).expect("read");
        assert!(updated.contains("return 42"), "replacement must be applied");
        assert!(!updated.contains("    pass"), "old string must be gone");
    }

    /// `edit` errors when `old_string` is not found in the file.
    ///
    /// Why: A zero-match edit would be a silent no-op; the agent must provide
    /// a valid substring to avoid confusion about whether the edit succeeded.
    /// What: `execute` with a non-existent `old_string` must return an error.
    /// Test: This test.
    #[tokio::test]
    async fn edit_errors_on_zero_matches() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "x = 1\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({
                "path": "f.py",
                "old_string": "not_in_file",
                "new_string": "replacement"
            }))
            .await;
        assert!(result.is_error());
        assert!(
            result.content().contains("not found"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `edit` errors when `old_string` appears more than once.
    ///
    /// Why: An ambiguous replacement would modify the wrong occurrence; the
    /// agent must provide more context.
    /// What: Write a file with two identical lines, attempt `edit`; expect error.
    /// Test: This test.
    #[tokio::test]
    async fn edit_errors_on_multiple_matches() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("dup.py"), "x = 1\nx = 1\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({
                "path": "dup.py",
                "old_string": "x = 1",
                "new_string": "x = 2"
            }))
            .await;
        assert!(result.is_error());
        assert!(
            result.content().contains("ambiguous"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `edit` rejects a path that escapes the working directory.
    ///
    /// Why: Path traversal must be blocked at the tool boundary.
    /// What: `execute` with `path = "../../etc/passwd"` must return error.
    /// Test: This test.
    #[tokio::test]
    async fn path_traversal_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({
                "path": "../../etc/passwd",
                "old_string": "root",
                "new_string": "evil"
            }))
            .await;
        assert!(result.is_error());
        assert!(
            result.content().contains("escapes"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// The schema lists `path`, `old_string`, and `new_string` as required.
    ///
    /// Why: The LLM must always provide all three arguments.
    /// What: Parses `schema()` and checks all three appear in `required`.
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
        assert!(names.contains(&"old_string"), "must require 'old_string'");
        assert!(names.contains(&"new_string"), "must require 'new_string'");
    }
}
