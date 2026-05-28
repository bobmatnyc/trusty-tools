//! Read-only filesystem tools: `read_file`, `list_dir`, `grep_files`.
//!
//! Why: #34 — an "explorer" agent needs to understand existing code by
//! reading files, listing directories, and searching content, WITHOUT any
//! ability to modify the codebase or execute arbitrary code. Keeping each
//! tool narrow and read-only lets the PM delegate "understand this code"
//! tasks safely.
//! What: Three `ToolExecutor`s, split into focused submodules (#361):
//!   - `ReadFileTool { path }` -> file contents, truncated at 50k chars.
//!   - `ListDirTool { path, show_hidden }` -> one-level directory listing.
//!   - `GrepFilesTool { pattern, path, max_results }` -> ripgrep (if
//!     available) falling back to walkdir+regex.
//! All three reject paths outside the current working directory to prevent
//! path traversal out of the project (see `cwd::resolve_within_cwd`).
//! Test: See the `tests` submodule — each tool has happy-path + traversal-
//! rejection coverage.

mod cwd;
mod grep;
mod list_dir;
mod read_file;

pub use grep::GrepFilesTool;
pub use list_dir::ListDirTool;
pub use read_file::ReadFileTool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::traits::ToolExecutor;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    use super::cwd::{READ_FILE_MAX_CHARS, resolve_within_cwd};

    #[allow(dead_code)]
    fn tempdir() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("open-mpm-fs-reader-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn resolve_accepts_subpath() {
        let cwd = std::env::current_dir().unwrap();
        let target = cwd.join("Cargo.toml");
        assert!(target.exists(), "sanity: Cargo.toml exists");
        let got = resolve_within_cwd("Cargo.toml").expect("should resolve");
        assert!(got.ends_with("Cargo.toml"));
    }

    #[test]
    fn resolve_rejects_traversal() {
        // Parent of the repo should be outside CWD.
        let got = resolve_within_cwd("..");
        assert!(got.is_err(), "parent dir should be rejected");
        let msg = got.unwrap_err();
        assert!(
            msg.contains("escapes") || msg.contains("does not exist"),
            "got: {msg}"
        );
    }

    #[tokio::test]
    async fn read_file_returns_contents() {
        // Use Cargo.toml as a reliable fixture.
        let tool = ReadFileTool::new();
        let r = tool.execute(json!({"path": "Cargo.toml"})).await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(r.content().contains("open-mpm"));
    }

    #[tokio::test]
    async fn read_file_rejects_outside_cwd() {
        let tool = ReadFileTool::new();
        let r = tool.execute(json!({"path": "../"})).await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn read_file_missing_path_arg() {
        let tool = ReadFileTool::new();
        let r = tool.execute(json!({})).await;
        assert!(r.is_error());
        assert!(r.content().contains("path"));
    }

    #[tokio::test]
    async fn read_file_truncates_large_content() {
        // Write a large file into a subdir of CWD, then read it back.
        let cwd = std::env::current_dir().unwrap();
        let target_dir = cwd.join("target").join("_fs_reader_test");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("big.txt");
        let body = "x".repeat(READ_FILE_MAX_CHARS + 1000);
        fs::write(&target, &body).unwrap();

        let rel = pathdiff::diff_or_absolute(&target, &cwd);
        let tool = ReadFileTool::new();
        let r = tool.execute(json!({"path": rel})).await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(r.content().contains("truncated"));
    }

    #[tokio::test]
    async fn list_dir_shows_files() {
        let tool = ListDirTool::new();
        let r = tool.execute(json!({"path": "."})).await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(r.content().contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn list_dir_hides_dotfiles_by_default() {
        // Create a tempdir inside CWD with a visible + hidden file.
        let cwd = std::env::current_dir().unwrap();
        let dir = cwd.join("target").join("_list_dir_dotfile");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("visible.txt"), "v").unwrap();
        fs::write(dir.join(".hidden"), "h").unwrap();

        let rel = pathdiff::diff_or_absolute(&dir, &cwd);
        let tool = ListDirTool::new();
        let default_r = tool.execute(json!({"path": rel.clone()})).await;
        assert!(default_r.content().contains("visible.txt"));
        assert!(!default_r.content().contains(".hidden"));

        let shown_r = tool
            .execute(json!({"path": rel, "show_hidden": true}))
            .await;
        assert!(shown_r.content().contains(".hidden"));
    }

    #[tokio::test]
    async fn list_dir_rejects_outside_cwd() {
        let tool = ListDirTool::new();
        let r = tool.execute(json!({"path": "/"})).await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn grep_files_finds_pattern_fallback() {
        // Use the fallback path by searching inside a known file. We can't
        // easily disable rg per-call, so this test checks *either* rg or
        // walkdir produces a match for a well-known literal.
        let tool = GrepFilesTool::new();
        let r = tool
            .execute(json!({
                "pattern": "open-mpm",
                "path": "Cargo.toml",
                "max_results": 10
            }))
            .await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(r.content().contains("open-mpm"));
    }

    #[tokio::test]
    async fn grep_files_rejects_outside_cwd() {
        let tool = GrepFilesTool::new();
        let r = tool.execute(json!({"pattern": "x", "path": "/etc"})).await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn grep_files_no_match_returns_empty_message() {
        let tool = GrepFilesTool::new();
        let r = tool
            .execute(json!({
                "pattern": "__this_literal_should_not_appear_anywhere__",
                "path": "Cargo.toml"
            }))
            .await;
        assert!(!r.is_error());
        // Either empty-result message from rg branch or walkdir branch.
        assert!(
            r.content().contains("no matches") || r.content().is_empty(),
            "got: {}",
            r.content()
        );
    }

    // Tiny local replacement for the `pathdiff` crate so we don't add a new
    // dependency for a two-function need.
    mod pathdiff {
        use std::path::{Path, PathBuf};

        /// Produce a path expressing `target` relative to `base` if possible,
        /// else return the absolute `target`.
        pub fn diff_or_absolute(target: &Path, base: &Path) -> String {
            match target.strip_prefix(base) {
                Ok(rel) => rel.display().to_string(),
                Err(_) => target.display().to_string(),
            }
        }

        // Keep PathBuf unused import quiet for platforms that lint strictly.
        #[allow(dead_code)]
        fn _unused() -> PathBuf {
            PathBuf::new()
        }
    }
}
