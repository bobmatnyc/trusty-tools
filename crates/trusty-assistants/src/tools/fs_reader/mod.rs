//! The read-only `grep_files` tool, kept as `vector_search`'s fallback.
//!
//! Why: #34 — an "explorer" agent needs to understand existing code by
//! reading files, listing directories, and searching content, WITHOUT any
//! ability to modify the codebase or execute arbitrary code. Keeping each
//! tool narrow and read-only lets the PM delegate "understand this code"
//! tasks safely.
//! What: `GrepFilesTool { pattern, path, max_results }` -> ripgrep (if
//! available) falling back to walkdir+regex. It rejects paths outside the
//! current working directory (see `cwd::resolve_within_cwd`). #11632:
//! `read_file` and `list_dir` were cut with the coding-tool surface; grep
//! stays because `tools::memory::vector_search` falls back to it.
//! Test: See the `tests` submodule — each tool has happy-path + traversal-
//! rejection coverage.

mod cwd;
mod grep;

pub use grep::GrepFilesTool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::traits::ToolExecutor;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    use super::cwd::resolve_within_cwd;

    #[allow(dead_code)]
    fn tempdir() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("trusty-agents-fs-reader-{}", uuid::Uuid::new_v4()));
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
    async fn grep_files_finds_pattern_fallback() {
        // Use the fallback path by searching inside a known file. We can't
        // easily disable rg per-call, so this test checks *either* rg or
        // walkdir produces a match for a well-known literal.
        let tool = GrepFilesTool::new();
        let r = tool
            .execute(json!({
                "pattern": "trusty-agents",
                "path": "Cargo.toml",
                "max_results": 10
            }))
            .await;
        assert!(!r.is_error(), "unexpected: {}", r.content());
        assert!(r.content().contains("trusty-agents"));
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
}
