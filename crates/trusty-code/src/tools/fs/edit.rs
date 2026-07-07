//! `edit` tool — per-model edit-format selection (#2068).
//!
//! Why: Agents that iteratively refine code need a surgical replace-in-place
//! primitive that is safer than re-writing the whole file from memory. Before
//! #2068 the only supported wire format was exact-unique SEARCH/REPLACE,
//! mirroring the Claude Code `Edit` tool contract. Aider's A/B data shows edit
//! success rate varies by model family and format (see
//! `tools::fs::edit_format`'s module docs for the matrix): some models
//! reliably reproduce an exact substring, others do much better with a
//! unified diff or a full-file rewrite. `EditTool` now accepts any of the
//! three formats in one call and applies them in the calling model's
//! preferred order via `edit_format::select_and_apply`.
//! What: `EditTool` reads the file, builds an `EditPayload` for each format
//! present in the tool-call arguments (`old_string`+`new_string`, `diff`, or
//! `content`), and delegates to `edit_format::select_and_apply` to pick and
//! apply the best-fit format, then writes the result back. All paths are
//! scoped to the working directory.
//! Test: See `#[cfg(test)]` below — covers each format, zero/ambiguous
//! SEARCH/REPLACE errors, malformed-diff errors, and path traversal.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::mode::HarnessMode;
use crate::tools::fs::edit_format::{
    EditFormat, EditPayload, select_and_apply, select_and_apply_for_mode,
};
use crate::tools::fs::{FsError, scoped_path};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `ToolExecutor` that applies an edit using whichever wire format(s) the
/// caller supplied, in the calling model's preferred order.
///
/// Why: Surgical in-place edits are the primary mutation primitive for coding
/// agents. Supporting all three #2068 formats behind one tool keeps the
/// registry/schema surface simple while letting the caller (or a future
/// repair loop) hand the model whichever format it is more likely to succeed
/// with.
/// What: Implements `ToolExecutor` with `name = "edit"`. Reads the file,
/// builds the present `EditPayload`s, calls `edit_format::select_and_apply`
/// with `self.model_slug`, and writes the result back.
/// Test: `cargo test -p trusty-code -- tools::fs::edit`.
pub struct EditTool {
    working_dir: PathBuf,
    model_slug: Option<String>,
    /// The resolved `HarnessMode` for this tool's owning run (#2073). `None`
    /// (the default, and every pre-#2073 call site) preserves the exact
    /// pre-#2073 behaviour: always the plain per-model order via
    /// `edit_format::select_and_apply`. `Some(mode)` routes through
    /// `edit_format::select_and_apply_for_mode`, whose `HarnessMode::Parity`
    /// arm requires a `diff` payload and does NOT fall back to SEARCH/REPLACE
    /// or whole-file (owner-tightened 2026-07-07, §5.9's edit-format
    /// reconciliation read literally for benchmark fairness).
    mode: Option<HarnessMode>,
}

impl EditTool {
    /// Construct a new `EditTool` scoped to `working_dir`.
    ///
    /// Why: The working directory is the security boundary set at construction.
    /// What: Stores `working_dir`. `model_slug` defaults to `None`, which
    /// selects the catch-all format order (see `edit_format::format_order_for`);
    /// this has no effect on a call that supplies only one format.
    /// Test: `edit_replaces_unique_match`, et al.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            model_slug: None,
            mode: None,
        }
    }

    /// Set the model slug used to look up the per-model format fallback order.
    ///
    /// Why: The registry factory resolves the calling agent's model slug
    /// (`provider::resolve_model`) once per delegation; threading it through
    /// here lets `edit_format::select_and_apply` pick the best-fit format when
    /// a tool call supplies more than one representation of the same edit.
    /// What: Stores `slug` for use in `execute`.
    /// Test: `edit_falls_back_to_whole_file_when_search_replace_fails`.
    pub fn with_model_slug(mut self, slug: impl Into<String>) -> Self {
        self.model_slug = Some(slug.into());
        self
    }

    /// Set the resolved `HarnessMode` this tool's owning run uses (#2073).
    ///
    /// Why: `HarnessMode::Parity` must score a model's raw diff-editing
    /// ability, unforgivingly — without this the tool always used the
    /// per-model fallback matrix even in Parity, letting a model "pass" an
    /// edit via SEARCH/REPLACE or whole-file without ever demonstrating a
    /// valid diff (owner decision, 2026-07-07: this would defeat M3's model
    /// bake-off). Every call site that does not opt in (`mode` stays `None`)
    /// keeps the exact pre-#2073 behaviour.
    /// What: Stores `mode` for use in `execute`/`edit_inner`.
    /// Test: `edit_under_parity_mode_applies_a_valid_diff`,
    /// `edit_under_parity_mode_errors_without_a_diff_payload`,
    /// `edit_under_daily_driver_mode_matches_legacy_model_based_order`.
    pub fn with_mode(mut self, mode: HarnessMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Perform the edit: read the file, apply the best-fit payload, write back.
    ///
    /// Why: Centralises the IO so `execute` stays focused on argument parsing.
    /// What: Returns the `EditFormat` that was actually applied. Errors
    /// propagate from `scoped_path`, file IO, or the underlying
    /// `edit_format::select_and_apply`/`select_and_apply_for_mode` call.
    /// `self.mode` (#2073) picks which of the two: `None` (unset) preserves
    /// the exact pre-#2073 plain per-model selection; `Some(mode)` routes
    /// through the mode-aware variant, whose `Parity` arm accepts ONLY a
    /// `diff` payload and returns a recoverable
    /// `FsError::ParityDiffRequired` (never a panic) when none was supplied
    /// or the diff fails to apply — no fallback to SEARCH/REPLACE or
    /// whole-file, even then.
    /// Test: All `EditTool` unit tests exercise this path.
    fn edit_inner(
        &self,
        path: &std::path::Path,
        payloads: &[EditPayload],
    ) -> Result<EditFormat, FsError> {
        let scoped = scoped_path(&self.working_dir, path)?;

        let content = std::fs::read_to_string(&scoped).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FsError::NotFound(scoped.clone())
            } else {
                FsError::io(&scoped, e)
            }
        })?;

        let model_slug = self.model_slug.as_deref().unwrap_or("");
        let (updated, format) = match self.mode {
            Some(mode) => select_and_apply_for_mode(mode, model_slug, &content, &scoped, payloads)?,
            None => select_and_apply(model_slug, &content, &scoped, payloads)?,
        };
        std::fs::write(&scoped, updated).map_err(|e| FsError::io(&scoped, e))?;
        Ok(format)
    }
}

#[async_trait]
impl ToolExecutor for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    /// OpenAI function-call schema for `edit`.
    ///
    /// Why: The LLM uses this schema to construct its tool call. `path` is the
    /// only unconditionally required field (#2068); exactly one edit format's
    /// fields must also be present, validated at execution time since JSON
    /// Schema cannot express "one of these three field groups" cleanly here.
    /// What: JSON object with `path` plus the three optional per-format field
    /// groups: `old_string`+`new_string` (SEARCH/REPLACE), `diff`
    /// (unified-diff), `content` (whole-file).
    /// Test: `schema_requires_only_path`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Edit a file using one of three formats: (1) SEARCH/REPLACE — provide old_string+new_string; old_string must appear exactly once in the file (cheapest, preferred when you can reproduce it exactly); (2) unified-diff — provide diff as one or more '@@ -l,s +l,s @@' hunks (use when context is easier to express as a diff than an exact substring); (3) whole-file — provide content as the file's complete new contents (most robust, most expensive; use as a last resort). Provide exactly one format's fields. The path must be inside the working directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative or absolute path to the file (must be inside the working directory)."
                        },
                        "old_string": {
                            "type": "string",
                            "description": "SEARCH/REPLACE format: the exact string to replace. Must appear exactly once in the file. Requires new_string."
                        },
                        "new_string": {
                            "type": "string",
                            "description": "SEARCH/REPLACE format: the replacement string. Requires old_string."
                        },
                        "diff": {
                            "type": "string",
                            "description": "Unified-diff format: one or more '@@ -l,s +l,s @@' hunks to apply against the file's current content."
                        },
                        "content": {
                            "type": "string",
                            "description": "Whole-file format: the complete new content of the file."
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute an `edit` tool call.
    ///
    /// Why: Applies whichever edit format(s) the caller supplied, in the
    /// model's preferred order.
    /// What: Builds an `EditPayload` for each format present in `args`
    /// (`old_string`+`new_string`, `diff`, `content`), errors if none are
    /// present or `old_string`/`new_string` are only partially supplied, then
    /// calls `edit_inner` and converts the result into a `ToolResult`.
    /// Test: `edit_replaces_unique_match`, `edit_errors_on_zero_matches`,
    /// `edit_applies_unified_diff`, `edit_applies_whole_file`, etc.
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(path_str) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("edit: missing required argument 'path'");
        };

        let mut payloads = Vec::new();
        match (
            args.get("old_string").and_then(Value::as_str),
            args.get("new_string").and_then(Value::as_str),
        ) {
            (Some(old_string), Some(new_string)) => payloads.push(EditPayload::SearchReplace {
                old_string: old_string.to_string(),
                new_string: new_string.to_string(),
            }),
            (None, None) => {}
            _ => {
                return ToolResult::err(
                    "edit: 'old_string' and 'new_string' must be provided together",
                );
            }
        }
        if let Some(diff) = args.get("diff").and_then(Value::as_str) {
            payloads.push(EditPayload::UnifiedDiff {
                diff: diff.to_string(),
            });
        }
        if let Some(content) = args.get("content").and_then(Value::as_str) {
            payloads.push(EditPayload::WholeFile {
                content: content.to_string(),
            });
        }
        if payloads.is_empty() {
            return ToolResult::err(
                "edit: must provide either 'old_string'+'new_string', 'diff', or 'content'",
            );
        }

        match self.edit_inner(std::path::Path::new(path_str), &payloads) {
            Ok(format) => ToolResult::ok(format!("edited {path_str} ({format})")),
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

    /// The schema requires only `path`; all three edit-format field groups are
    /// optional at the schema level (validated at execution time instead).
    ///
    /// Why: #2068 lets a call supply SEARCH/REPLACE, unified-diff, or
    /// whole-file fields — JSON Schema cannot cleanly express "exactly one of
    /// these three field groups", so only the universally-required `path` is
    /// listed, and every format's properties are documented but optional.
    /// What: Parses `schema()` and checks `required == ["path"]`, and that all
    /// five properties (`path`, `old_string`, `new_string`, `diff`, `content`)
    /// are present.
    /// Test: This test.
    #[test]
    fn schema_requires_only_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(&tmp);
        let schema = tool.schema();
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert_eq!(names, vec!["path"], "only 'path' must be required");

        let properties = schema["function"]["parameters"]["properties"]
            .as_object()
            .expect("properties object");
        for key in ["path", "old_string", "new_string", "diff", "content"] {
            assert!(properties.contains_key(key), "missing property '{key}'");
        }
    }

    /// `edit` applies a unified-diff payload when `diff` is supplied.
    ///
    /// Why: Guard the second #2068 format end-to-end through `execute`.
    /// What: Write a two-line file, execute `edit` with a `diff` argument
    /// replacing one line, and assert the file was patched.
    /// Test: This test.
    #[tokio::test]
    async fn edit_applies_unified_diff() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "line1\nline2\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({
                "path": "f.py",
                "diff": "@@ -2,1 +2,1 @@\n-line2\n+line2-changed\n"
            }))
            .await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert!(result.content().contains("unified_diff"));
        let updated = fs::read_to_string(tmp.path().join("f.py")).expect("read");
        assert_eq!(updated, "line1\nline2-changed\n");
    }

    /// `edit` applies a whole-file payload when `content` is supplied.
    ///
    /// Why: Guard the third #2068 format (last-resort, most-robust) end-to-end.
    /// What: Write a file, execute `edit` with a `content` argument, and
    /// assert the file's entire content was replaced.
    /// Test: This test.
    #[tokio::test]
    async fn edit_applies_whole_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "old content\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({
                "path": "f.py",
                "content": "brand new file\n"
            }))
            .await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert!(result.content().contains("whole_file"));
        let updated = fs::read_to_string(tmp.path().join("f.py")).expect("read");
        assert_eq!(updated, "brand new file\n");
    }

    /// `edit` errors when `old_string` is supplied without `new_string`.
    ///
    /// Why: A half-supplied SEARCH/REPLACE pair is an unambiguous caller
    /// mistake and must be rejected before touching the filesystem.
    /// What: `execute` with only `old_string` set must return a recoverable error.
    /// Test: This test.
    #[tokio::test]
    async fn edit_errors_on_partial_search_replace_pair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "x = 1\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool
            .execute(json!({"path": "f.py", "old_string": "x = 1"}))
            .await;
        assert!(result.is_error());
        assert!(result.content().contains("together"));
    }

    /// `edit` errors when none of the three format field groups are supplied.
    ///
    /// Why: Nothing to apply is a caller mistake, not a silent no-op.
    /// What: `execute` with only `path` must return a recoverable error.
    /// Test: This test.
    #[tokio::test]
    async fn edit_errors_when_no_format_supplied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "x = 1\n").expect("write");
        let tool = make_tool(&tmp);
        let result = tool.execute(json!({"path": "f.py"})).await;
        assert!(result.is_error());
        assert!(result.content().contains("must provide"));
    }

    /// With a model slug set, a failing SEARCH/REPLACE payload falls back to a
    /// concurrently-supplied whole-file payload within the same call.
    ///
    /// Why: This is the #2068 fallback-ordering acceptance criterion exercised
    /// through the actual `ToolExecutor::execute` entry point, not just the
    /// underlying `edit_format::select_and_apply` unit tests.
    /// What: Give a flagship model slug (SEARCH/REPLACE-first order), a
    /// non-matching `old_string`/`new_string` pair, and a `content` fallback;
    /// assert the whole-file fallback was applied.
    /// Test: This test.
    #[tokio::test]
    async fn edit_falls_back_to_whole_file_when_search_replace_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "original\n").expect("write");
        let tool = EditTool::new(tmp.path()).with_model_slug("anthropic/claude-opus-4-5");
        let result = tool
            .execute(json!({
                "path": "f.py",
                "old_string": "not-present-in-file",
                "new_string": "x",
                "content": "fallback content\n"
            }))
            .await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert!(result.content().contains("whole_file"));
        let updated = fs::read_to_string(tmp.path().join("f.py")).expect("read");
        assert_eq!(updated, "fallback content\n");
    }

    /// Under `HarnessMode::Parity`, a flagship model slug (which would prefer
    /// SEARCH/REPLACE first under the plain per-model matrix) still applies
    /// ONLY the unified-diff payload when both are supplied — the
    /// SEARCH/REPLACE payload is never even attempted (#2073, tightened
    /// 2026-07-07: Parity's edit-format is strict unified-diff only, not
    /// merely model-independent).
    #[tokio::test]
    async fn edit_under_parity_mode_applies_a_valid_diff() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "line1\nline2\n").expect("write");
        let tool = EditTool::new(tmp.path())
            .with_model_slug("anthropic/claude-opus-4-5")
            .with_mode(crate::mode::HarnessMode::Parity);

        let result = tool
            .execute(json!({
                "path": "f.py",
                // Deliberately non-matching, so a SEARCH/REPLACE-first order
                // would fail; only the diff below can actually apply. Under
                // strict Parity this payload is never even attempted.
                "old_string": "not-present",
                "new_string": "x",
                "diff": "@@ -2,1 +2,1 @@\n-line2\n+line2-diffed\n"
            }))
            .await;

        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert!(result.content().contains("unified_diff"));
        let updated = fs::read_to_string(tmp.path().join("f.py")).expect("read");
        assert_eq!(updated, "line1\nline2-diffed\n");
    }

    /// Under `HarnessMode::Parity`, an `edit` call that supplies only
    /// SEARCH/REPLACE (no `diff`) must return a RECOVERABLE tool error — the
    /// same `ToolResult::err` mechanism every other edit failure uses, never
    /// a panic — whose message makes clear Parity requires a unified-diff
    /// edit, and must NOT silently succeed via the fallback matrix (#2073,
    /// tightened 2026-07-07: the model is scored as failing the edit, per
    /// the owner's benchmark-fairness decision).
    #[tokio::test]
    async fn edit_under_parity_mode_errors_without_a_diff_payload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "line1\nline2\n").expect("write");
        let tool = EditTool::new(tmp.path())
            .with_model_slug("anthropic/claude-opus-4-5")
            .with_mode(crate::mode::HarnessMode::Parity);

        // A payload that WOULD succeed under DailyDriver's fallback matrix
        // (unique old_string match) — under Parity it must never be tried.
        let result = tool
            .execute(json!({
                "path": "f.py",
                "old_string": "line2",
                "new_string": "line2-replaced"
            }))
            .await;

        assert!(
            result.is_error(),
            "Parity must reject an edit with no diff payload, got: {}",
            result.content()
        );
        assert!(
            result.content().contains("unified-diff"),
            "error must make the Parity requirement clear: {}",
            result.content()
        );
        let unchanged = fs::read_to_string(tmp.path().join("f.py")).expect("read");
        assert_eq!(
            unchanged, "line1\nline2\n",
            "the file must be untouched when Parity rejects the edit"
        );
    }

    /// Under `HarnessMode::DailyDriver`, `with_mode` must produce the exact
    /// same outcome as the pre-#2073 default (no `with_mode` call at all) —
    /// the mode-aware path is a no-op wrapper around the legacy per-model
    /// matrix for this mode.
    #[tokio::test]
    async fn edit_under_daily_driver_mode_matches_legacy_model_based_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("f.py"), "original\n").expect("write");
        let tool = EditTool::new(tmp.path())
            .with_model_slug("anthropic/claude-opus-4-5")
            .with_mode(crate::mode::HarnessMode::DailyDriver);

        let result = tool
            .execute(json!({
                "path": "f.py",
                "old_string": "not-present-in-file",
                "new_string": "x",
                "content": "fallback content\n"
            }))
            .await;

        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert!(result.content().contains("whole_file"));
        let updated = fs::read_to_string(tmp.path().join("f.py")).expect("read");
        assert_eq!(updated, "fallback content\n");
    }
}
