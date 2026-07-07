//! Per-model edit-format selection for the `edit` tool (#2068, P1B-1).
//!
//! Why: Aider's A/B data (cited in #2068) shows edit-application success rate
//! varies a lot by model family for a given wire format: exact-match
//! SEARCH/REPLACE is the cheapest (fewest tokens) but some models struggle to
//! reproduce `old_string` byte-for-byte; unified-diff is more forgiving of
//! minor context drift but costs more tokens to emit; whole-file replacement
//! is the most robust (no precision needed at all) but the most expensive.
//! Picking a fixed global order wastes retries on models that reliably fail
//! their first attempt. [`format_order_for`] centralises a static per-model
//! preference matrix (Phase 1 — "simple matrix in agent config", full
//! success-rate learning loop deferred to Phase 2) so the tool can offer
//! formats in the order most likely to succeed for the calling model,
//! mirroring the `strategy_order_for` matrix in
//! `llm::tool_call_extractor` (#1023).
//! What: [`EditFormat`] names the three supported wire formats; [`EditPayload`]
//! bundles the format-specific arguments recovered from the LLM's tool call;
//! [`format_order_for`] returns the per-model fallback order; [`select_and_apply`]
//! tries the caller-supplied payloads in that order against the current file
//! content and returns the first one that applies cleanly. [`format_order_for_mode`]
//! and [`select_and_apply_for_mode`] (#2073) are the `HarnessMode`-aware
//! variants that reconcile this per-model matrix with vision spec §5.9's
//! Parity/DailyDriver split (see their own docs).
//!
//! ## Per-model format matrix
//!
//! | Model family (slug substring) | Fallback order |
//! |---|---|
//! | `claude-`, `gpt-`, `gemini-` (flagship, native tool-calling) | SEARCH/REPLACE → unified-diff → whole-file |
//! | `qwen`, `deepseek` | unified-diff → SEARCH/REPLACE → whole-file |
//! | everything else (`gemma`, small/unknown models) | whole-file → SEARCH/REPLACE → unified-diff |
//!
//! Rationale: flagship models reliably reproduce exact substrings, so the
//! cheapest format leads. Qwen/DeepSeek's published chat templates and code
//! training corpora lean heavily on diff-formatted code, so diff application
//! out-performs exact match for that family (per the Aider leaderboard data
//! referenced in #2068). Weaker/unknown models frequently botch both
//! precision-dependent formats, so the most forgiving format (whole-file)
//! leads for the catch-all bucket. This table is the `HarnessMode::DailyDriver`
//! behaviour (#2073, §5.9): production, per-model-optimised edit-format
//! selection. `HarnessMode::Parity` overrides it — see [`format_order_for_mode`].
//! Test: `tests::format_order_*`, `tests::select_and_apply_*`.

mod diff;

use std::path::Path;

use super::FsError;
use crate::mode::HarnessMode;

pub(crate) use diff::apply_unified_diff;

/// The three supported edit wire formats.
///
/// Why: Callers (the tool's `execute`, tests, and future telemetry) need a
/// concrete, comparable value naming which format was attempted/succeeded.
/// What: Mirrors the three formats in #2068's scope.
/// Test: `tests::display_matches_wire_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFormat {
    /// Exact-unique string replacement (the pre-#2068 sole format).
    SearchReplace,
    /// A unified-diff hunk (or hunks) applied against the file's current content.
    UnifiedDiff,
    /// Full-file content replacement.
    WholeFile,
}

impl EditFormat {
    /// The wire/config name used in the matrix table and tool-result messages.
    fn as_str(self) -> &'static str {
        match self {
            EditFormat::SearchReplace => "search_replace",
            EditFormat::UnifiedDiff => "unified_diff",
            EditFormat::WholeFile => "whole_file",
        }
    }
}

impl std::fmt::Display for EditFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One candidate edit, in the shape the LLM actually supplied it.
///
/// Why: A single tool call may carry more than one representation of the same
/// edit (e.g. a model hedges with both `old_string`/`new_string` and a `diff`);
/// [`select_and_apply`] needs a typed payload per format to try in preference
/// order.
/// What: Each variant carries exactly the arguments its format needs.
/// Test: Constructed directly in `edit.rs::execute` from the parsed tool-call
/// arguments; exercised via `tests::select_and_apply_*`.
#[derive(Debug, Clone, PartialEq)]
pub enum EditPayload {
    /// `old_string` must appear exactly once in the file; replaced with `new_string`.
    SearchReplace {
        old_string: String,
        new_string: String,
    },
    /// A unified-diff hunk set to apply against the file's current content.
    UnifiedDiff { diff: String },
    /// The complete new content of the file.
    WholeFile { content: String },
}

impl EditPayload {
    /// The [`EditFormat`] this payload represents.
    fn format(&self) -> EditFormat {
        match self {
            EditPayload::SearchReplace { .. } => EditFormat::SearchReplace,
            EditPayload::UnifiedDiff { .. } => EditFormat::UnifiedDiff,
            EditPayload::WholeFile { .. } => EditFormat::WholeFile,
        }
    }
}

/// Per-model fallback order for edit-format application, when multiple
/// candidate payloads are available.
///
/// Why: Centralises the matrix documented in the module table above.
/// What: Returns the three formats in priority order for `model_slug`.
/// Matching is case-insensitive and substring-based, mirroring
/// `llm::tool_call_extractor::strategy_order_for`.
/// Test: `tests::format_order_prefers_search_replace_for_flagship_models`,
/// `tests::format_order_prefers_unified_diff_for_qwen_and_deepseek`,
/// `tests::format_order_prefers_whole_file_for_catch_all`.
pub fn format_order_for(model_slug: &str) -> [EditFormat; 3] {
    let lower = model_slug.to_ascii_lowercase();
    if lower.contains("claude-") || lower.contains("gpt-") || lower.contains("gemini-") {
        [
            EditFormat::SearchReplace,
            EditFormat::UnifiedDiff,
            EditFormat::WholeFile,
        ]
    } else if lower.contains("qwen") || lower.contains("deepseek") {
        [
            EditFormat::UnifiedDiff,
            EditFormat::SearchReplace,
            EditFormat::WholeFile,
        ]
    } else {
        [
            EditFormat::WholeFile,
            EditFormat::SearchReplace,
            EditFormat::UnifiedDiff,
        ]
    }
}

/// Apply an exact-unique string replacement against in-memory `content`.
///
/// Why: Shared by [`select_and_apply`]; kept free of file IO so it is testable
/// without a tempdir.
/// What: Returns the updated content on success. Errors on 0 or >1 matches,
/// mirroring the pre-#2068 `EditTool` contract exactly.
/// Test: `tests::select_and_apply_search_replace_unique_match`,
/// `tests::select_and_apply_search_replace_zero_and_ambiguous`.
fn apply_search_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    path: &Path,
) -> Result<String, FsError> {
    let count = content.matches(old_string).count();
    match count {
        0 => Err(FsError::EditNotFound {
            path: path.to_path_buf(),
        }),
        1 => Ok(content.replacen(old_string, new_string, 1)),
        n => Err(FsError::EditAmbiguous {
            path: path.to_path_buf(),
            count: n,
        }),
    }
}

/// Apply one payload against `content`, dispatching on its format.
fn apply_payload(payload: &EditPayload, content: &str, path: &Path) -> Result<String, FsError> {
    match payload {
        EditPayload::SearchReplace {
            old_string,
            new_string,
        } => apply_search_replace(content, old_string, new_string, path),
        EditPayload::UnifiedDiff { diff } => apply_unified_diff(content, diff, path),
        EditPayload::WholeFile { content: new } => Ok(new.clone()),
    }
}

/// Try `payloads` against `content` in `model_slug`'s preferred format order,
/// returning the first one that applies cleanly.
///
/// Why: This is the fallback-retry loop #2068 asks for: SEARCH/REPLACE first
/// (for models that prefer it), falling back to unified-diff, then whole-file,
/// all within a single tool call when the model supplied more than one
/// representation of the same edit.
/// What: Iterates [`format_order_for`], and for each preferred format tries
/// the first payload of that format present in `payloads`; returns
/// `(updated_content, format_used)` on the first success. If no payload
/// matches any format (empty `payloads`), or every present payload fails to
/// apply, returns the last encountered `FsError`.
/// Test: `tests::select_and_apply_*`.
pub fn select_and_apply(
    model_slug: &str,
    content: &str,
    path: &Path,
    payloads: &[EditPayload],
) -> Result<(String, EditFormat), FsError> {
    apply_in_order(format_order_for(model_slug), content, path, payloads)
}

/// `HarnessMode`-aware fallback order for edit-format application (#2073,
/// §5.9's reconciliation table).
///
/// Why: The parity spec's benchmark-fairness goal (D2) extends to the edit
/// layer: §5.9's table lists Parity's edit-format column as "unified-diff
/// only", in contrast to DailyDriver's "per-model optimized". The literal
/// reading ("reject every other format outright") would strand a Parity run
/// against any model that cannot reliably emit a diff, which defeats
/// "benchmark fairness" more than it serves it — so this applies the
/// spec's INTENT (edit-format choice must not vary by model in Parity) by
/// returning a FIXED, model-independent order with unified-diff leading,
/// rather than hard-rejecting the other two formats. A caller wanting the
/// stricter literal reading can inspect the returned `EditFormat` and treat
/// anything but `UnifiedDiff` as a policy violation; `select_and_apply_for_mode`
/// itself still falls back so a Parity run can always complete an edit.
/// What: `HarnessMode::Parity` ignores `model_slug` and always returns
/// `[UnifiedDiff, SearchReplace, WholeFile]`. `HarnessMode::DailyDriver`
/// delegates to [`format_order_for`] unchanged (the existing per-model
/// matrix, #2068).
/// Test: `tests::format_order_for_mode_parity_is_model_independent`,
/// `tests::format_order_for_mode_daily_driver_matches_legacy_order`.
pub fn format_order_for_mode(mode: HarnessMode, model_slug: &str) -> [EditFormat; 3] {
    match mode {
        HarnessMode::Parity => [
            EditFormat::UnifiedDiff,
            EditFormat::SearchReplace,
            EditFormat::WholeFile,
        ],
        HarnessMode::DailyDriver => format_order_for(model_slug),
    }
}

/// `HarnessMode`-aware [`select_and_apply`] (#2073).
///
/// Why: The single call site (`EditTool::edit_inner`) that needs to pick
/// between the plain per-model order and the Parity-fixed order without
/// duplicating the apply loop.
/// What: Computes the order via [`format_order_for_mode`], then applies it
/// exactly as [`select_and_apply`] does.
/// Test: `tests::select_and_apply_for_mode_parity_prefers_unified_diff_regardless_of_model`.
pub fn select_and_apply_for_mode(
    mode: HarnessMode,
    model_slug: &str,
    content: &str,
    path: &Path,
    payloads: &[EditPayload],
) -> Result<(String, EditFormat), FsError> {
    apply_in_order(
        format_order_for_mode(mode, model_slug),
        content,
        path,
        payloads,
    )
}

/// Shared apply loop behind both [`select_and_apply`] and
/// [`select_and_apply_for_mode`].
///
/// Why: Both public entry points differ only in how they compute the
/// preference order; centralising the loop keeps that the ONLY difference.
/// What: Tries each format in `order`, applying the first present payload
/// that succeeds; returns the last error if none apply, or `EditNotFound` if
/// `payloads` never contained any of `order`'s formats.
/// Test: Exercised transitively by every `select_and_apply*` test.
fn apply_in_order(
    order: [EditFormat; 3],
    content: &str,
    path: &Path,
    payloads: &[EditPayload],
) -> Result<(String, EditFormat), FsError> {
    let mut last_err: Option<FsError> = None;

    for format in order {
        let Some(payload) = payloads.iter().find(|p| p.format() == format) else {
            continue;
        };
        match apply_payload(payload, content, path) {
            Ok(updated) => return Ok((updated, format)),
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| FsError::EditNotFound {
        path: path.to_path_buf(),
    }))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// `Display` renders the wire-format name used in tool-result messages.
    #[test]
    fn display_matches_wire_name() {
        assert_eq!(EditFormat::SearchReplace.to_string(), "search_replace");
        assert_eq!(EditFormat::UnifiedDiff.to_string(), "unified_diff");
        assert_eq!(EditFormat::WholeFile.to_string(), "whole_file");
    }

    /// Flagship native-tool-calling families prefer SEARCH/REPLACE first.
    #[test]
    fn format_order_prefers_search_replace_for_flagship_models() {
        for slug in [
            "anthropic/claude-opus-4-5",
            "openai/gpt-4o",
            "google/gemini-2.5-pro",
        ] {
            assert_eq!(
                format_order_for(slug),
                [
                    EditFormat::SearchReplace,
                    EditFormat::UnifiedDiff,
                    EditFormat::WholeFile
                ],
                "unexpected order for {slug}"
            );
        }
    }

    /// Qwen/DeepSeek prefer unified-diff first.
    #[test]
    fn format_order_prefers_unified_diff_for_qwen_and_deepseek() {
        for slug in ["qwen/qwen-2.5-72b-instruct", "deepseek/deepseek-chat"] {
            assert_eq!(
                format_order_for(slug),
                [
                    EditFormat::UnifiedDiff,
                    EditFormat::SearchReplace,
                    EditFormat::WholeFile
                ],
                "unexpected order for {slug}"
            );
        }
    }

    /// The catch-all bucket (gemma, unknown, empty slug) prefers whole-file first.
    #[test]
    fn format_order_prefers_whole_file_for_catch_all() {
        for slug in ["google/gemma-2-9b-it", "some/unknown-model", ""] {
            assert_eq!(
                format_order_for(slug),
                [
                    EditFormat::WholeFile,
                    EditFormat::SearchReplace,
                    EditFormat::UnifiedDiff
                ],
                "unexpected order for {slug}"
            );
        }
    }

    /// A single `SearchReplace` payload applies regardless of model order.
    #[test]
    fn select_and_apply_search_replace_unique_match() {
        let payloads = [EditPayload::SearchReplace {
            old_string: "foo".into(),
            new_string: "bar".into(),
        }];
        // Even under the whole-file-first catch-all order, the only payload
        // present (SearchReplace) must still be the one applied.
        let (updated, format) =
            select_and_apply("unknown-model", "foo baz", Path::new("f.py"), &payloads)
                .expect("unique match must apply");
        assert_eq!(updated, "bar baz");
        assert_eq!(format, EditFormat::SearchReplace);
    }

    /// Zero and ambiguous matches surface the same errors as the legacy tool.
    #[test]
    fn select_and_apply_search_replace_zero_and_ambiguous() {
        let zero = [EditPayload::SearchReplace {
            old_string: "missing".into(),
            new_string: "x".into(),
        }];
        let err = select_and_apply("claude-opus", "content", Path::new("f.py"), &zero)
            .expect_err("zero matches must error");
        assert!(matches!(err, FsError::EditNotFound { .. }));

        let ambiguous = [EditPayload::SearchReplace {
            old_string: "dup".into(),
            new_string: "x".into(),
        }];
        let err = select_and_apply("claude-opus", "dup dup", Path::new("f.py"), &ambiguous)
            .expect_err("ambiguous matches must error");
        assert!(matches!(err, FsError::EditAmbiguous { count: 2, .. }));
    }

    /// A `WholeFile` payload always applies, replacing the entire content.
    #[test]
    fn select_and_apply_whole_file_replaces_everything() {
        let payloads = [EditPayload::WholeFile {
            content: "brand new content\n".into(),
        }];
        let (updated, format) =
            select_and_apply("claude-opus", "old content\n", Path::new("f.py"), &payloads)
                .expect("whole-file must always apply");
        assert_eq!(updated, "brand new content\n");
        assert_eq!(format, EditFormat::WholeFile);
    }

    /// When the top-preference payload fails to apply, `select_and_apply`
    /// falls back to the next preferred format present in `payloads`.
    #[test]
    fn select_and_apply_falls_back_on_failure() {
        // Flagship order is SearchReplace -> UnifiedDiff -> WholeFile. Give a
        // SearchReplace payload that cannot match, plus a WholeFile payload
        // that always can; expect the fallback to WholeFile.
        let payloads = [
            EditPayload::SearchReplace {
                old_string: "not-present".into(),
                new_string: "x".into(),
            },
            EditPayload::WholeFile {
                content: "fallback content\n".into(),
            },
        ];
        let (updated, format) = select_and_apply(
            "anthropic/claude-opus-4-5",
            "orig\n",
            Path::new("f.py"),
            &payloads,
        )
        .expect("must fall back to whole-file");
        assert_eq!(updated, "fallback content\n");
        assert_eq!(format, EditFormat::WholeFile);
    }

    /// Empty `payloads` is a clear error, not a panic.
    #[test]
    fn select_and_apply_empty_payloads_errors() {
        let err = select_and_apply("claude-opus", "content", Path::new("f.py"), &[])
            .expect_err("no payloads must error");
        assert!(matches!(err, FsError::EditNotFound { .. }));
    }

    /// `HarnessMode::Parity` returns the SAME fixed order regardless of
    /// `model_slug` (#2073) — proves edit-format selection is model-independent
    /// under Parity, matching every other reconciled layer (prompt assembly,
    /// tool schemas, compaction).
    #[test]
    fn format_order_for_mode_parity_is_model_independent() {
        let expected = [
            EditFormat::UnifiedDiff,
            EditFormat::SearchReplace,
            EditFormat::WholeFile,
        ];
        for slug in [
            "anthropic/claude-opus-4-5",
            "qwen/qwen-2.5-72b-instruct",
            "google/gemma-2-9b-it",
            "",
        ] {
            assert_eq!(
                format_order_for_mode(HarnessMode::Parity, slug),
                expected,
                "Parity order must not vary for {slug}"
            );
        }
    }

    /// `HarnessMode::DailyDriver` delegates verbatim to the pre-#2073
    /// per-model matrix (#2068) — no behavioural change for the production
    /// default.
    #[test]
    fn format_order_for_mode_daily_driver_matches_legacy_order() {
        for slug in [
            "anthropic/claude-opus-4-5",
            "qwen/qwen-2.5-72b-instruct",
            "google/gemma-2-9b-it",
        ] {
            assert_eq!(
                format_order_for_mode(HarnessMode::DailyDriver, slug),
                format_order_for(slug),
                "DailyDriver order must match the legacy per-model matrix for {slug}"
            );
        }
    }

    /// A flagship model slug (SEARCH/REPLACE-first under DailyDriver) must
    /// still prefer unified-diff under Parity when both payloads are present
    /// and the diff applies cleanly (#2073).
    #[test]
    fn select_and_apply_for_mode_parity_prefers_unified_diff_regardless_of_model() {
        let payloads = [
            EditPayload::SearchReplace {
                old_string: "line2".into(),
                new_string: "replaced".into(),
            },
            EditPayload::UnifiedDiff {
                diff: "@@ -2,1 +2,1 @@\n-line2\n+line2-diffed\n".into(),
            },
        ];
        let (updated, format) = select_and_apply_for_mode(
            HarnessMode::Parity,
            "anthropic/claude-opus-4-5",
            "line1\nline2\n",
            Path::new("f.py"),
            &payloads,
        )
        .expect("unified diff must apply");
        assert_eq!(format, EditFormat::UnifiedDiff);
        assert_eq!(updated, "line1\nline2-diffed\n");
    }

    /// Parity still falls back to a present, applicable format when no
    /// unified-diff payload was supplied at all — Parity must never strand a
    /// run that only emitted SEARCH/REPLACE or whole-file.
    #[test]
    fn select_and_apply_for_mode_parity_falls_back_without_a_diff_payload() {
        let payloads = [EditPayload::WholeFile {
            content: "brand new\n".into(),
        }];
        let (updated, format) = select_and_apply_for_mode(
            HarnessMode::Parity,
            "anthropic/claude-opus-4-5",
            "old\n",
            Path::new("f.py"),
            &payloads,
        )
        .expect("whole-file fallback must apply");
        assert_eq!(format, EditFormat::WholeFile);
        assert_eq!(updated, "brand new\n");
    }
}
