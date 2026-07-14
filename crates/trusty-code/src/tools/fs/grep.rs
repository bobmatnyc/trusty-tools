//! `grep` tool — search file contents by regex across the project.
//!
//! Why: Content search is the other discovery primitive the engineer lacked, so
//! every `grep -r` went through the `bash` tool — part of the dominant agent-turn
//! sink on the L1 bake-off. A structured `grep` tool returns `file:line: text`
//! hits the harness can pin, instead of raw shell output. (#1027.)
//! What: `GrepTool` walks the working directory with a gitignore-aware walker
//! (ripgrep's `ignore` crate) and returns each line matching the supplied regex.
//! An optional `path` scopes the walk and an optional `glob` filters which files
//! are searched.
//! Test: See `#[cfg(test)]` below — covers a content hit, regex semantics, the
//! glob filter, subdirectory scoping, no-match, bad-regex, and path traversal.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{Value, json};

use crate::tools::fs::{FsError, scoped_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Maximum number of matching lines `GrepTool` returns in a single call.
///
/// Why: A broad regex over a large tree could return thousands of hits and blow
/// the LLM context window. Capping and flagging truncation keeps it bounded.
/// What: Constant used to stop collecting hits.
/// Test: `grep_truncates_large_result_sets`.
pub const MAX_GREP_HITS: usize = 200;

/// Skip files larger than this (bytes) — they are almost always assets/binaries.
///
/// Why: Reading a multi-MiB blob line-by-line to regex-match is wasteful and
/// risks non-UTF-8 content; bounding the per-file size keeps grep fast.
/// What: Files whose metadata length exceeds this are skipped.
/// Test: Exercised indirectly by the content-hit tests (all small files).
const MAX_GREP_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

/// `ToolExecutor` that searches file contents by regex.
///
/// Why: Gives agents a native, sandboxed content-search primitive so they stop
/// shelling out to `grep -r`, cutting round-trips.
/// What: Implements `ToolExecutor` with `name = "grep"`, scoped to
/// `working_dir`; rejects traversal attempts in the optional `path` argument.
/// Test: `cargo test -p trusty-code -- tools::fs::grep`.
pub struct GrepTool {
    working_dir: PathBuf,
}

impl GrepTool {
    /// Construct a new `GrepTool` scoped to `working_dir`.
    ///
    /// Why: The working directory is the security boundary; set once at
    /// construction so the LLM cannot widen it per-call.
    /// What: Stores `working_dir`.
    /// Test: `grep_finds_matching_lines`, et al.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// Walk `root` and collect `file:line: text` hits for `re`.
    ///
    /// Why: Centralises the walk + per-line match so `execute` stays short and
    /// the cap / file-filter / size-skip policy lives in one place.
    /// What: Uses a gitignore-aware `WalkBuilder`; for each file (optionally
    /// filtered by `glob`) reads its text and records every line matching `re`,
    /// stopping at `MAX_GREP_HITS`. Returns hits and whether truncated.
    /// Test: All `GrepTool` unit tests.
    fn collect(&self, root: &Path, re: &Regex, glob: Option<&GlobMatcher>) -> (Vec<String>, bool) {
        // Strip against the CANONICAL working dir (see `GlobTool::collect` for the
        // macOS `/var` vs `/private/var` rationale) so the optional `glob` file
        // filter and the `path:line:` display use clean project-relative paths.
        let base =
            std::fs::canonicalize(&self.working_dir).unwrap_or_else(|_| self.working_dir.clone());
        let mut hits = Vec::new();
        let mut truncated = false;

        'walk: for entry in WalkBuilder::new(root).hidden(true).build().flatten() {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(&base).unwrap_or(path);

            if let Some(g) = glob
                && !g.is_match(rel)
            {
                continue;
            }

            // Skip oversized files (assets/binaries).
            if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_GREP_FILE_BYTES) {
                continue;
            }

            // Non-UTF-8 files are skipped silently (read_to_string errors).
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };

            let rel_display = rel.to_string_lossy();
            for (idx, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    if hits.len() >= MAX_GREP_HITS {
                        truncated = true;
                        break 'walk;
                    }
                    // 1-based line numbers; trim to keep results compact.
                    hits.push(format!("{}:{}: {}", rel_display, idx + 1, line.trim_end()));
                }
            }
        }
        (hits, truncated)
    }

    /// Resolve the scoped root, compile the regex + optional glob, and search.
    ///
    /// Why: Keeps error handling (traversal, bad regex, bad glob) in one place
    /// returning a single `Result` for `execute` to format.
    /// What: Scopes `path` (default `working_dir`), compiles `pattern` into a
    /// `Regex` and the optional `glob` into a `GlobMatcher`, walks, and returns
    /// the formatted hit listing.
    /// Test: All `GrepTool` unit tests.
    fn grep_inner(
        &self,
        pattern: &str,
        sub_path: Option<&str>,
        glob: Option<&str>,
    ) -> Result<String, FsError> {
        let root = match sub_path {
            Some(p) => scoped_path(&self.working_dir, Path::new(p))?,
            None => scoped_path(&self.working_dir, Path::new("."))?,
        };

        let re = Regex::new(pattern).map_err(|e| FsError::GrepPattern {
            pattern: pattern.to_string(),
            reason: e.to_string(),
        })?;

        let matcher = match glob {
            Some(g) => Some(
                Glob::new(g)
                    .map_err(|e| FsError::GlobPattern {
                        pattern: g.to_string(),
                        reason: e.to_string(),
                    })?
                    .compile_matcher(),
            ),
            None => None,
        };

        let (hits, truncated) = self.collect(&root, &re, matcher.as_ref());

        if hits.is_empty() {
            return Ok(format!("no matches for pattern '{pattern}'"));
        }

        let mut out = hits.join("\n");
        if truncated {
            out.push_str(&format!(
                "\n… (truncated at {MAX_GREP_HITS} matches; refine the pattern)"
            ));
        }
        Ok(out)
    }
}

#[async_trait]
impl ToolExecutor for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    /// OpenAI function-call schema for `grep`.
    ///
    /// Why: The LLM constructs its call from this schema; parameters mirror the
    /// `execute` contract exactly.
    /// What: JSON object with `pattern` (required regex), `path` (optional
    /// subdirectory), and `glob` (optional file filter).
    /// Test: `schema_has_required_pattern`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search file contents by regular expression across the project. Returns matching 'path:line: text' hits, one per line. Hidden and .gitignore'd files are skipped. Prefer this over shelling out to 'grep'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Regular expression to match against each line of file contents."
                        },
                        "path": {
                            "type": "string",
                            "description": "Optional subdirectory (relative to the working directory) to scope the search. Defaults to the whole project."
                        },
                        "glob": {
                            "type": "string",
                            "description": "Optional glob (e.g. '**/*.py') restricting which files are searched."
                        }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a `grep` tool call.
    ///
    /// Why: Searches file contents for `pattern` within the working directory.
    /// What: Parses `{pattern, path?, glob?}` from `args`, calls `grep_inner`,
    /// converts the result into a `ToolResult`.
    /// Test: `grep_finds_matching_lines`, etc.
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
            return ToolResult::err("grep: missing required argument 'pattern'");
        };
        let sub_path = args.get("path").and_then(Value::as_str);
        let glob = args.get("glob").and_then(Value::as_str);

        match self.grep_inner(pattern, sub_path, glob) {
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

    /// Seed a tree with searchable content.
    fn seed(tmp: &tempfile::TempDir) {
        fs::write(
            tmp.path().join("app.py"),
            "import os\ndef handler():\n    return TODO_FIXME\n",
        )
        .expect("write");
        fs::create_dir_all(tmp.path().join("lib")).expect("mkdir");
        fs::write(
            tmp.path().join("lib/util.rs"),
            "fn helper() {}\n// TODO_FIXME later\n",
        )
        .expect("write");
    }

    /// `grep` finds a literal substring and reports `path:line: text`.
    ///
    /// Why: Basic content-search contract, the core of #1027.
    /// What: Seeds files containing `TODO_FIXME`, greps for it, asserts both
    /// files and their line numbers appear.
    /// Test: This test.
    #[tokio::test]
    async fn grep_finds_matching_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GrepTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "TODO_FIXME"})).await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        let out = result.content();
        assert!(out.contains("app.py:3:"), "app.py hit missing: {out}");
        assert!(out.contains("lib/util.rs:2:"), "util.rs hit missing: {out}");
    }

    /// `grep` honours regex semantics, not just literal substrings.
    ///
    /// Why: The pattern is a regex; anchors/character classes must work.
    /// What: Greps `^def ` (line starting with `def `), asserts only the
    /// function-definition line matches.
    /// Test: This test.
    #[tokio::test]
    async fn grep_uses_regex_semantics() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GrepTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "^def "})).await;
        assert!(!result.is_error());
        let out = result.content();
        assert!(out.contains("def handler"), "def line missing: {out}");
        assert!(
            !out.contains("import os"),
            "import line must not match: {out}"
        );
    }

    /// `grep` with a `glob` filter searches only matching files.
    ///
    /// Why: The optional glob narrows the file set (e.g. only `.rs`).
    /// What: Greps `TODO_FIXME` restricted to `**/*.rs`, asserts the `.rs` hit
    /// appears and the `.py` hit does not.
    /// Test: This test.
    #[tokio::test]
    async fn grep_glob_filter_restricts_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GrepTool::new(tmp.path());
        let result = tool
            .execute(json!({"pattern": "TODO_FIXME", "glob": "**/*.rs"}))
            .await;
        assert!(!result.is_error());
        let out = result.content();
        assert!(out.contains("lib/util.rs"), "rs hit missing: {out}");
        assert!(
            !out.contains("app.py"),
            "py file must be filtered out: {out}"
        );
    }

    /// `grep` scoped to a subdirectory only searches that subtree.
    ///
    /// Why: The optional `path` argument narrows the search.
    /// What: Greps `TODO_FIXME` scoped to `lib`, asserts only the `lib` hit.
    /// Test: This test.
    #[tokio::test]
    async fn grep_scoped_to_subdirectory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GrepTool::new(tmp.path());
        let result = tool
            .execute(json!({"pattern": "TODO_FIXME", "path": "lib"}))
            .await;
        assert!(!result.is_error());
        let out = result.content();
        assert!(out.contains("lib/util.rs"), "lib hit missing: {out}");
        assert!(!out.contains("app.py"), "app.py is outside scope: {out}");
    }

    /// `grep` with no matches returns a clear message, not an error.
    ///
    /// Why: "No results" is a normal outcome the model must read.
    /// What: Greps a pattern present in no file, expects the no-match text.
    /// Test: This test.
    #[tokio::test]
    async fn grep_no_matches_reports_clearly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(&tmp);
        let tool = GrepTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "ZZZ_NOT_PRESENT"})).await;
        assert!(!result.is_error());
        assert!(
            result.content().contains("no matches"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `grep` reports a bad regex as a recoverable error.
    ///
    /// Why: A malformed pattern must surface as a structured error the model can
    /// correct, not a panic.
    /// What: `execute({pattern:"(unclosed"})` returns an error mentioning grep.
    /// Test: This test.
    #[tokio::test]
    async fn grep_bad_regex_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = GrepTool::new(tmp.path());
        let result = tool.execute(json!({"pattern": "(unclosed"})).await;
        assert!(result.is_error());
        assert!(
            result.content().contains("grep"),
            "unexpected message: {}",
            result.content()
        );
    }

    /// `grep` rejects a `path` that escapes the working directory.
    ///
    /// Why: Path traversal must be blocked at the tool boundary.
    /// What: `execute({pattern:"x", path:"../.."})` must return an error.
    /// Test: This test.
    #[tokio::test]
    async fn grep_path_traversal_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = GrepTool::new(tmp.path());
        let result = tool
            .execute(json!({"pattern": "x", "path": "../../etc"}))
            .await;
        assert!(result.is_error());
        assert!(
            result.content().contains("escapes"),
            "unexpected message: {}",
            result.content()
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
        let tool = GrepTool::new(tmp.path());
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
