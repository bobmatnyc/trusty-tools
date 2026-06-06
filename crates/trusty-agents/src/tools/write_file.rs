//! `write_file` tool — restricted file writer for the documentation agent (#82).
//!
//! Why: The docs-agent needs to produce README and API docs files alongside
//! generated code. Giving it a full shell would be too permissive; a narrowly
//! scoped writer that refuses paths outside the workflow's `out_dir` keeps the
//! blast radius tight while still enabling multi-file output.
//! What: `WriteFileTool { out_dir }` — implements `ToolExecutor`. The `path`
//! argument must be relative and must not contain `..` components; the
//! resolved path must stay within `out_dir`. Parent directories are created
//! automatically, and existing files are overwritten.
//! Test: `write_tool_writes_file`, `write_tool_rejects_absolute_path`,
//! `write_tool_rejects_parent_dir_traversal`, `write_tool_creates_parent_dirs`.

#![allow(dead_code)]

use std::path::{Component, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// Writes text content to a file inside `out_dir`.
///
/// Why: Documentation agents must emit files but must not be able to modify
/// arbitrary project sources. Tying the tool to a single `out_dir` root makes
/// the security surface reviewable.
/// What: Holds the canonicalizable root; `resolve_safe_path` rejects absolute
/// paths, `..` components, and anything that would escape `out_dir`.
/// Test: See the module tests below.
pub struct WriteFileTool {
    /// The permitted root — writes are restricted to this directory tree.
    pub out_dir: PathBuf,
    /// #88: When `Some(rel)`, writes are ADDITIONALLY restricted to a single
    /// relative path within `out_dir`. Used by the wave loop to guarantee a
    /// per-file code-agent can only write to its assigned file. `None` keeps
    /// the original permissive behavior (anywhere under `out_dir`).
    pub allowed_path: Option<PathBuf>,
}

impl WriteFileTool {
    /// Construct a new writer rooted at `out_dir`.
    ///
    /// Why: The caller (main.rs) supplies the per-run output directory so the
    /// tool's authority is scoped to that specific workflow run.
    /// What: Stores the path verbatim. Canonicalization happens lazily in
    /// `resolve_safe_path` so construction never fails even if `out_dir`
    /// doesn't exist yet.
    /// Test: Exercised by every `write_tool_*` test below.
    pub fn new(out_dir: PathBuf) -> Self {
        Self {
            out_dir,
            allowed_path: None,
        }
    }

    /// #88: Restrict writes to a single relative path within `out_dir`.
    ///
    /// Why: The wave-loop code-agent should only be able to write its assigned
    /// file — any other path (including `stubs/...`) must be rejected so the
    /// stub contracts stay pristine across waves.
    /// What: Builder setter storing the allowed relative path. Paths with `..`
    /// components are normalized by `resolve_safe_path` before comparison.
    /// Test: `scoped_write_file_rejects_outside_path`,
    /// `scoped_write_file_allows_assigned_path`.
    pub fn with_allowed_path(mut self, rel_path: PathBuf) -> Self {
        self.allowed_path = Some(rel_path);
        self
    }

    /// Resolve `rel_path` relative to `out_dir`, rejecting traversal attempts.
    ///
    /// Why: Prevents an LLM from writing `../../etc/passwd` or a path like
    /// `/tmp/foo` that would bypass the `out_dir` sandbox. We reject absolute
    /// paths outright and walk components looking for `..` to handle the
    /// common cases without requiring the file to exist yet.
    /// What: Returns the joined absolute path on success; otherwise an error
    /// string describing the rejection.
    /// Test: `write_tool_rejects_absolute_path` and
    /// `write_tool_rejects_parent_dir_traversal`.
    fn resolve_safe_path(&self, rel_path: &str) -> Result<PathBuf, String> {
        if rel_path.is_empty() {
            return Err("path is empty".to_string());
        }
        // Reject absolute paths (both POSIX and Windows-style).
        if rel_path.starts_with('/') || rel_path.starts_with('\\') {
            return Err("absolute paths not allowed".to_string());
        }
        let rel = std::path::Path::new(rel_path);
        if rel.is_absolute() {
            return Err("absolute paths not allowed".to_string());
        }
        // Reject any `..` component.
        for component in rel.components() {
            match component {
                Component::ParentDir => {
                    return Err("path traversal ('..') not allowed".to_string());
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err("absolute paths not allowed".to_string());
                }
                _ => {}
            }
        }

        let candidate = self.out_dir.join(rel);

        // If we can canonicalize out_dir, make sure candidate would also stay
        // inside after stripping `..` resolution. The fallback (when out_dir
        // doesn't exist yet) relies on the component walk above.
        if let Ok(root_canon) = self.out_dir.canonicalize() {
            // Try to canonicalize the closest existing ancestor of candidate
            // so we can compare prefixes even when the file itself is new.
            let mut probe = candidate.clone();
            while !probe.exists() {
                match probe.parent() {
                    Some(parent) => probe = parent.to_path_buf(),
                    None => break,
                }
            }
            if probe.exists()
                && let Ok(probe_canon) = probe.canonicalize()
                && !probe_canon.starts_with(&root_canon)
            {
                return Err(format!("path escapes out_dir: {rel_path}"));
            }
        }

        Ok(candidate)
    }
}

#[async_trait]
impl ToolExecutor for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write text content to a file in the workflow's output directory. Creates parent directories automatically. Use relative paths only (e.g., 'README.md', 'docs/api.md'). Overwrites existing files. Does not allow writing outside out_dir.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path within out_dir (e.g., 'README.md' or 'docs/usage.md')."
                        },
                        "content": {
                            "type": "string",
                            "description": "Full text content to write. Overwrites the file if it exists."
                        }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let path_str = match args.get("path").and_then(Value::as_str) {
            Some(p) if !p.trim().is_empty() => p,
            _ => return ToolResult::err("write_file: 'path' is required and must be non-empty"),
        };
        let content = match args.get("content").and_then(Value::as_str) {
            Some(c) => c,
            None => return ToolResult::err("write_file: 'content' is required"),
        };

        let dest = match self.resolve_safe_path(path_str) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("write_file: {e}")),
        };

        // #88: Enforce single-file scope when `allowed_path` is set. Compare
        // the normalized relative path (not the absolute dest) so tempdir
        // canonicalization differences don't cause false rejections.
        //
        // #91 (CRIT-2): Normalize both sides via `.components().collect()`
        // before comparison. Raw string/PathBuf comparison treats
        // `./src/main.py` and `src/main.py` as distinct, letting an LLM
        // bypass the single-file scope with a leading `./`. Collecting into
        // a PathBuf from components strips `CurDir` ("./") segments and
        // normalizes separators so the check is structural rather than
        // lexical. (ParentDir components were already rejected by
        // `resolve_safe_path`, so they cannot reach this comparison.)
        if let Some(allowed) = &self.allowed_path {
            // Filter out `CurDir` ("./") components explicitly — PathBuf's
            // `FromIterator<Component>` preserves them, so a naive
            // `.components().collect()` would still treat `./src/main.py`
            // as distinct from `src/main.py`.
            let normalize = |p: &std::path::Path| -> PathBuf {
                p.components()
                    .filter(|c| !matches!(c, Component::CurDir))
                    .collect()
            };
            let requested_norm = normalize(std::path::Path::new(path_str));
            let allowed_norm = normalize(allowed);
            if requested_norm != allowed_norm {
                return ToolResult::err(format!(
                    "write_file: you may only write to your assigned file: {} (got {})",
                    allowed.display(),
                    path_str
                ));
            }
        }

        if let Some(parent) = dest.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult::err(format!(
                "write_file: failed to create parent dirs for {}: {e}",
                dest.display()
            ));
        }

        // #129: Atomic write — stage to a sibling .tmp file in the same
        // directory (same filesystem guarantees rename(2) atomicity) then
        // rename over the target. Prevents readers from seeing a partial
        // file if the process crashes mid-write.
        let tmp_path = {
            // Build `<dest>.tmp.<uuid>` so concurrent writers to distinct
            // targets don't collide on a shared `.tmp` suffix. Using UUID
            // avoids relying on PIDs which are not unique across
            // concurrent async tasks.
            let mut t = dest.clone().into_os_string();
            t.push(format!(".tmp.{}", uuid::Uuid::new_v4()));
            PathBuf::from(t)
        };
        if let Err(e) = tokio::fs::write(&tmp_path, content.as_bytes()).await {
            return ToolResult::err(format!(
                "write_file: failed to stage write to {}: {e}",
                tmp_path.display()
            ));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &dest).await {
            // Best-effort cleanup of the stale temp file.
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return ToolResult::err(format!(
                "write_file: failed to atomically rename into place {}: {e}",
                dest.display()
            ));
        }

        // #129: For Rust sources, run `rustfmt` on the written file. Best-effort:
        // failures (rustfmt not installed, syntax error) don't abort the write.
        if dest.extension().and_then(|e| e.to_str()) == Some("rs") {
            let _ = tokio::process::Command::new("rustfmt")
                .arg(&dest)
                .status()
                .await;
        }

        ToolResult::ok(format!("wrote {} bytes to {}", content.len(), path_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("trusty-agents-write-file-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[tokio::test]
    async fn write_tool_writes_file() {
        let dir = tempdir();
        let tool = WriteFileTool::new(dir.clone());
        let r = tool
            .execute(json!({"path": "README.md", "content": "# Hello\n"}))
            .await;
        assert!(!r.is_error(), "unexpected error: {}", r.content());
        let body = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert_eq!(body, "# Hello\n");
    }

    #[tokio::test]
    async fn write_tool_rejects_absolute_path() {
        let dir = tempdir();
        let tool = WriteFileTool::new(dir);
        let r = tool
            .execute(json!({"path": "/etc/passwd", "content": "bad"}))
            .await;
        assert!(r.is_error(), "expected rejection of absolute path");
        assert!(
            r.content().contains("absolute"),
            "error should mention absolute: {}",
            r.content()
        );
    }

    #[tokio::test]
    async fn write_tool_rejects_parent_dir_traversal() {
        let dir = tempdir();
        let tool = WriteFileTool::new(dir);
        let r = tool
            .execute(json!({"path": "../escape.txt", "content": "bad"}))
            .await;
        assert!(r.is_error(), "expected rejection of ../ traversal");
        assert!(
            r.content().contains("traversal") || r.content().contains(".."),
            "error should mention traversal: {}",
            r.content()
        );
    }

    #[tokio::test]
    async fn write_tool_creates_parent_dirs() {
        let dir = tempdir();
        let tool = WriteFileTool::new(dir.clone());
        let r = tool
            .execute(json!({"path": "docs/nested/deep/api.md", "content": "body"}))
            .await;
        assert!(!r.is_error(), "unexpected error: {}", r.content());
        let body = std::fs::read_to_string(dir.join("docs/nested/deep/api.md")).unwrap();
        assert_eq!(body, "body");
    }

    #[tokio::test]
    async fn write_tool_missing_path_arg() {
        let dir = tempdir();
        let tool = WriteFileTool::new(dir);
        let r = tool.execute(json!({"content": "body"})).await;
        assert!(r.is_error());
        assert!(r.content().contains("path"));
    }

    #[tokio::test]
    async fn write_tool_missing_content_arg() {
        let dir = tempdir();
        let tool = WriteFileTool::new(dir);
        let r = tool.execute(json!({"path": "README.md"})).await;
        assert!(r.is_error());
        assert!(r.content().contains("content"));
    }

    #[tokio::test]
    async fn scoped_write_file_rejects_outside_path() {
        // #88: With `allowed_path` set, writes to any other relative path
        // (including `stubs/...`) must return an error instead of succeeding.
        let dir = tempdir();
        let tool = WriteFileTool::new(dir.clone()).with_allowed_path(PathBuf::from("src/main.py"));
        let r = tool
            .execute(json!({"path": "stubs/main.py", "content": "x"}))
            .await;
        assert!(r.is_error(), "expected rejection, got {}", r.content());
        assert!(
            r.content().contains("assigned"),
            "error should mention assigned file: {}",
            r.content()
        );
        // And nothing was written.
        assert!(!dir.join("stubs/main.py").exists());
    }

    #[tokio::test]
    async fn scoped_write_file_allows_assigned_path() {
        // #88: The assigned path writes through normally.
        let dir = tempdir();
        let tool = WriteFileTool::new(dir.clone()).with_allowed_path(PathBuf::from("src/main.py"));
        let r = tool
            .execute(json!({"path": "src/main.py", "content": "body"}))
            .await;
        assert!(!r.is_error(), "unexpected error: {}", r.content());
        let body = std::fs::read_to_string(dir.join("src/main.py")).unwrap();
        assert_eq!(body, "body");
    }

    #[tokio::test]
    async fn scoped_write_file_rejects_dotslash_bypass() {
        // #91 (CRIT-2): A leading `./` previously bypassed the allowed_path
        // check because the comparison was a raw Path equality. After
        // normalization via `.components().collect()` the two paths should
        // compare equal only when structurally identical.
        let dir = tempdir();
        let tool = WriteFileTool::new(dir.clone()).with_allowed_path(PathBuf::from("src/main.py"));
        // `./src/main.py` MUST be accepted (same file, just prefixed).
        let r = tool
            .execute(json!({"path": "./src/main.py", "content": "ok"}))
            .await;
        assert!(
            !r.is_error(),
            "dotslash-prefixed assigned path should pass: {}",
            r.content()
        );
        // But a different file prefixed with `./` must still be rejected.
        let r = tool
            .execute(json!({"path": "./other.py", "content": "bad"}))
            .await;
        assert!(
            r.is_error(),
            "dotslash prefix must not bypass allowed_path check"
        );
        assert!(
            r.content().contains("assigned"),
            "error should mention assigned file: {}",
            r.content()
        );
    }

    #[tokio::test]
    async fn write_tool_overwrites_existing_file() {
        let dir = tempdir();
        let tool = WriteFileTool::new(dir.clone());
        std::fs::write(dir.join("f.txt"), "old").unwrap();
        let r = tool
            .execute(json!({"path": "f.txt", "content": "new"}))
            .await;
        assert!(!r.is_error(), "unexpected error: {}", r.content());
        let body = std::fs::read_to_string(dir.join("f.txt")).unwrap();
        assert_eq!(body, "new");
    }
}
