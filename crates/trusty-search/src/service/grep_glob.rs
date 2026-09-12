//! Glob filtering for `/grep` — normalization, matching, and the zero-match
//! diagnostic (#7674).
//!
//! Why: `grep`'s `glob` parameter is matched against the *index-relative* path
//! of every file the corpus knows about, and `glob::Pattern` is **fully
//! anchored** — it matches the whole string or nothing. That anchoring is
//! invisible in the common `*.rs` case, because `require_literal_separator =
//! false` lets `*` swallow `/`, so `*.rs` appears to match at any depth. Two
//! real caller spellings then fail silently: a bare basename (`grep.rs`), which
//! `rg -g` matches at any depth but which anchors to the repo root here; and an
//! absolute path, which is exactly the spelling `search`/`search_lexical` hand
//! back in their own `file` field, so copying a path out of one tool and into
//! this one's `glob` returns nothing. Both render as `{matches: [], total: 0}`,
//! indistinguishable from "the pattern is nowhere in your code" — which is how
//! #7674 was reported.
//! What: [`normalize_glob`] applies ripgrep's separator rule (a glob with no
//! `/` matches by basename, i.e. gains a `**/` prefix); [`GlobFilter`] compiles
//! the normalized form once and tests it against the index-relative path, plus
//! the file's absolute path when the caller supplied an absolute glob; and
//! [`GrepMeta`] carries the normalized form and the matched-file count back to
//! the caller so an empty result set is never ambiguous.
//! Test: the `tests` module below covers normalization, every glob shape in
//! #7674, and the meta note; the end-to-end shapes are pinned by
//! `crate::service::server::tests_grep_glob_7674`.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Match options shared by every glob comparison.
///
/// Why: `require_literal_separator = false` is the long-standing choice that
/// makes `*.rs` match `a/b/c.rs` (ripgrep `--include` ergonomics). Holding it
/// in one place stops the relative and absolute comparisons drifting apart.
/// What: case-sensitive, separators crossable by `*`, leading dots matchable.
/// Test: `star_crosses_a_separator`.
const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

/// Rewrite a caller's glob into the form actually compiled.
///
/// Why: ripgrep's rule is that a glob containing a `/` is anchored to the search
/// root while a glob without one matches the *basename* at any depth. Only the
/// first half held here, so `grep.rs` — which `rg -g grep.rs` matches — silently
/// matched nothing (#7674).
/// What: trims the glob, and prefixes `**/` when it contains no `/` at all.
/// Every other spelling is returned unchanged, so an anchored glob such as
/// `src/**/*.rs` keeps its ripgrep meaning (anchored at the index root, and so
/// legitimately empty against a workspace whose sources live under `crates/`).
/// Test: `basename_glob_gains_a_recursive_prefix`, `anchored_glob_is_untouched`.
pub fn normalize_glob(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains('/') {
        trimmed.to_string()
    } else {
        format!("**/{trimmed}")
    }
}

/// A compiled `--include` filter over index-relative file paths.
///
/// Why: the filter has to remember three things the raw `glob::Pattern` cannot —
/// what the caller typed, what was actually compiled, and whether the caller's
/// spelling was absolute — because all three appear in the [`GrepMeta`] the
/// caller needs to tell "no hits" from "the glob excluded everything" (#7674).
/// What: holds the raw and normalized spellings beside the compiled pattern.
/// [`Self::matches_rel`] is the cheap index-relative test the file walk uses;
/// [`Self::matches_in_root`] adds the absolute-path comparison, and only for an
/// absolute glob, so a relative glob pays nothing for it.
/// Test: `absolute_glob_matches_via_the_root`, `relative_glob_ignores_the_root`.
#[derive(Debug, Clone)]
pub struct GlobFilter {
    raw: String,
    normalized: String,
    pattern: glob::Pattern,
    absolute: bool,
}

impl GlobFilter {
    /// Compile a caller-supplied glob, or report why it is not a glob.
    ///
    /// Why: an unparseable glob must reach the caller as a `400`, never as an
    /// empty result set — a silent empty answer is the exact failure #7674 is
    /// about, so the fail-open arm does not exist here.
    /// What: normalizes via [`normalize_glob`], compiles the result, and returns
    /// the underlying crate's message on the `Err` arm.
    /// Test: `an_unparseable_glob_is_an_error`.
    pub fn compile(raw: &str) -> Result<Self, String> {
        let normalized = normalize_glob(raw);
        let pattern = glob::Pattern::new(&normalized).map_err(|e| e.to_string())?;
        Ok(Self {
            raw: raw.trim().to_string(),
            absolute: normalized.starts_with('/'),
            normalized,
            pattern,
        })
    }

    /// Test the glob against an index-relative path (`crates/foo/src/bar.rs`).
    pub fn matches_rel(&self, rel_path: &str) -> bool {
        self.pattern.matches_with(rel_path, MATCH_OPTIONS)
    }

    /// Test the glob against a file, trying its absolute path for an absolute glob.
    ///
    /// Why: `search`/`search_lexical` report `file` as an absolute path, so the
    /// spelling a caller most naturally pastes into `grep`'s `glob` is absolute
    /// — and an absolute glob can never match the index-relative path this
    /// filter is otherwise applied to (#7674). Accepting it is a deliberate
    /// superset of `rg -g`, which has no notion of an absolute include.
    /// What: tries the index-relative path first (the overwhelmingly common
    /// case, and the only one a relative glob can satisfy); for an absolute glob
    /// it then resolves the file under `root` and retries.
    /// Test: `absolute_glob_matches_via_the_root`, `relative_glob_ignores_the_root`.
    pub fn matches_in_root(&self, rel_path: &str, root: &Path) -> bool {
        if self.matches_rel(rel_path) {
            return true;
        }
        if !self.absolute {
            return false;
        }
        let abs = if Path::new(rel_path).is_absolute() {
            PathBuf::from(rel_path)
        } else {
            root.join(rel_path)
        };
        self.pattern
            .matches_with(&abs.to_string_lossy(), MATCH_OPTIONS)
    }

    /// The glob exactly as the caller supplied it (trimmed).
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The glob as actually compiled, after [`normalize_glob`].
    pub fn normalized(&self) -> &str {
        &self.normalized
    }
}

/// How many files a glob-filtered grep actually looked at.
///
/// Why: `{matches: [], total: 0}` cannot distinguish "the pattern is absent"
/// from "the glob excluded every file" from "the corpus holds nothing" — the
/// ambiguity #7674 reported. These two counters are what separate them.
/// What: `corpus_files` is the distinct file set the index knows about;
/// `glob_matched_files` is how many of those passed the filter. [`Self::add`]
/// accumulates across a global fan-out.
/// Test: `counts_accumulate_across_indexes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GrepScanCounts {
    /// Distinct files the index corpus knows about.
    pub corpus_files: usize,
    /// How many of those passed the glob filter.
    pub glob_matched_files: usize,
}

impl GrepScanCounts {
    /// Fold another index's counts into this one (global fan-out).
    pub fn add(&mut self, other: Self) {
        self.corpus_files += other.corpus_files;
        self.glob_matched_files += other.glob_matched_files;
    }
}

/// Diagnostic block attached to a grep response that used a glob (#7674).
///
/// Why: a caller cannot act on `total: 0` without knowing whether the glob
/// participated. Reporting the normalized glob alongside the matched-file count
/// makes the two failure modes — a glob that excluded everything, and a pattern
/// genuinely absent from the files it did select — distinguishable in one
/// round trip, and shows the caller what their glob was rewritten to.
/// What: serialized as the response's `meta` object, and only when the request
/// carried a `glob`. `note` is populated only when the glob selected no files,
/// so its presence is itself the signal.
/// Test: `meta_notes_a_glob_that_selected_nothing`,
/// `meta_has_no_note_when_the_glob_selected_files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrepMeta {
    /// The glob exactly as supplied by the caller.
    pub glob: String,
    /// The glob as compiled, after ripgrep-parity normalization.
    pub glob_normalized: String,
    /// How many indexed files passed the glob filter.
    pub glob_matched_files: usize,
    /// How many distinct files the index corpus holds.
    pub corpus_files: usize,
    /// Present only when `glob_matched_files == 0`; says so in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl GrepMeta {
    /// Build the diagnostic for a completed glob-filtered scan.
    ///
    /// Why: the note is the whole point — it is what stops an agent reading an
    /// empty `matches` array as proof that a literal is absent from the repo.
    /// What: copies the filter's raw and normalized spellings and the scan
    /// counters, and attaches a note when the filter selected no file at all.
    /// Test: `meta_notes_a_glob_that_selected_nothing`.
    pub fn new(filter: &GlobFilter, counts: GrepScanCounts) -> Self {
        let note = (counts.glob_matched_files == 0).then(|| {
            format!(
                "glob '{}' (compiled as '{}') selected 0 of {} indexed files, so no file was \
                 read and an empty `matches` array does NOT mean the pattern is absent from the \
                 corpus. Check the path is indexed, or widen the glob (a leading '**/' matches \
                 at any depth).",
                filter.raw(),
                filter.normalized(),
                counts.corpus_files
            )
        });
        Self {
            glob: filter.raw().to_string(),
            glob_normalized: filter.normalized().to_string(),
            glob_matched_files: counts.glob_matched_files,
            corpus_files: counts.corpus_files,
            note,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A glob with no separator matches by basename at any depth (`rg -g` rule).
    #[test]
    fn basename_glob_gains_a_recursive_prefix() {
        assert_eq!(normalize_glob("grep.rs"), "**/grep.rs");
        assert_eq!(normalize_glob("*.rs"), "**/*.rs");
        // Whitespace a caller pasted in is not part of the glob.
        assert_eq!(normalize_glob("  grep.rs  "), "**/grep.rs");
    }

    /// Any glob containing a separator keeps its anchored ripgrep meaning.
    #[test]
    fn anchored_glob_is_untouched() {
        for g in [
            "**/*.rs",
            "crates/trusty-search/**",
            "src/**/*.rs",
            "/abs/path/*.rs",
        ] {
            assert_eq!(normalize_glob(g), g, "glob {g} must not be rewritten");
        }
    }

    /// The historical `require_literal_separator = false` choice is preserved.
    #[test]
    fn star_crosses_a_separator() {
        let f = GlobFilter::compile("crates/trusty-search/*.rs").expect("compiles");
        assert!(f.matches_rel("crates/trusty-search/src/service/grep.rs"));
    }

    /// An absolute glob matches once the file is resolved under the index root.
    #[test]
    fn absolute_glob_matches_via_the_root() {
        let root = Path::new("/repo");
        let f = GlobFilter::compile("/repo/crates/a/src/lib.rs").expect("compiles");
        assert!(
            !f.matches_rel("crates/a/src/lib.rs"),
            "relative form cannot"
        );
        assert!(f.matches_in_root("crates/a/src/lib.rs", root));
        assert!(!f.matches_in_root("crates/b/src/lib.rs", root));
    }

    /// A relative glob never pays for (or is rescued by) the absolute retry.
    #[test]
    fn relative_glob_ignores_the_root() {
        let f = GlobFilter::compile("crates/a/**").expect("compiles");
        assert!(f.matches_in_root("crates/a/src/lib.rs", Path::new("/repo")));
        assert!(!f.matches_in_root("crates/b/src/lib.rs", Path::new("/repo")));
    }

    /// Fail-Open Check: a glob that will not parse is an error, never an
    /// empty result set.
    #[test]
    fn an_unparseable_glob_is_an_error() {
        let err = GlobFilter::compile("a[b").expect_err("unterminated class must be rejected");
        assert!(!err.is_empty(), "the error must carry a reason: {err}");
    }

    /// Global fan-out sums each index's scan counters.
    #[test]
    fn counts_accumulate_across_indexes() {
        let mut acc = GrepScanCounts::default();
        acc.add(GrepScanCounts {
            corpus_files: 10,
            glob_matched_files: 2,
        });
        acc.add(GrepScanCounts {
            corpus_files: 5,
            glob_matched_files: 0,
        });
        assert_eq!(
            acc,
            GrepScanCounts {
                corpus_files: 15,
                glob_matched_files: 2
            }
        );
    }

    /// A glob that selected nothing is reported in words, not just a zero.
    #[test]
    fn meta_notes_a_glob_that_selected_nothing() {
        let f = GlobFilter::compile("crates/nope/**").expect("compiles");
        let meta = GrepMeta::new(
            &f,
            GrepScanCounts {
                corpus_files: 4_096,
                glob_matched_files: 0,
            },
        );
        assert_eq!(meta.glob, "crates/nope/**");
        assert_eq!(meta.glob_normalized, "crates/nope/**");
        assert_eq!(meta.glob_matched_files, 0);
        let note = meta.note.expect("a zero-selection glob must carry a note");
        assert!(note.contains("selected 0 of 4096"), "note was: {note}");
    }

    /// A glob that did select files carries the counts but no note.
    #[test]
    fn meta_has_no_note_when_the_glob_selected_files() {
        let f = GlobFilter::compile("*.rs").expect("compiles");
        let meta = GrepMeta::new(
            &f,
            GrepScanCounts {
                corpus_files: 10,
                glob_matched_files: 3,
            },
        );
        assert_eq!(meta.glob_normalized, "**/*.rs");
        assert!(meta.note.is_none());
    }
}
