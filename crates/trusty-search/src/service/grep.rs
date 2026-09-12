//! `/grep` — grep-parity regex search over the files in an index.
//!
//! Why: the hybrid `/search` lane (BM25 + vector + KG) is excellent for
//! conceptual / semantic recall but it deliberately *fuzzes* matches. Some
//! workflows (an LLM agent verifying a refactor, a human chasing every literal
//! call site) need exact, deterministic, line-accurate matching with the same
//! ergonomics they get from `grep`/`ripgrep`: regex patterns, `-i`
//! case-insensitivity, `-A`/`-B`/`-C` context windows, `--include` glob
//! filtering, and dot-matches-newline multiline mode. Crucially this must work
//! *without re-embedding* — it greps the raw file bytes of whatever files the
//! index already knows about, so it is instant and never touches the ONNX
//! model.
//! What: a pure, I/O-free core ([`CompiledGrep`] + [`grep_file_content`]) that
//! the HTTP handler drives by (1) collecting the distinct set of file paths the
//! index has chunked, (2) reading each file fresh from disk under the index
//! `root_path`, and (3) running the matcher. Keeping the matcher pure makes the
//! line/column/context logic trivially unit-testable with in-memory strings.
//! Glob normalization, matching and the zero-match diagnostic live one module
//! over in [`crate::service::grep_glob`] (#7674).
//! Test: the `tests` module covers literal + regex matching, case folding,
//! `before`/`after`/`combined` context windows, multiline (dot-matches-newline)
//! matching, glob filtering, the `max_results` truncation flag, and invalid
//! regex/glob rejection. The HTTP wiring is exercised by the server-level
//! integration tests (`grep_*`), and the glob shapes of #7674 by
//! `crate::service::server::tests_grep_glob_7674`.

use serde::{Deserialize, Serialize};

// #7674: glob normalization, matching and the zero-match diagnostic live in
// their own module so this one stays the matcher.
pub use crate::service::grep_glob::{GlobFilter, GrepMeta, GrepScanCounts};

/// Request body for `POST /grep` and `POST /indexes/:id/grep`.
///
/// Why: a single struct mirrors the common `grep`/`ripgrep` flag surface so
/// callers can translate a CLI invocation to JSON field-by-field without
/// guessing. Every option has a sensible serde default, so the minimal request
/// is just `{ "pattern": "..." }`.
/// What: `serde`-deserialized; `pattern` is the only required field. The
/// `index_id` is optional at the type level so the same struct serves both the
/// global (`POST /grep`) and per-index (`POST /indexes/:id/grep`, where the id
/// comes from the path) endpoints.
/// Test: `request_defaults_are_grep_like` asserts the zero-value request is a
/// case-sensitive, no-context, all-files, single-line search.
#[derive(Debug, Clone, Deserialize)]
pub struct GrepRequest {
    /// The regex pattern to match (PCRE-ish, via the `regex` crate).
    pub pattern: String,

    /// Optional index id. When omitted on the global endpoint, every
    /// registered index is searched. Ignored by the per-index endpoint (the
    /// id is taken from the URL path there).
    #[serde(default)]
    pub index_id: Option<String>,

    /// `-i` parity: ASCII + Unicode case-insensitive matching.
    #[serde(default)]
    pub case_insensitive: bool,

    /// `-B` parity: number of lines of context to include before each match.
    #[serde(default)]
    pub context_before: usize,

    /// `-A` parity: number of lines of context to include after each match.
    #[serde(default)]
    pub context_after: usize,

    /// `-C` parity: when set, overrides both `context_before` and
    /// `context_after` (matches `grep -C`'s precedence over `-A`/`-B`).
    #[serde(default)]
    pub context: Option<usize>,

    /// `--include=<glob>` parity: only files whose path matches this glob are
    /// searched. The glob is matched against the index-relative file path
    /// (e.g. `crates/foo/src/bar.rs`). `None` = no filter.
    ///
    /// See #7674 for the normalization rules
    /// ([`crate::service::grep_glob::normalize_glob`]): a glob with no `/`
    /// matches by basename at any depth, and an absolute glob is additionally
    /// compared against the file's absolute path so a `file` value copied out
    /// of a `search` result works verbatim. When a glob is supplied the
    /// response carries a [`GrepMeta`] saying how many files it selected.
    #[serde(default)]
    pub glob: Option<String>,

    /// When true, `.` in the pattern matches newlines too (`(?s)` mode) so a
    /// single pattern can span multiple lines within a file.
    #[serde(default)]
    pub multiline: bool,

    /// `-F` / `--fixed-strings` parity: when true, `pattern` is treated as a
    /// literal string with no regex metacharacters. The literal is escaped via
    /// `regex::escape` before being handed to the regex engine so dots,
    /// brackets, etc. lose their special meaning.
    #[serde(default)]
    pub fixed_strings: bool,

    /// `-l` / `--files-with-matches` parity: when true, return at most one
    /// `GrepMatch` per file (the path of the first match) and short-circuit
    /// further scans of that file. The emitted match carries `line: 0`,
    /// `column: 0`, empty `text`, and empty context windows.
    #[serde(default)]
    pub files_with_matches: bool,

    /// `-v` / `--invert-match` parity: when true, return lines that do NOT
    /// match the pattern. Composes with `case_insensitive` and `word_regexp`.
    /// Not honoured in `multiline` mode (line-orientation is intrinsic to
    /// inversion); requesting both leaves `multiline` semantics in place.
    #[serde(default)]
    pub invert_match: bool,

    /// `-w` / `--word-regexp` parity: when true, the pattern only matches at
    /// word boundaries (`\b<pattern>\b`). When combined with `fixed_strings`
    /// the literal is escaped first, then wrapped in boundaries.
    #[serde(default)]
    pub word_regexp: bool,

    /// Hard cap on the number of matches returned across all files. Defaults
    /// to [`DEFAULT_MAX_RESULTS`]. The response `truncated` flag is set when
    /// the cap is hit.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

/// Default `max_results` when the caller omits it.
///
/// Why: an unbounded grep over a large repo could return tens of thousands of
/// lines and balloon the response. 100 mirrors the suggested API default and
/// keeps payloads small; callers wanting more raise it explicitly.
/// What: returns `100`.
/// Test: `request_defaults_are_grep_like`.
fn default_max_results() -> usize {
    DEFAULT_MAX_RESULTS
}

/// Default cap on returned matches.
pub const DEFAULT_MAX_RESULTS: usize = 100;

/// A single grep hit: one matching line plus its surrounding context.
///
/// Why: gives callers the same information `grep -n` would (file, 1-based line,
/// the line text) plus the column of the match start and optional context
/// windows so an LLM can reason about the surrounding code without a follow-up
/// fetch.
/// What: `serde`-serialized into the response `matches` array. `line` and
/// `column` are 1-based to match `grep`/editor conventions. For multiline
/// matches, `line`/`column` point at the first line/column of the match and
/// `text` is the first physical line of the match.
/// Test: `single_literal_match_reports_line_and_column`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrepMatch {
    /// Index-relative path of the file the match was found in.
    pub file: String,
    /// 1-based line number of the match's first line.
    pub line: usize,
    /// 1-based column (char offset within the line) of the match start.
    pub column: usize,
    /// The full text of the matching line.
    pub text: String,
    /// Up to `context_before` lines immediately preceding `line`.
    pub context_before: Vec<String>,
    /// Up to `context_after` lines immediately following `line`.
    pub context_after: Vec<String>,
}

/// Response body for the grep endpoints.
///
/// Why: callers need the matches plus enough metadata to know whether the
/// result set was clipped (`truncated`) and how many hits there were
/// (`total`).
/// What: `serde`-serialized. `total` equals `matches.len()` today but is kept
/// distinct so a future "count only" mode can report a total larger than the
/// returned slice. `meta` is present only when the request carried a `glob`,
/// and is what lets a caller tell "the pattern is absent" from "the glob
/// excluded every file" (#7674) — without it both render as `total: 0`.
/// Test: `truncates_at_max_results` asserts `truncated` flips and the slice is
/// clamped; `grep_reports_a_glob_that_selected_no_files` covers `meta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrepResponse {
    pub matches: Vec<GrepMatch>,
    pub total: usize,
    pub truncated: bool,
    /// Glob diagnostic; `None` when the request supplied no `glob` (#7674).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<GrepMeta>,
}

/// Errors that can occur while preparing a grep query.
///
/// Why: invalid user input (a malformed regex or glob) must map to a `400 Bad
/// Request`, not a `500`. A typed error lets the handler translate cleanly
/// without string-sniffing.
/// What: `thiserror`-derived; carries the underlying crate's message so the
/// caller can see *why* their pattern was rejected.
/// Test: `invalid_regex_is_rejected` and `invalid_glob_is_rejected`.
#[derive(Debug, thiserror::Error)]
pub enum GrepError {
    /// The supplied `pattern` did not compile as a regex.
    #[error("invalid regex pattern: {0}")]
    InvalidRegex(String),
    /// The supplied `glob` was not a valid glob pattern.
    #[error("invalid glob pattern: {0}")]
    InvalidGlob(String),
}

/// A compiled, ready-to-run grep query.
///
/// Why: compiling the regex and glob once (per request) and reusing them across
/// every file in the index avoids re-parsing the pattern thousands of times.
/// Holding the resolved context-window sizes here keeps [`grep_file_content`]'s
/// signature small.
/// What: built via [`CompiledGrep::compile`] from a [`GrepRequest`]. Owns the
/// compiled `regex::Regex`, an optional compiled `glob::Pattern`, and the
/// resolved before/after context line counts (`-C` already folded in).
/// Test: `compile_folds_context_C_over_A_B` checks the `-C` precedence; the
/// matching tests build a `CompiledGrep` directly.
#[derive(Debug)]
pub struct CompiledGrep {
    regex: regex::Regex,
    // #7674: the filter remembers the raw and normalized spellings so the
    // response `meta` can report what was actually compiled.
    glob: Option<GlobFilter>,
    context_before: usize,
    context_after: usize,
    multiline: bool,
    files_with_matches: bool,
    invert_match: bool,
}

impl CompiledGrep {
    /// Compile a request into an executable matcher.
    ///
    /// Why: this is the single place user-supplied pattern/glob strings are
    /// validated, so the handler can return `400` on the `Err` arm and never
    /// has to touch `regex`/`glob` directly.
    /// What: builds a `regex::RegexBuilder` with `case_insensitive` and
    /// `dot_matches_new_line` (multiline) flags applied, compiles the optional
    /// glob, and folds `-C` over `-A`/`-B` (a present `context` overrides both).
    /// Test: `compile_folds_context_C_over_A_B`, `invalid_regex_is_rejected`,
    /// `invalid_glob_is_rejected`.
    pub fn compile(req: &GrepRequest) -> Result<Self, GrepError> {
        // Step 1: optionally escape the pattern so regex metacharacters lose
        // their meaning (`-F` parity). When `fixed_strings` is false we feed
        // the user's pattern through verbatim, matching the default regex
        // behaviour callers expect.
        let escaped = if req.fixed_strings {
            regex::escape(&req.pattern)
        } else {
            req.pattern.clone()
        };
        // Step 2: optionally wrap in word boundaries (`-w` parity). Applied
        // *after* fixed-string escaping so `\b<literal>\b` works on patterns
        // that would otherwise contain regex metacharacters. We use `(?:...)`
        // to avoid mutating capture-group numbering callers may rely on.
        let final_pattern = if req.word_regexp {
            format!(r"\b(?:{escaped})\b")
        } else {
            escaped
        };

        let regex = regex::RegexBuilder::new(&final_pattern)
            .case_insensitive(req.case_insensitive)
            // `dot_matches_new_line` makes `.` span newlines so a single
            // pattern can match across lines — the multiline parity we want.
            .dot_matches_new_line(req.multiline)
            .build()
            .map_err(|e| GrepError::InvalidRegex(e.to_string()))?;

        // #7674: normalize before compiling (a separator-free glob matches by
        // basename, `rg -g` parity). An unparseable glob is still an error —
        // never a silently empty result set.
        let glob = match req.glob.as_deref() {
            Some(pat) => Some(GlobFilter::compile(pat).map_err(GrepError::InvalidGlob)?),
            None => None,
        };

        // `-C` (context) overrides both `-A` and `-B`, matching grep's
        // precedence.
        let (context_before, context_after) = match req.context {
            Some(c) => (c, c),
            None => (req.context_before, req.context_after),
        };

        Ok(Self {
            regex,
            glob,
            context_before,
            context_after,
            multiline: req.multiline,
            files_with_matches: req.files_with_matches,
            invert_match: req.invert_match,
        })
    }

    /// Test whether an index-relative file path passes the glob filter.
    ///
    /// Why: lets the handler skip reading files that can't match before paying
    /// the I/O cost.
    /// What: returns `true` when no glob was supplied, otherwise delegates to
    /// [`GlobFilter::matches_rel`]. Prefer [`Self::path_matches_in_root`] on a
    /// path the index root is known for — it additionally honours an absolute
    /// glob (#7674).
    /// Test: `glob_filters_by_path`.
    pub fn path_matches(&self, rel_path: &str) -> bool {
        match &self.glob {
            None => true,
            Some(filter) => filter.matches_rel(rel_path),
        }
    }

    /// Test a file against the glob, resolving it under the index root first.
    ///
    /// Why: an absolute glob — the spelling `search`/`search_lexical` hand back
    /// in their own `file` field — can never match the index-relative path the
    /// corpus stores, so pasting one into `glob` returned zero matches with no
    /// explanation (#7674). The root is what turns one form into the other.
    /// What: delegates to [`GlobFilter::matches_in_root`], which tries the
    /// index-relative path first and only resolves against `root` for a glob
    /// the caller wrote absolutely.
    /// Test: `absolute_glob_matches_via_the_root` (in `grep_glob`);
    /// `grep_honours_an_absolute_glob_as_search_reports_it`.
    pub fn path_matches_in_root(&self, rel_path: &str, root: &std::path::Path) -> bool {
        match &self.glob {
            None => true,
            Some(filter) => filter.matches_in_root(rel_path, root),
        }
    }

    /// The compiled glob filter, when the request supplied one (#7674).
    pub fn glob_filter(&self) -> Option<&GlobFilter> {
        self.glob.as_ref()
    }

    /// Build the response `meta` for a completed scan, if a glob was in play.
    ///
    /// Why: the handlers must not each decide when a diagnostic is owed —
    /// "a glob was supplied" is the single rule, and it lives here.
    /// What: returns `None` when no glob was supplied, otherwise
    /// [`GrepMeta::new`] over the accumulated scan counters.
    /// Test: `grep_reports_a_glob_that_selected_no_files`.
    pub fn glob_meta(&self, counts: GrepScanCounts) -> Option<GrepMeta> {
        self.glob.as_ref().map(|f| GrepMeta::new(f, counts))
    }
}

/// Run the compiled grep over one file's text, appending hits to `out`.
///
/// Why: this is the pure heart of the feature — no filesystem, no locks, just
/// `&str` in, `Vec<GrepMatch>` out — so the line/column/context arithmetic can
/// be unit-tested exhaustively. The handler calls it once per file.
/// What: in single-line mode it scans line-by-line and records the first match
/// column per line (multiple matches on one line still yield one `GrepMatch`,
/// matching `grep`'s default line-oriented output). In multiline mode it runs
/// the regex over the whole file, maps each match's byte offset back to a
/// 1-based `(line, column)`, and uses that line's text as `text`. Context
/// windows are clamped to file bounds. Stops early once `out.len()` reaches
/// `max_results` so a runaway pattern can't blow up memory. `file` is copied
/// into every emitted match verbatim.
/// Test: `single_literal_match_reports_line_and_column`,
/// `context_windows_are_clamped`, `multiline_match_spans_lines`,
/// `respects_remaining_budget`.
pub fn grep_file_content(
    file: &str,
    content: &str,
    compiled: &CompiledGrep,
    out: &mut Vec<GrepMatch>,
    max_results: usize,
) {
    if out.len() >= max_results {
        return;
    }

    // `-l` short-circuit: when the caller only wants the path of matching
    // files we use a fast probe (single regex find over the whole content)
    // and emit a synthetic `GrepMatch`. `invert_match` flips the predicate
    // (emit when *no* line matches), matching `rg -lv` / `grep -lv`.
    if compiled.files_with_matches {
        emit_files_with_matches(file, content, compiled, out);
        return;
    }

    // Pre-split into lines once; reused for both match text and context.
    let lines: Vec<&str> = content.lines().collect();

    if compiled.multiline {
        grep_multiline(file, content, &lines, compiled, out, max_results);
    } else {
        grep_line_by_line(file, &lines, compiled, out, max_results);
    }
}

/// `-l` / `--files-with-matches`: emit at most one synthetic match per file.
///
/// Why: callers that just want the list of matching paths (e.g. an LLM
/// driving a follow-up `read_file`) get O(1) wire size per file instead of
/// O(matches). Honours `invert_match` so `grep -Lv` semantics also work.
/// What: probes `content` for any match (single line in the non-multiline
/// case, whole-file in multiline mode), then emits a `GrepMatch` with
/// `line: 0, column: 0, text: ""` so downstream callers can detect the
/// "path only" shape unambiguously. Returns without emitting when the
/// predicate is unsatisfied.
/// Test: `files_with_matches_returns_path_once`,
/// `files_with_matches_honours_invert`.
fn emit_files_with_matches(
    file: &str,
    content: &str,
    compiled: &CompiledGrep,
    out: &mut Vec<GrepMatch>,
) {
    // The predicate "file has any matching line" reduces to a single
    // regex find over the full content — `\n` is not in the default
    // dot-character class, so a line-oriented pattern still matches.
    let any_match = compiled.regex.is_match(content);
    let should_emit = if compiled.invert_match {
        // `grep -Lv`: emit when at least one line does NOT match. We have
        // to scan line-by-line because "any non-matching line" cannot be
        // derived from a single whole-content probe.
        content
            .lines()
            .any(|line| !compiled.regex.is_match(line))
            // An empty file has zero lines — match grep's behaviour where
            // `-Lv` on an empty file emits nothing.
            && content.lines().next().is_some()
    } else {
        any_match
    };
    if should_emit {
        out.push(GrepMatch {
            file: file.to_string(),
            line: 0,
            column: 0,
            text: String::new(),
            context_before: Vec::new(),
            context_after: Vec::new(),
        });
    }
}

/// Line-oriented scan: one `GrepMatch` per matching line (grep default).
///
/// When `invert_match` is set, emit one `GrepMatch` per line that does NOT
/// match. The reported `column` for inverted matches is always `1` (there
/// is no match offset to point at) and the line text is the full line.
fn grep_line_by_line(
    file: &str,
    lines: &[&str],
    compiled: &CompiledGrep,
    out: &mut Vec<GrepMatch>,
    max_results: usize,
) {
    for (idx, line) in lines.iter().enumerate() {
        if out.len() >= max_results {
            return;
        }
        if compiled.invert_match {
            if !compiled.regex.is_match(line) {
                out.push(build_match(file, lines, idx, 1, compiled));
            }
        } else if let Some(m) = compiled.regex.find(line) {
            out.push(build_match(
                file,
                lines,
                idx,
                byte_to_col(line, m.start()),
                compiled,
            ));
        }
    }
}

/// Whole-file scan for `multiline` mode: map each match offset back to a line.
fn grep_multiline(
    file: &str,
    content: &str,
    lines: &[&str],
    compiled: &CompiledGrep,
    out: &mut Vec<GrepMatch>,
    max_results: usize,
) {
    // Precompute the starting byte offset of each line so we can translate a
    // whole-file match offset into a 1-based (line, column) in O(log n).
    let mut line_starts = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for line in lines {
        line_starts.push(offset);
        // +1 for the '\n' stripped by `.lines()`. Files without a trailing
        // newline still work because we never index past the last match.
        offset += line.len() + 1;
    }

    for m in compiled.regex.find_iter(content) {
        if out.len() >= max_results {
            return;
        }
        let start = m.start();
        // Find the line whose start is the greatest <= match start.
        let line_idx = match line_starts.binary_search(&start) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_text = lines.get(line_idx).copied().unwrap_or("");
        let col = byte_to_col(line_text, start - line_starts[line_idx]);
        out.push(build_match(file, lines, line_idx, col, compiled));
    }
}

/// Assemble a [`GrepMatch`] for the line at `idx` with clamped context windows.
fn build_match(
    file: &str,
    lines: &[&str],
    idx: usize,
    column: usize,
    compiled: &CompiledGrep,
) -> GrepMatch {
    let before_start = idx.saturating_sub(compiled.context_before);
    let context_before: Vec<String> = lines[before_start..idx]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let after_end = (idx + 1 + compiled.context_after).min(lines.len());
    let context_after: Vec<String> = lines[idx + 1..after_end]
        .iter()
        .map(|s| s.to_string())
        .collect();

    GrepMatch {
        file: file.to_string(),
        line: idx + 1,
        column,
        text: lines.get(idx).copied().unwrap_or("").to_string(),
        context_before,
        context_after,
    }
}

/// Translate a byte offset within a line into a 1-based char column.
///
/// Why: regex match offsets are byte offsets, but editors/grep count columns by
/// character. Multi-byte UTF-8 (e.g. an emoji or accented char) before the
/// match would otherwise inflate the reported column.
/// What: counts `char` boundaries in `line[..byte]` and returns count + 1.
/// Test: `byte_to_col_handles_multibyte`.
fn byte_to_col(line: &str, byte: usize) -> usize {
    let clamped = byte.min(line.len());
    line[..clamped].chars().count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `GrepRequest` with everything defaulted but `pattern`.
    fn req(pattern: &str) -> GrepRequest {
        GrepRequest {
            pattern: pattern.to_string(),
            index_id: None,
            case_insensitive: false,
            context_before: 0,
            context_after: 0,
            context: None,
            glob: None,
            multiline: false,
            fixed_strings: false,
            files_with_matches: false,
            invert_match: false,
            word_regexp: false,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }

    fn run(file: &str, content: &str, r: &GrepRequest) -> Vec<GrepMatch> {
        let compiled = CompiledGrep::compile(r).expect("compile");
        let mut out = Vec::new();
        grep_file_content(file, content, &compiled, &mut out, r.max_results);
        out
    }

    /// A request with only `pattern` set is a case-sensitive, no-context,
    /// all-files, single-line search with the default cap.
    #[test]
    fn request_defaults_are_grep_like() {
        let r = req("x");
        assert!(!r.case_insensitive);
        assert_eq!(r.context_before, 0);
        assert_eq!(r.context_after, 0);
        assert!(r.context.is_none());
        assert!(r.glob.is_none());
        assert!(!r.multiline);
        assert!(!r.fixed_strings);
        assert!(!r.files_with_matches);
        assert!(!r.invert_match);
        assert!(!r.word_regexp);
        assert_eq!(r.max_results, DEFAULT_MAX_RESULTS);
        // Round-trip the documented default through serde.
        let parsed: GrepRequest = serde_json::from_str(r#"{"pattern":"x"}"#).unwrap();
        assert_eq!(parsed.max_results, DEFAULT_MAX_RESULTS);
        assert!(!parsed.case_insensitive);
        assert!(!parsed.fixed_strings);
        assert!(!parsed.files_with_matches);
        assert!(!parsed.invert_match);
        assert!(!parsed.word_regexp);
    }

    /// A literal match reports the 1-based line and column.
    #[test]
    fn single_literal_match_reports_line_and_column() {
        let content = "fn a() {}\n    fn authenticate() {}\nfn b() {}\n";
        let matches = run("src/auth.rs", content, &req("authenticate"));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file, "src/auth.rs");
        assert_eq!(matches[0].line, 2);
        // "    fn " = 4 spaces + "fn " = 7 chars before "authenticate" ⇒ col 8.
        assert_eq!(matches[0].column, 8);
        assert_eq!(matches[0].text, "    fn authenticate() {}");
    }

    /// Regex (not just literal) patterns work.
    #[test]
    fn regex_pattern_matches() {
        let content = "let x = 1;\nlet y = 22;\nlet z = 333;\n";
        let matches = run("a.rs", content, &req(r"=\s*\d{2,};"));
        assert_eq!(matches.len(), 2); // 22 and 333
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[1].line, 3);
    }

    /// `case_insensitive` folds case.
    #[test]
    fn case_insensitive_matches() {
        let content = "ERROR here\nno match\nerror there\n";
        let mut r = req("error");
        assert_eq!(run("a.rs", content, &r).len(), 1); // only "error there"
        r.case_insensitive = true;
        assert_eq!(run("a.rs", content, &r).len(), 2);
    }

    /// Context windows are clamped to the file bounds.
    #[test]
    fn context_windows_are_clamped() {
        let content = "l1\nl2\nMATCH\nl4\nl5\n";
        let mut r = req("MATCH");
        r.context_before = 5; // more than available ⇒ clamp to 2
        r.context_after = 5; // clamp to 2
        let matches = run("a.rs", content, &r);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].context_before, vec!["l1", "l2"]);
        assert_eq!(matches[0].context_after, vec!["l4", "l5"]);
    }

    /// `-C` overrides both `-A` and `-B`.
    #[test]
    fn compile_folds_context_c_over_a_b() {
        let mut r = req("MATCH");
        r.context_before = 1;
        r.context_after = 1;
        r.context = Some(3);
        let content = "a\nb\nc\nd\nMATCH\ne\nf\ng\nh\n";
        let matches = run("a.rs", content, &r);
        assert_eq!(matches[0].context_before, vec!["b", "c", "d"]);
        assert_eq!(matches[0].context_after, vec!["e", "f", "g"]);
    }

    /// Multiline mode lets `.` span newlines so one pattern matches across
    /// lines, and the match is attributed to its first line.
    #[test]
    fn multiline_match_spans_lines() {
        let content = "struct S {\n    field: i32,\n}\n";
        let mut r = req(r"struct S \{.*field");
        // Single-line: no match (the `.` can't cross the newline).
        assert_eq!(run("a.rs", content, &r).len(), 0);
        // Multiline: matches, attributed to line 1.
        r.multiline = true;
        let matches = run("a.rs", content, &r);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].column, 1);
    }

    /// The glob filter accepts/rejects index-relative paths.
    #[test]
    fn glob_filters_by_path() {
        let mut r = req("x");
        r.glob = Some("**/*.rs".to_string());
        let compiled = CompiledGrep::compile(&r).unwrap();
        assert!(compiled.path_matches("crates/foo/src/bar.rs"));
        assert!(compiled.path_matches("top.rs"));
        assert!(!compiled.path_matches("crates/foo/README.md"));

        // Bare `*.rs` still matches nested paths (ripgrep --include parity).
        r.glob = Some("*.rs".to_string());
        let compiled = CompiledGrep::compile(&r).unwrap();
        assert!(compiled.path_matches("a/b/c.rs"));
        assert!(!compiled.path_matches("a/b/c.py"));
    }

    /// `max_results` truncates the per-file emission and never exceeds the cap.
    #[test]
    fn respects_remaining_budget() {
        let content = "x\nx\nx\nx\nx\n";
        let mut r = req("x");
        r.max_results = 3;
        let matches = run("a.rs", content, &r);
        assert_eq!(matches.len(), 3);
    }

    /// `grep_file_content` is a no-op once the budget is already exhausted.
    #[test]
    fn no_op_when_budget_exhausted() {
        let r = req("x");
        let compiled = CompiledGrep::compile(&r).unwrap();
        let mut out = vec![GrepMatch {
            file: "pre.rs".into(),
            line: 1,
            column: 1,
            text: "x".into(),
            context_before: vec![],
            context_after: vec![],
        }];
        grep_file_content("a.rs", "x\nx\n", &compiled, &mut out, 1);
        assert_eq!(out.len(), 1); // unchanged
    }

    /// Invalid regex is rejected with a typed error (→ 400 at the handler).
    #[test]
    fn invalid_regex_is_rejected() {
        let r = req("(unclosed");
        let err = CompiledGrep::compile(&r).unwrap_err();
        assert!(matches!(err, GrepError::InvalidRegex(_)));
    }

    /// Invalid glob is rejected with a typed error (→ 400 at the handler).
    #[test]
    fn invalid_glob_is_rejected() {
        let mut r = req("x");
        r.glob = Some("[unclosed".to_string());
        let err = CompiledGrep::compile(&r).unwrap_err();
        assert!(matches!(err, GrepError::InvalidGlob(_)));
    }

    /// Multi-byte chars before the match don't inflate the reported column.
    #[test]
    fn byte_to_col_handles_multibyte() {
        // "café_" has a 2-byte 'é'; the match on "X" should report char col 6.
        let content = "café_X\n";
        let matches = run("a.rs", content, &req("X"));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].column, 6);
    }

    /// `-F` / `fixed_strings`: regex metacharacters are treated literally.
    #[test]
    fn fixed_strings_treats_pattern_as_literal() {
        let content = "a.b\nacb\nax\n";
        // As a regex, "a.b" matches both "a.b" and "acb"; as a literal, only "a.b".
        let mut r = req("a.b");
        let regex_hits = run("a.rs", content, &r);
        assert_eq!(regex_hits.len(), 2);

        r.fixed_strings = true;
        let literal_hits = run("a.rs", content, &r);
        assert_eq!(literal_hits.len(), 1);
        assert_eq!(literal_hits[0].text, "a.b");
    }

    /// `-F` rejects no input — a pattern that would be an invalid regex
    /// compiles cleanly when `fixed_strings` is set.
    #[test]
    fn fixed_strings_accepts_invalid_regex_chars() {
        // `vec[` is an unterminated character class as a regex but a perfectly
        // legal literal byte sequence; fixed_strings should still compile and
        // match it.
        let content = "vec[0] = 1;\nvec.get(0);\n";
        let mut r = req("vec[");
        assert!(CompiledGrep::compile(&r).is_err());
        r.fixed_strings = true;
        let matches = run("a.rs", content, &r);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
    }

    /// `-l` / `files_with_matches`: emit one synthetic match per matching
    /// file, with line/column zero and empty context.
    #[test]
    fn files_with_matches_returns_path_once() {
        let content = "fn a() {}\n// TODO refactor\nfn b() {}\n// TODO inline\n";
        let mut r = req("TODO");
        // Without -l: two matches.
        assert_eq!(run("a.rs", content, &r).len(), 2);

        r.files_with_matches = true;
        let matches = run("src/a.rs", content, &r);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file, "src/a.rs");
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[0].column, 0);
        assert!(matches[0].text.is_empty());
        assert!(matches[0].context_before.is_empty());
        assert!(matches[0].context_after.is_empty());
    }

    /// `-l` emits nothing when the file has no matching line.
    #[test]
    fn files_with_matches_skips_non_matching_files() {
        let mut r = req("ZZZ");
        r.files_with_matches = true;
        let matches = run("a.rs", "fn a() {}\nfn b() {}\n", &r);
        assert!(matches.is_empty());
    }

    /// `-Lv`: emit only files that have at least one non-matching line and
    /// are not empty.
    #[test]
    fn files_with_matches_honours_invert() {
        let mut r = req("fn");
        r.files_with_matches = true;
        r.invert_match = true;
        // Every line matches → emit nothing.
        let all_match = "fn a() {}\nfn b() {}\n";
        assert!(run("a.rs", all_match, &r).is_empty());
        // At least one line doesn't match → emit once.
        let mixed = "fn a() {}\nstruct S;\n";
        let hits = run("a.rs", mixed, &r);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "a.rs");
    }

    /// `-v` / `invert_match`: return lines that do NOT match the pattern.
    #[test]
    fn invert_match_returns_non_matching_lines() {
        let content = "fn a() {}\nstruct S;\nfn b() {}\n";
        let mut r = req("^fn");
        // Default: two matching lines.
        assert_eq!(run("a.rs", content, &r).len(), 2);
        // Inverted: one non-matching line (`struct S;`).
        r.invert_match = true;
        let matches = run("a.rs", content, &r);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text, "struct S;");
        assert_eq!(matches[0].line, 2);
        // Column is 1 for inverted matches (no offset to point at).
        assert_eq!(matches[0].column, 1);
    }

    /// `-w` / `word_regexp`: pattern only matches at word boundaries.
    #[test]
    fn word_regexp_requires_boundaries() {
        let content = "let log = 1;\nlet catalog = 2;\nlet log_level = 3;\n";
        let mut r = req("log");
        // Without -w: every line matches (substring hits).
        assert_eq!(run("a.rs", content, &r).len(), 3);

        r.word_regexp = true;
        let matches = run("a.rs", content, &r);
        // Only the bare `log` token survives; `catalog` and `log_level` are
        // both word-character continuations on at least one boundary.
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
    }

    /// `-Fw`: word boundaries wrap the escaped literal so regex
    /// metacharacters in `pattern` don't break boundary semantics.
    #[test]
    fn word_regexp_composes_with_fixed_strings() {
        let content = "use a.b;\nuse a.bc;\n";
        let mut r = req("a.b");
        r.fixed_strings = true;
        r.word_regexp = true;
        let matches = run("a.rs", content, &r);
        // Boundary after `b` excludes `a.bc`.
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
    }
}
