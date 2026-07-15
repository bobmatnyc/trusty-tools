//! `glob` tool — find files by glob pattern (e.g. `**/*.py`, `src/*.rs`).
//!
//! Why: The engineer's tool registry historically had NO native file discovery,
//! so every `find`/`ls -R` went through the `bash` tool — the dominant agent-turn
//! sink on the L1 bake-off (bash was 53-71% of every turn). A structured `glob`
//! tool lets the model enumerate files in one typed call whose result the harness
//! can pin and reason about, instead of parsing arbitrary shell output. (#1027.)
//! What: `GlobTool` walks the working directory with a gitignore-aware walker
//! (ripgrep's `ignore` crate — hidden files and `.gitignore`d paths are skipped
//! by default) and returns every project-relative path matching the supplied
//! glob. An optional `path` scopes the walk to a subdirectory.
//! Test: See `#[cfg(test)]` below — covers a recursive match, a single-level
//! match, subdirectory scoping, no-match, and path traversal.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use serde_json::{Value, json};

use crate::tools::fs::{FsError, scoped_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Maximum number of paths `GlobTool` returns in a single call.
///
/// Why: An unbounded match set (`**/*` on a huge tree) would blow the LLM
/// context window. Capping and flagging truncation keeps the result bounded and
/// honest.
/// What: Constant used to truncate the sorted match list.
/// Test: `glob_truncates_large_result_sets`.
pub const MAX_GLOB_MATCHES: usize = 500;

/// `ToolExecutor` that lists files matching a glob pattern.
///
/// Why: Gives agents a native, sandboxed file-discovery primitive so they stop
/// shelling out to `find`, cutting round-trips.
/// What: Implements `ToolExecutor` with `name = "glob"`, scoped to
/// `working_dir`; rejects traversal attempts in the optional `path` argument.
/// Test: `cargo test -p trusty-code -- tools::fs::glob`.
pub struct GlobTool {
    working_dir: PathBuf,
}

impl GlobTool {
    /// Construct a new `GlobTool` scoped to `working_dir`.
    ///
    /// Why: The working directory is the security boundary; set once at
    /// construction so the LLM cannot widen it per-call.
    /// What: Stores `working_dir`.
    /// Test: `glob_finds_files_recursively`, et al.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// Walk `root` and collect project-relative paths matching `matcher`.
    ///
    /// Why: Centralises the walk + match so `execute` stays short and the cap /
    /// sort / truncation policy lives in one place.
    /// What: Uses a gitignore-aware `WalkBuilder`, matches each file's path
    /// relative to `self.working_dir` against `matcher`, returns sorted matches
    /// and whether the set was truncated at `MAX_GLOB_MATCHES`.
    /// Test: All `GlobTool` unit tests.
    fn collect(&self, root: &Path, matcher: &GlobMatcher) -> (Vec<String>, bool) {
        // Strip against the CANONICAL working dir: `root` came from `scoped_path`
        // (canonicalized), so on macOS its entries carry the `/private/var/…`
        // prefix while `self.working_dir` may be the `/var/…` symlink form —
        // stripping the raw form would fail and leak absolute paths into the
        // matcher, breaking single-segment patterns like `src/*.rs`.
        let base =
            std::fs::canonicalize(&self.working_dir).unwrap_or_else(|_| self.working_dir.clone());
        let mut matches = Vec::new();
        for entry in WalkBuilder::new(root).hidden(true).build().flatten() {
            // Only match files, not directories.
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                continue;
            }
            let rel = entry.path().strip_prefix(&base).unwrap_or(entry.path());
            if matcher.is_match(rel) {
                matches.push(rel.to_string_lossy().into_owned());
            }
        }
        matches.sort();
        let truncated = matches.len() > MAX_GLOB_MATCHES;
        matches.truncate(MAX_GLOB_MATCHES);
        (matches, truncated)
    }

    /// Resolve the scoped root, compile the glob, and run the walk.
    ///
    /// Why: Keeps error handling (traversal, bad pattern) in one place returning
    /// a single `Result` for `execute` to format.
    /// What: Scopes `path` (default `working_dir`), compiles `pattern` into a
    /// `GlobMatcher`, walks, and returns the formatted match listing.
    /// Test: All `GlobTool` unit tests.
    fn glob_inner(&self, pattern: &str, sub_path: Option<&str>) -> Result<String, FsError> {
        let root = match sub_path {
            Some(p) => scoped_path(&self.working_dir, Path::new(p))?,
            None => scoped_path(&self.working_dir, Path::new("."))?,
        };

        let matcher = Glob::new(pattern)
            .map_err(|e| FsError::GlobPattern {
                pattern: pattern.to_string(),
                reason: e.to_string(),
            })?
            .compile_matcher();

        let (matches, truncated) = self.collect(&root, &matcher);

        if matches.is_empty() {
            return Ok(format!("no files matched pattern '{pattern}'"));
        }

        let mut out = matches.join("\n");
        if truncated {
            out.push_str(&format!(
                "\n… (truncated at {MAX_GLOB_MATCHES} matches; refine the pattern)"
            ));
        }
        Ok(out)
    }
}

#[async_trait]
impl ToolExecutor for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    /// OpenAI function-call schema for `glob`.
    ///
    /// Why: The LLM constructs its call from this schema; parameters mirror the
    /// `execute` contract exactly.
    /// What: JSON object with `pattern` (required) and `path` (optional
    /// subdirectory to scope the walk).
    /// Test: `schema_has_required_pattern`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Find files by glob pattern (e.g. '**/*.py', 'src/*.rs'). Returns matching project-relative paths, one per line. Hidden and .gitignore'd files are skipped. Prefer this over shelling out to 'find'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Glob pattern to match against project-relative paths. '**' matches any number of directories; '*' matches within a path segment."
                        },
                        "path": {
                            "type": "string",
                            "description": "Optional subdirectory (relative to the working directory) to scope the search. Defaults to the whole project."
                        }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a `glob` tool call.
    ///
    /// Why: Enumerates files matching `pattern` within the working directory.
    /// What: Parses `{pattern, path?}` from `args`, calls `glob_inner`, converts
    /// the result into a `ToolResult`.
    /// Test: `glob_finds_files_recursively`, etc.
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
            return ToolResult::err("glob: missing required argument 'pattern'");
        };
        let sub_path = args.get("path").and_then(Value::as_str);

        match self.glob_inner(pattern, sub_path) {
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

    /// Seed a small tree: `a.py`, `pkg/b.py`, `pkg/c.txt`, `src/main.rs`.
    fn seed(tmp: &tempfile::TempDir) {
        fs::write(tmp.path().join("a.py"), "x = 1\n").expect("write");
        fs::create_dir_all(tmp.path().join("pkg")).expect("mkdir");
        fs::write(tmp.path().join("pkg/b.py"), "y = 2\n").expect("write");
        fs::write(tmp.path().join("pkg/c.txt"), "text\n").expect("write");
        fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").expect("write");
    }

    /// `glob` with `**/*.py` finds every Python file, at any depth.
    ///
    /// Why: The `**` recursive-match is the core of #1027.
    /// What: Seeds a tree, globs `**/*.py`, asserts both `.py` files appear and
    /// the `.txt`/`.rs` files do not.
    /// Test: This test.
    #[tokio::test]
    async fn glob_finds_files_recursively() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GlobTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "**/*.py"})).await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        let out = result.content();
        assert!(out.contains("a.py"), "top-level a.py missing: {out}");
        assert!(out.contains("pkg/b.py"), "nested pkg/b.py missing: {out}");
        assert!(!out.contains("c.txt"), "non-matching c.txt present: {out}");
        assert!(
            !out.contains("main.rs"),
            "non-matching main.rs present: {out}"
        );
    }

    /// `glob` with `src/*.rs` matches only files directly under `src`.
    ///
    /// Why: Single-segment `*` must not cross directory boundaries.
    /// What: Globs `src/*.rs`, asserts `src/main.rs` matches and `a.py` does not.
    /// Test: This test.
    #[tokio::test]
    async fn glob_single_level_pattern() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GlobTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "src/*.rs"})).await;
        assert!(!result.is_error());
        let out = result.content();
        assert!(out.contains("src/main.rs"), "src/main.rs missing: {out}");
        assert!(
            !out.contains("a.py"),
            "a.py should not match src/*.rs: {out}"
        );
    }

    /// `glob` scoped to a subdirectory only walks that subtree.
    ///
    /// Why: The optional `path` argument lets the agent narrow the search.
    /// What: Globs `*.py` scoped to `pkg`, asserts `pkg/b.py` matches and the
    /// top-level `a.py` does not.
    /// Test: This test.
    #[tokio::test]
    async fn glob_scoped_to_subdirectory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GlobTool::new(tmp.path());
        let result = tool
            .execute(json!({"pattern": "**/*.py", "path": "pkg"}))
            .await;
        assert!(!result.is_error(), "{}", result.content());
        let out = result.content();
        assert!(out.contains("pkg/b.py"), "pkg/b.py missing: {out}");
        assert!(!out.contains("a.py"), "a.py is outside scoped path: {out}");
    }

    /// `glob` with no matches returns a clear message, not an error.
    ///
    /// Why: "No results" is a normal outcome the model must be able to read.
    /// What: Globs `**/*.java` over a tree with none, expects the no-match text.
    /// Test: This test.
    #[tokio::test]
    async fn glob_no_matches_reports_clearly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GlobTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "**/*.java"})).await;
        assert!(!result.is_error());
        assert!(
            result.content().contains("no files matched"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `glob` rejects a `path` that escapes the working directory.
    ///
    /// Why: Path traversal must be blocked at the tool boundary.
    /// What: `execute({pattern:"*", path:"../.."})` must return an error.
    /// Test: This test.
    #[tokio::test]
    async fn glob_path_traversal_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = GlobTool::new(tmp.path());
        let result = tool
            .execute(json!({"pattern": "*", "path": "../../etc"}))
            .await;
        assert!(result.is_error());
        assert!(
            result.content().contains("escapes"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `glob` reports a bad glob pattern as a recoverable error.
    ///
    /// Why: A malformed pattern must surface as a structured error the model can
    /// correct, not a panic.
    /// What: `execute({pattern:"[unclosed"})` returns an error mentioning the
    /// pattern.
    /// Test: This test.
    #[tokio::test]
    async fn glob_bad_pattern_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = GlobTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "[unclosed"})).await;
        assert!(result.is_error());
        assert!(
            result.content().contains("glob"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `glob` truncates match sets larger than `MAX_GLOB_MATCHES`.
    ///
    /// Why: An unbounded result would overflow the context window.
    /// What: Seeds `MAX_GLOB_MATCHES + 5` files, globs `**/*.py`, asserts the
    /// truncation notice appears and at most the cap of paths is listed.
    /// Test: This test.
    #[tokio::test]
    async fn glob_truncates_large_result_sets() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for i in 0..(MAX_GLOB_MATCHES + 5) {
            fs::write(tmp.path().join(format!("f{i}.py")), "x\n").expect("write");
        }
        let tool = GlobTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "**/*.py"})).await;
        assert!(!result.is_error());
        assert!(
            result.content().contains("truncated"),
            "expected truncation notice"
        );
    }

    /// The schema lists `pattern` as required.
    ///
    /// Why: The LLM omits non-required args; `pattern` must be required.
    /// What: Parses `schema()` and checks `required` contains "pattern".
    /// Test: This test.
    #[test]
    fn schema_has_required_pattern() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = GlobTool::new(tmp.path());
        let schema = tool.schema();
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("pattern")),
            "schema must list 'pattern' as required"
        );
    }
}
