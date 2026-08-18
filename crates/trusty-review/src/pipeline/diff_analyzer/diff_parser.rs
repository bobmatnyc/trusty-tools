//! Unified-diff parser feeding Stage A (spec REV-200).
//!
//! Why: Stage A classifies files, so it needs one record per file in the diff.
//! The previous parser opened a record only on `diff --git ` and bound the path
//! on `+++ b/`, without flushing — so a diff carrying `---`/`+++` pairs but no
//! `diff --git ` markers rebound the open record's path on every file and
//! returned ONE record holding every file's hunks, named after the last file
//! (#4458). It also dropped any file whose record never saw a `+++ b/` line:
//! deletions, binary files, mode-only changes, and pure renames left no record
//! at all, and the caller saw a smaller list with no signal that anything was
//! lost.
//!
//! What: a line-oriented state machine over the diff. Records start on
//! `diff --git ` and on a `---`/`+++` header pair that arrives after the open
//! record already has a path or hunks. Hunk bodies are consumed against the
//! line budget declared by their `@@` header, so a diff-of-a-diff's body lines
//! never read as file headers. Every record resolves a path from the first
//! available of `+++ b/`, `rename to`, `--- a/`, and the `diff --git` header
//! itself; a record that resolves none is reported as an `UnparsedSection`
//! rather than discarded.
//!
//! Test: `diff_parser_tests.rs` — see `multi_file_diff_without_git_markers`.

// ─── Public result types ──────────────────────────────────────────────────────

/// A run of diff lines that carried content but no resolvable path.
///
/// Why: a parse failure that shrinks the file list silently is how #4458 stayed
/// invisible in production — callers saw `kept=1` and nothing else. Reporting
/// the section gives the caller an explicit count to log or reject on.
/// What: the section's first line (truncated) and how many lines it held.
/// Test: `headerless_hunks_are_reported_unparsed`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnparsedSection {
    /// First line of the section, truncated to `HEADER_SAMPLE_LIMIT` characters.
    pub header: String,
    /// Number of lines the section held.
    pub line_count: usize,
}

/// Parsed diff: one entry per file, plus anything that resolved to no file.
///
/// Why: `parse_diff_files` keeps its `Vec<(path, status, patch)>` shape for
/// existing callers; this type carries the loss signal alongside it.
/// What: `files` are `(path, status, patch)` triples in diff order; `unparsed`
/// holds sections that produced no file.
/// Test: `diff_parser_tests.rs`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ParsedDiff {
    /// One `(path, status, patch)` triple per file, in diff order.
    pub files: Vec<(String, String, String)>,
    /// Sections that carried content but resolved to no path.
    pub unparsed: Vec<UnparsedSection>,
}

/// Longest `UnparsedSection::header` sample retained, in characters.
const HEADER_SAMPLE_LIMIT: usize = 120;

// ─── Entry points ─────────────────────────────────────────────────────────────

/// Parse a unified diff into `(path, status, patch)` triples.
///
/// Why: Stage A needs per-file structured data; the raw diff is a flat string.
/// What: thin wrapper over [`parse_diff_files_detailed`] that discards the
/// unparsed-section report. Prefer the detailed form where the loss matters.
/// Test: `parse_diff_files_basic`, `parse_diff_files_new_file`.
pub fn parse_diff_files(diff: &str) -> Vec<(String, String, String)> {
    parse_diff_files_detailed(diff).files
}

/// Parse a unified diff, reporting sections that resolved to no file.
///
/// Why: a diff with N files must yield N entries, and any content the parser
/// cannot attribute must surface as a count rather than shrink the result
/// (#4458).
/// What: runs the state machine described in the module doc and returns both
/// halves of the outcome.
/// Test: `multi_file_diff_without_git_markers`, `each_file_shape_yields_one_entry`,
/// `concatenated_diff_yields_one_entry_per_file`, `headerless_hunks_are_reported_unparsed`.
pub fn parse_diff_files_detailed(diff: &str) -> ParsedDiff {
    let lines: Vec<&str> = diff.lines().collect();
    let mut out = ParsedDiff::default();
    let mut cur: Option<Section> = None;
    // Remaining old-side / new-side lines declared by the open `@@` header.
    let mut old_rem = 0usize;
    let mut new_rem = 0usize;
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let in_hunk_body = old_rem > 0 || new_rem > 0;

        // `diff --git` always starts a file. A well-formed hunk body never
        // carries one unprefixed, so this stays unconditional.
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(cur.take(), &mut out);
            let mut section = Section::new(line);
            let (old, new) = split_git_header_paths(rest);
            section.header_old = old;
            section.header_new = new;
            cur = Some(section);
            old_rem = 0;
            new_rem = 0;
            i += 1;
            continue;
        }

        // A `---`/`+++` pair outside a hunk body is a file header. Requiring
        // both halves keeps a deleted line spelled `-- x` (rendered `--- x`)
        // from reading as one.
        if !in_hunk_body
            && line.starts_with("--- ")
            && lines.get(i + 1).is_some_and(|n| n.starts_with("+++ "))
        {
            // #4458: the open record already has a path or hunks, so this pair
            // opens the NEXT file of a diff that carries no `diff --git`
            // markers. Without this flush every such pair rebound the same
            // record and the whole diff collapsed into one file.
            if cur.as_ref().is_some_and(Section::has_content) {
                flush(cur.take(), &mut out);
            }
            let section = cur.get_or_insert_with(|| Section::new(line));
            section.apply_from_header(line);
            section.apply_to_header(lines[i + 1]);
            i += 2;
            continue;
        }

        if !in_hunk_body && let Some((old, new)) = parse_hunk_counts(line) {
            old_rem = old;
            new_rem = new;
            let section = cur.get_or_insert_with(|| Section::new(line));
            section.saw_hunk = true;
            section.push(line);
            i += 1;
            continue;
        }

        let section = cur.get_or_insert_with(|| Section::new(line));
        if in_hunk_body {
            match line.as_bytes().first() {
                Some(b'-') => old_rem = old_rem.saturating_sub(1),
                Some(b'+') => new_rem = new_rem.saturating_sub(1),
                // `\ No newline at end of file` belongs to neither side.
                Some(b'\\') => {}
                _ => {
                    old_rem = old_rem.saturating_sub(1);
                    new_rem = new_rem.saturating_sub(1);
                }
            }
            section.push(line);
        } else if !section.apply_extended_header(line) {
            section.push(line);
        }
        i += 1;
    }

    flush(cur.take(), &mut out);
    out
}

// ─── Section state ────────────────────────────────────────────────────────────

/// One file's accumulated state while the parser walks its lines.
#[derive(Default)]
struct Section {
    /// Old path from the `diff --git` header, used when no other path exists.
    header_old: Option<String>,
    /// New path from the `diff --git` header, used when no other path exists.
    header_new: Option<String>,
    /// Path from `--- a/…`; the only source for a deleted file.
    from_path: Option<String>,
    /// Path from `+++ b/…`; the preferred source.
    to_path: Option<String>,
    /// Path from `rename to …` / `copy to …`.
    moved_to: Option<String>,
    /// Status stated outright by a mode or rename line.
    explicit_status: Option<&'static str>,
    /// Status implied by a `/dev/null` side.
    inferred_status: Option<&'static str>,
    patch: String,
    saw_hunk: bool,
    line_count: usize,
    has_non_blank: bool,
    first_line: String,
}

impl Section {
    fn new(first_line: &str) -> Self {
        Self {
            first_line: first_line.chars().take(HEADER_SAMPLE_LIMIT).collect(),
            ..Self::default()
        }
    }

    fn push(&mut self, line: &str) {
        self.patch.push_str(line);
        self.patch.push('\n');
        self.line_count += 1;
        if !line.trim().is_empty() {
            self.has_non_blank = true;
        }
    }

    /// Has this section already claimed a file? Used to decide whether an
    /// incoming `---`/`+++` pair belongs to it or opens the next file.
    fn has_content(&self) -> bool {
        self.saw_hunk || self.from_path.is_some() || self.to_path.is_some()
    }

    fn apply_from_header(&mut self, line: &str) {
        if line.starts_with("--- /dev/null") {
            self.inferred_status.get_or_insert("added");
        } else if let Some(path) = header_path(line) {
            self.from_path = Some(path);
        }
    }

    fn apply_to_header(&mut self, line: &str) {
        if line.starts_with("+++ /dev/null") {
            self.inferred_status.get_or_insert("removed");
        } else if let Some(path) = header_path(line) {
            self.to_path = Some(path);
        }
    }

    /// Consume a git extended-header line. Returns `true` when the line was a
    /// header (and so must not land in the patch body).
    fn apply_extended_header(&mut self, line: &str) -> bool {
        if line.starts_with("new file mode ") {
            self.explicit_status = Some("added");
        } else if line.starts_with("deleted file mode ") {
            self.explicit_status = Some("removed");
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            self.explicit_status = Some("renamed");
            self.moved_to = plain_path(rest);
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            self.explicit_status.get_or_insert("added");
            self.moved_to = plain_path(rest);
        } else if line.starts_with("rename from ") {
            self.explicit_status = Some("renamed");
        } else if !(line.starts_with("index ")
            || line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || line.starts_with("similarity index ")
            || line.starts_with("dissimilarity index ")
            || line.starts_with("copy from "))
        {
            return false;
        }
        self.line_count += 1;
        true
    }

    fn resolve_path(&self) -> Option<String> {
        self.to_path
            .clone()
            .or_else(|| self.moved_to.clone())
            .or_else(|| self.from_path.clone())
            .or_else(|| self.header_new.clone())
            .or_else(|| self.header_old.clone())
    }

    fn status(&self) -> &'static str {
        self.explicit_status
            .or(self.inferred_status)
            .unwrap_or("modified")
    }
}

/// Emit a finished section as either a file or an unparsed-section report.
fn flush(section: Option<Section>, out: &mut ParsedDiff) {
    let Some(section) = section else { return };
    if let Some(path) = section.resolve_path() {
        out.files
            .push((path, section.status().to_string(), section.patch));
    } else if section.has_non_blank {
        // #4458: never shrink the result silently — a section we could not name
        // is reported so the caller can log or reject on the count.
        out.unparsed.push(UnparsedSection {
            header: section.first_line,
            line_count: section.line_count,
        });
    }
}

// ─── Line helpers ─────────────────────────────────────────────────────────────

/// Read the old/new line budgets out of an `@@ -a,b +c,d @@` hunk header.
fn parse_hunk_counts(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let mut parts = rest[..end].split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((range_len(old), range_len(new)))
}

/// `start,count` → `count`; a bare `start` means one line.
fn range_len(spec: &str) -> usize {
    match spec.split_once(',') {
        Some((_, count)) => count.parse().unwrap_or(1),
        None => 1,
    }
}

/// Path from a `--- a/path` or `+++ b/path` header, ignoring a trailing
/// TAB-separated timestamp.
fn header_path(line: &str) -> Option<String> {
    let rest = line.get(4..)?;
    let rest = rest.split('\t').next().unwrap_or(rest);
    strip_src_prefix(rest)
}

/// Drop a leading `a/` or `b/` and reject the empty and `/dev/null` cases.
///
/// Only for the header forms that carry that prefix — `--- a/x`, `+++ b/x`, and
/// the `diff --git` header. `rename to` / `copy to` name the path directly, so
/// they use [`plain_path`]; stripping there would rewrite a real `b/` directory.
fn strip_src_prefix(path: &str) -> Option<String> {
    let path = path.trim();
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    plain_path(path)
}

/// Trim a path and reject the empty and `/dev/null` cases.
fn plain_path(path: &str) -> Option<String> {
    let path = path.trim();
    (!path.is_empty() && path != "/dev/null").then(|| path.to_string())
}

/// Split `a/<old> b/<new>` out of a `diff --git` header.
///
/// Git quotes paths containing spaces or control characters, which makes the
/// split ambiguous; those headers return `(None, None)` and the record falls
/// back to its `---`/`+++` pair, which git quotes the same way but which is
/// only missing for binary and mode-only files.
fn split_git_header_paths(rest: &str) -> (Option<String>, Option<String>) {
    let rest = rest.trim();
    if rest.starts_with('"') || rest.contains(" \"") {
        return (None, None);
    }
    let mut fallback = None;
    let mut start = 0usize;
    while let Some(offset) = rest[start..].find(" b/") {
        let mid = start + offset;
        let old = strip_src_prefix(&rest[..mid]);
        let new = strip_src_prefix(&rest[mid + 1..]);
        // A non-rename header has the same path on both sides; prefer the split
        // that produces one, so a path containing " b/" does not mis-split.
        if old.is_some() && old == new {
            return (old, new);
        }
        if fallback.is_none() {
            fallback = Some((old, new));
        }
        start = mid + 3;
    }
    fallback.unwrap_or((None, None))
}

// ─── Unit tests (extracted to keep this file under the 500-SLOC cap) ──────────

#[cfg(test)]
#[path = "diff_parser_tests.rs"]
mod tests;
