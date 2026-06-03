//! `MapUnit` — the per-file (or per-hunk-chunk) review work unit for the
//! map-reduce path (Phase 2, #696 / #680).
//!
//! Why: the map stage needs a uniform, typed description of each slice of work
//! so it can decide whether to call the LLM (`Review`) or skip it
//! (`MetadataOnly`), and to carry the relevant diff text without re-rendering
//! it from the raw `FilteredDiff` at call time.
//!
//! What: `MapUnitKind` distinguishes reviewable diff text from metadata-only
//! slots; `MapUnit` bundles the file path, git status, kind, char count, and
//! hunk-chunk metadata into a single value.
//!
//! Test: `map_unit_is_metadata_only`, `map_unit_char_count`, and the
//! comprehensive splitter tests in `splitter_tests.rs`.

// ─── MapUnitKind ─────────────────────────────────────────────────────────────

/// Discriminates reviewable diff text from metadata-only (no-LLM) slots.
///
/// Why: the map stage must skip the LLM call for deleted/binary/rename-only/
/// summary-only files (biggest cost saver on refactor PRs, per §2.1 of the
/// design doc).  Having a typed variant — rather than a boolean flag — makes
/// the match exhaustive and keeps the two arms' data separate.
/// What: `Review` carries the rendered diff text to send to the LLM;
/// `MetadataOnly` carries a brief human-readable note about why no LLM call
/// is made (used in the reduce stage's partial-result banner).
/// Test: `map_unit_is_metadata_only` checks the predicate helpers on both
/// variants; the splitter tests (splitter_tests.rs) exercise the full
/// classification logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapUnitKind {
    /// A reviewable diff slice — send this to the LLM reviewer.
    Review {
        /// Rendered diff text for this unit (bounded to the per-file char
        /// budget).  May include a truncation marker when a single hunk
        /// exceeded the budget.
        diff_text: String,
    },
    /// No LLM call needed — file is deleted, binary, rename-only, or
    /// summary-only (fixture/i18n artefact).
    MetadataOnly {
        /// Short note describing why this unit needs no review
        /// (e.g. `"deleted file"`, `"binary file"`, `"rename-only"`).
        note: String,
    },
}

// ─── MapUnit ─────────────────────────────────────────────────────────────────

/// One reviewable slice of a PR — the primary input type for the map stage.
///
/// Why: the map stage fans out one `MapUnit` per item; a uniform owned value
/// (rather than a borrow into `FilteredDiff`) lets the fan-out move units
/// across async tasks without lifetime constraints.
/// What: bundles the file path, git status string (`"added"` / `"modified"` /
/// `"renamed"` / `"removed"`), the `MapUnitKind` (Review vs MetadataOnly),
/// the char count of the diff text (pre-computed to avoid re-scanning), and
/// optional hunk-chunk metadata for oversized files that were sub-chunked.
///
/// **Chunk fields**: for files that fit in one unit, `chunk_index = 0` and
/// `chunk_total = 1`.  For sub-chunked files, `chunk_index` is 0-based and
/// `chunk_total` is the total number of chunks emitted for that file.  The
/// reduce stage groups units by `file` and reconciles chunks by taking the
/// stricter verdict.
///
/// Test: constructed directly in splitter unit tests; `map_unit_char_count`
/// checks `diff_char_count` consistency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapUnit {
    /// File path, as it appears in the `+++ b/` diff header (no leading `/`).
    pub file: String,

    /// Git status from the DiffAnalyzer: `"added"`, `"modified"`,
    /// `"renamed"`, or `"removed"`.
    pub status: String,

    /// Whether this unit carries reviewable diff text or is metadata-only.
    pub kind: MapUnitKind,

    /// Pre-computed char count of the diff text (0 for `MetadataOnly` units).
    ///
    /// Why: the splitter uses this to enforce the total-char-budget limit
    /// without re-scanning the diff text; the map stage can use it for
    /// per-call telemetry.
    pub diff_char_count: usize,

    /// 0-based index within the sub-chunked sequence for this file.
    /// Always `0` for non-chunked (whole-file) units.
    pub chunk_index: usize,

    /// Total number of chunks emitted for this file.
    /// Always `1` for non-chunked (whole-file) units.
    pub chunk_total: usize,

    /// True when this chunk was flagged oversized — i.e. a single hunk alone
    /// exceeded the per-file char budget and could not be further split.
    ///
    /// Why: the reduce stage surfaces this flag in `MapReduceStats` so a
    /// partial review is never silently treated as complete (analogous to the
    /// existing `[DIFF TRUNCATED …]` honesty marker).
    pub hunk_oversized: bool,
}

impl MapUnit {
    /// Returns `true` if this unit requires no LLM call.
    ///
    /// Why: the map stage must gate the LLM call on this predicate to avoid
    /// spending tokens on deleted/binary/summary-only files.
    /// What: returns `true` iff `kind` is `MetadataOnly`.
    /// Test: `map_unit_is_metadata_only`.
    pub fn is_metadata_only(&self) -> bool {
        matches!(self.kind, MapUnitKind::MetadataOnly { .. })
    }

    /// Returns the diff text slice, or `None` for metadata-only units.
    ///
    /// Why: convenience for the map stage — avoids a match arm when only the
    /// diff text is needed.
    /// What: returns `Some(&diff_text)` for `Review` units, `None` otherwise.
    /// Test: covered implicitly by splitter tests that assert on diff text
    /// contents.
    pub fn diff_text(&self) -> Option<&str> {
        match &self.kind {
            MapUnitKind::Review { diff_text } => Some(diff_text.as_str()),
            MapUnitKind::MetadataOnly { .. } => None,
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn review_unit(file: &str, diff: &str) -> MapUnit {
        MapUnit {
            file: file.to_string(),
            status: "modified".to_string(),
            kind: MapUnitKind::Review {
                diff_text: diff.to_string(),
            },
            diff_char_count: diff.len(),
            chunk_index: 0,
            chunk_total: 1,
            hunk_oversized: false,
        }
    }

    fn meta_unit(file: &str, note: &str) -> MapUnit {
        MapUnit {
            file: file.to_string(),
            status: "removed".to_string(),
            kind: MapUnitKind::MetadataOnly {
                note: note.to_string(),
            },
            diff_char_count: 0,
            chunk_index: 0,
            chunk_total: 1,
            hunk_oversized: false,
        }
    }

    #[test]
    fn map_unit_is_metadata_only() {
        let r = review_unit("src/lib.rs", "+fn foo() {}");
        assert!(
            !r.is_metadata_only(),
            "review unit must not be metadata-only"
        );
        let m = meta_unit("src/old.rs", "deleted file");
        assert!(m.is_metadata_only(), "metadata unit must be metadata-only");
    }

    #[test]
    fn map_unit_char_count() {
        let diff = "+fn bar() { 42 }";
        let u = review_unit("src/bar.rs", diff);
        assert_eq!(
            u.diff_char_count,
            diff.len(),
            "diff_char_count must match diff length"
        );
    }

    #[test]
    fn map_unit_diff_text_accessor() {
        let diff = "+fn baz() {}";
        let r = review_unit("src/baz.rs", diff);
        assert_eq!(r.diff_text(), Some(diff));

        let m = meta_unit("src/gone.rs", "binary file");
        assert_eq!(m.diff_text(), None);
    }

    #[test]
    fn map_unit_chunk_defaults() {
        let u = review_unit("src/f.rs", "+x");
        assert_eq!(u.chunk_index, 0);
        assert_eq!(u.chunk_total, 1);
        assert!(!u.hunk_oversized);
    }
}
