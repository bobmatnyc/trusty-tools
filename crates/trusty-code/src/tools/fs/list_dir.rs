//! `list_dir` tool — list the entries of a single directory.
//!
//! Why: Complements `glob`/`grep` (#1027) — the cheapest discovery primitive is
//! "what's in this directory?", which the engineer previously could only get by
//! shelling out to `ls`. A structured, non-recursive listing lets the model
//! orient itself in one typed call instead of a bash round-trip.
//! What: `ListDirTool` reads one directory (default: the working-directory root)
//! and returns its immediate entries, each tagged as a directory (`name/`) or a
//! file (`name`). Non-recursive by design — recursion is `glob`'s job.
//! Test: See `#[cfg(test)]` below — covers a mixed listing, subdirectory
//! listing, missing directory, not-a-directory, and path traversal.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::fs::{FsError, scoped_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `ToolExecutor` that lists the immediate entries of a directory.
///
/// Why: Gives agents a native, sandboxed directory-listing primitive so they
/// stop shelling out to `ls`, cutting round-trips.
/// What: Implements `ToolExecutor` with `name = "list_dir"`, scoped to
/// `working_dir`; rejects traversal attempts in the optional `path` argument.
/// Test: `cargo test -p trusty-code -- tools::fs::list_dir`.
pub struct ListDirTool {
    working_dir: PathBuf,
}

impl ListDirTool {
    /// Construct a new `ListDirTool` scoped to `working_dir`.
    ///
    /// Why: The working directory is the security boundary; set once at
    /// construction so the LLM cannot widen it per-call.
    /// What: Stores `working_dir`.
    /// Test: `list_dir_lists_entries`, et al.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// List the immediate entries of `sub_path` (default: the working dir).
    ///
    /// Why: Centralises the scope + read_dir + formatting so `execute` stays
    /// short.
    /// What: Scopes the path, verifies it is a directory, reads its entries,
    /// tags each as `name/` (dir) or `name` (file), and returns them sorted.
    /// Test: All `ListDirTool` unit tests.
    fn list_inner(&self, sub_path: Option<&str>) -> Result<String, FsError> {
        let target = match sub_path {
            Some(p) => scoped_path(&self.working_dir, Path::new(p))?,
            None => scoped_path(&self.working_dir, Path::new("."))?,
        };

        let meta = std::fs::metadata(&target).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FsError::NotFound(target.clone())
            } else {
                FsError::io(&target, e)
            }
        })?;

        if !meta.is_dir() {
            return Err(FsError::NotADirectory(target));
        }

        let read = std::fs::read_dir(&target).map_err(|e| FsError::io(&target, e))?;
        let mut entries = Vec::new();
        for dirent in read {
            let dirent = dirent.map_err(|e| FsError::io(&target, e))?;
            let name = dirent.file_name().to_string_lossy().into_owned();
            // A trailing slash marks directories so the model can tell them
            // apart without a second call.
            let is_dir = dirent.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir {
                entries.push(format!("{name}/"));
            } else {
                entries.push(name);
            }
        }

        if entries.is_empty() {
            return Ok("(empty directory)".to_string());
        }

        entries.sort();
        Ok(entries.join("\n"))
    }
}

#[async_trait]
impl ToolExecutor for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    /// OpenAI function-call schema for `list_dir`.
    ///
    /// Why: The LLM constructs its call from this schema; parameters mirror the
    /// `execute` contract exactly.
    /// What: JSON object with an optional `path` (directory to list; defaults to
    /// the working-directory root).
    /// Test: `schema_has_no_required_fields`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List the immediate entries of a directory (non-recursive). Directories are suffixed with '/'. Defaults to the project root. Prefer this over shelling out to 'ls'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory to list, relative to the working directory. Defaults to the project root."
                        }
                    },
                    "required": [],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a `list_dir` tool call.
    ///
    /// Why: Lists the entries of a directory within the working directory.
    /// What: Parses the optional `{path}` from `args`, calls `list_inner`,
    /// converts the result into a `ToolResult`.
    /// Test: `list_dir_lists_entries`, etc.
    async fn execute(&self, args: Value) -> ToolResult {
        let sub_path = args.get("path").and_then(Value::as_str);
        match self.list_inner(sub_path) {
            Ok(listing) => ToolResult::ok(listing),
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

    /// `list_dir` at the root lists files and directories, tagging dirs with `/`.
    ///
    /// Why: Basic listing contract, the core of the complement to #1027.
    /// What: Seeds a file and a subdir, lists the root, asserts both appear with
    /// the directory tagged by a trailing slash.
    /// Test: This test.
    #[tokio::test]
    async fn list_dir_lists_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("main.py"), "x\n").expect("write");
        fs::create_dir_all(tmp.path().join("pkg")).expect("mkdir");
        let tool = ListDirTool::new(tmp.path());
        let result = tool.execute(json!({})).await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        let out = result.content();
        assert!(out.contains("main.py"), "file missing: {out}");
        assert!(out.contains("pkg/"), "dir must be tagged with '/': {out}");
    }

    /// `list_dir` scoped to a subdirectory lists that directory's entries.
    ///
    /// Why: The optional `path` argument narrows the listing.
    /// What: Seeds `pkg/mod.py`, lists `pkg`, asserts `mod.py` appears and the
    /// root's `main.py` does not.
    /// Test: This test.
    #[tokio::test]
    async fn list_dir_scoped_to_subdirectory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("main.py"), "x\n").expect("write");
        fs::create_dir_all(tmp.path().join("pkg")).expect("mkdir");
        fs::write(tmp.path().join("pkg/mod.py"), "y\n").expect("write");
        let tool = ListDirTool::new(tmp.path());
        let result = tool.execute(json!({"path": "pkg"})).await;
        assert!(!result.is_error());
        let out = result.content();
        assert!(out.contains("mod.py"), "pkg/mod.py entry missing: {out}");
        assert!(!out.contains("main.py"), "root file must not appear: {out}");
    }

    /// `list_dir` on an empty directory returns a clear marker.
    ///
    /// Why: An empty listing must be distinguishable from an error.
    /// What: Lists an empty subdir, expects the empty-directory marker.
    /// Test: This test.
    #[tokio::test]
    async fn list_dir_empty_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join("empty")).expect("mkdir");
        let tool = ListDirTool::new(tmp.path());
        let result = tool.execute(json!({"path": "empty"})).await;
        assert!(!result.is_error());
        assert!(
            result.content().contains("empty directory"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `list_dir` on a missing directory returns a not-found error.
    ///
    /// Why: Missing paths must surface as `ToolResult::Error`, not panic.
    /// What: Lists a non-existent subdir, expects a "not found" error.
    /// Test: This test.
    #[tokio::test]
    async fn list_dir_missing_directory_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = ListDirTool::new(tmp.path());
        let result = tool.execute(json!({"path": "does_not_exist"})).await;
        assert!(result.is_error());
        assert!(
            result.content().contains("not found"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `list_dir` on a file (not a directory) returns a clear error.
    ///
    /// Why: Listing a file makes no sense; the error must say so.
    /// What: Seeds a file, lists it as if a directory, expects a not-a-directory
    /// error.
    /// Test: This test.
    #[tokio::test]
    async fn list_dir_on_file_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.txt"), "x\n").expect("write");
        let tool = ListDirTool::new(tmp.path());
        let result = tool.execute(json!({"path": "f.txt"})).await;
        assert!(result.is_error());
        assert!(
            result.content().contains("not a directory"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `list_dir` rejects a `path` that escapes the working directory.
    ///
    /// Why: Path traversal must be blocked at the tool boundary.
    /// What: `execute({path:"../.."})` must return an error.
    /// Test: This test.
    #[tokio::test]
    async fn list_dir_path_traversal_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = ListDirTool::new(tmp.path());
        let result = tool.execute(json!({"path": "../../etc"})).await;
        assert!(result.is_error());
        assert!(
            result.content().contains("escapes"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// The schema declares `path` optional (empty `required` list).
    ///
    /// Why: `list_dir` must work with zero arguments (list the root).
    /// What: Parses `schema()` and asserts `required` is empty.
    /// Test: This test.
    #[test]
    fn schema_has_no_required_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = ListDirTool::new(tmp.path());
        let schema = tool.schema();
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        assert!(required.is_empty(), "list_dir must have no required fields");
    }
}
