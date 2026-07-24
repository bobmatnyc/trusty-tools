//! Bidirectional SLD traceability gap report (issue #595 slice 1).
//!
//! Why: DOC-38 fixes the reference grammar and lets [`crate::checks`] resolve
//! each *declared* link, but a declared-links-only resolver (§1.2 G4) can never
//! answer "what is NOT yet declared?" — that requires actively enumerating both
//! sides of the traceability relationship (code units and spec sections) and
//! set-differencing them against what IS declared. Issue #595 scopes this as a
//! read-only, index-free, LLM-free report: slice 1 of a 6-slice epic. It
//! deliberately targets the DOC-38 grammar as shipped (`# Spec References` /
//! `spec_refs:`), not the issue's original superseded WWL/OpenFastTrace
//! four-status vocabulary.
//!
//! What: [`detect_units`] is a lightweight, deterministic, per-language regex
//! scan for public code-unit declarations (no AST, no code index — a pragmatic
//! symbol scan is explicitly sufficient for slice 1). [`backward_gaps`] pairs
//! those units against a file's already-parsed [`trusty_common::sld::Reference`]s
//! to find units with no directly preceding `# Spec References` declaration.
//! [`run_gap_report`] orchestrates a full repository scan: backward gaps (code
//! units missing linkage), forward gaps (spec sections under `docs/specs/**`
//! with no inbound code reference), and the existing reference-resolution
//! diagnostics ([`crate::checks::check_code_file`] /
//! [`crate::checks::check_markdown_refs`]) folded in as one coherent picture.
//! [`GapReport`] renders both machine-readable JSON ([`GapReport::to_json`]) and
//! a human summary ([`GapReport::summary`]).
//!
//! **Known pragmatic limitations (slice 1, by design):**
//! - Module-level units (crate/file doc blocks) are not tracked separately —
//!   only explicit `pub fn`/`struct`/`enum`/`trait`/`type`/`static`/`mod`
//!   declarations (or their Python/TS/JS equivalents) count as a code unit.
//! - A unit is "covered" only by a reference inside the CONTIGUOUS
//!   comment/docstring run immediately above its own declaration line — not
//!   by any reference anywhere between the previous unit and this one. This
//!   mirrors rustdoc's own attachment rule (a doc comment separated from its
//!   item by a blank line is not attached to it either) and specifically
//!   avoids a large struct/enum body's own reference block being mistaken for
//!   the linkage of the NEXT, unrelated `pub fn` that happens to follow it.
//!   See [`preceding_doc_block_start`].
//! - Because coverage looks only ABOVE the unit, a language whose idiomatic
//!   doc form sits BELOW/inside the declaration (e.g. a Python docstring as
//!   the function body's first statement) is not recognised as covering that
//!   unit — a documented false-positive-gap risk for that idiom in slice 1.
//! - Detection is line-oriented regex, not an AST: multi-line signatures,
//!   macro-generated items, and `pub(crate)`/`pub(super)` (intentionally
//!   excluded — only crate-external `pub` counts as a "public" unit) are out of
//!   scope. Plain `pub const`/`pub static` items ARE skipped (not detected as
//!   units) to avoid misparsing `pub const fn` — see [`detect_units`].
//! - Forward-gap "linked" membership only counts CODE references (inline
//!   `# Spec References` blocks), per the issue's wording ("no code unit
//!   linking to them") — a spec cross-referenced only from another spec's
//!   frontmatter is still a forward gap.
//!
//! Test: `super::tests::gap_*`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use trusty_common::sld::Reference;
use trusty_common::sld::{
    base_id, parse_inline_refs, spec_anchors, syntax_for_extension, CommentSyntax,
};

use crate::checks;
use crate::discover;
use crate::report::Diagnostic;

/// One publicly-declared code unit detected by [`detect_units`].
///
/// Why: the backward-gap view needs an addressable, orderable unit — its file,
/// line, and a human-readable kind/name — so a report can name exactly what
/// lacks linkage.
/// What: the repo-relative `path`, the 1-based declaration `line`, the `kind`
/// (`"fn"`, `"struct"`, `"enum"`, `"trait"`, `"type"`, `"static"`, `"mod"`,
/// `"class"`, `"interface"`, `"const"` depending on language), and the
/// declared `name`.
/// Test: `super::tests::gap_detect_units_rust`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeUnit {
    /// Repo-relative path of the file declaring this unit.
    pub path: String,
    /// 1-based source line of the declaration.
    pub line: usize,
    /// The declaration kind (language-specific keyword, lowercase).
    pub kind: String,
    /// The declared identifier.
    pub name: String,
}

/// One anchored spec section under `docs/specs/**`.
///
/// Why: the forward-gap view needs an addressable spec section — its
/// containing doc, id, and line — to report which sections have no inbound
/// code reference.
/// What: the repo-relative `path`, the section's `id` (`SPEC-…~rev`), and its
/// 1-based heading `line`.
/// Test: `super::tests::gap_forward_gap_detects_unlinked_section`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpecSection {
    /// Repo-relative path of the spec document.
    pub path: String,
    /// The section's anchored spec id.
    pub id: String,
    /// 1-based line of the anchored heading.
    pub line: usize,
}

/// The bidirectional SLD traceability gap report.
///
/// Why: issue #595 slice 1 asks for a single coherent picture — backward gaps,
/// forward gaps, and the existing reference-resolution diagnostics — rather
/// than three disconnected outputs.
/// What: `backward_gaps` (public code units with no directly preceding
/// `# Spec References` block), `forward_gaps` (spec sections with no inbound
/// code reference), `broken_references` (the existing reference-resolution
/// diagnostics: `ref-*`/`frontmatter-schema`), and scan counts.
/// Test: `super::tests::gap_run_report_*`.
#[derive(Debug, Default, Serialize)]
pub struct GapReport {
    /// Public code units with no directly preceding `# Spec References` block.
    pub backward_gaps: Vec<CodeUnit>,
    /// Spec sections with no inbound code (`# Spec References`) link.
    pub forward_gaps: Vec<SpecSection>,
    /// Existing reference-resolution findings (broken/mismatched references).
    pub broken_references: Vec<Diagnostic>,
    /// Total public code units scanned (in files with a known comment idiom).
    pub units_scanned: usize,
    /// Total anchored spec sections scanned.
    pub spec_sections_scanned: usize,
}

impl GapReport {
    /// True when nothing was found in any of the three views (backward,
    /// forward, or an error-severity broken reference).
    ///
    /// Why: `--strict` is the only way this report can fail a CI run (the CLI
    /// default is report-and-succeed, per issue #595); this is the strict
    /// predicate it checks.
    /// What: `backward_gaps` and `forward_gaps` are both empty, and no
    /// `broken_references` entry is `Severity::Error` (advisories, e.g.
    /// `ref-revision-drift`, never fail).
    /// Test: `super::tests::gap_is_strict_clean`.
    #[must_use]
    pub fn is_strict_clean(&self) -> bool {
        self.backward_gaps.is_empty()
            && self.forward_gaps.is_empty()
            && !self
                .broken_references
                .iter()
                .any(|d| d.severity == crate::report::Severity::Error)
    }

    /// Render the report as a `serde_json::Value` (machine-readable output).
    ///
    /// Why: issue #595 requires a `--json` output mode; a pure value builder
    /// (no I/O) keeps this testable independent of the CLI.
    /// What: a JSON object with `backward_gaps`, `forward_gaps`,
    /// `broken_references`, `units_scanned`, and `spec_sections_scanned`
    /// keys, mirroring this struct's fields.
    /// Test: `super::tests::gap_to_json_round_trips_counts`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "backward_gaps": self.backward_gaps,
            "forward_gaps": self.forward_gaps,
            "broken_references": self.broken_references,
            "units_scanned": self.units_scanned,
            "spec_sections_scanned": self.spec_sections_scanned,
        })
    }

    /// Render a human-readable summary: counts plus the top offenders.
    ///
    /// Why: a raw diagnostic dump does not answer "where should I start?"; a
    /// summary with the top files/specs by gap count does.
    /// What: total counts for each view, then up to 5 files with the most
    /// backward gaps and up to 5 spec docs with the most forward gaps, each
    /// with its count.
    /// Test: `super::tests::gap_summary_lists_top_offenders`.
    #[must_use]
    pub fn summary(&self) -> String {
        let errors = self
            .broken_references
            .iter()
            .filter(|d| d.severity == crate::report::Severity::Error)
            .count();
        let warnings = self.broken_references.len() - errors;

        let mut out = format!(
            "sld-lint gap-report: scanned {} code unit(s) + {} spec section(s)\n  \
             backward gaps (units with no spec link): {}\n  \
             forward gaps (spec sections with no code link): {}\n  \
             broken references: {errors} error(s), {warnings} warning(s)\n",
            self.units_scanned,
            self.spec_sections_scanned,
            self.backward_gaps.len(),
            self.forward_gaps.len(),
        );

        for (label, top) in [
            (
                "Top backward-gap files",
                top_offenders(&self.backward_gaps, |u| &u.path),
            ),
            (
                "Top forward-gap specs",
                top_offenders(&self.forward_gaps, |s| &s.path),
            ),
        ] {
            if top.is_empty() {
                continue;
            }
            out.push_str(&format!("  {label}:\n"));
            for (path, count) in top {
                out.push_str(&format!("    {count:>4}  {path}\n"));
            }
        }
        out
    }
}

/// Rank `items` by how many share each `key`, descending, top 5.
///
/// Why: [`GapReport::summary`] needs "which files have the most gaps" for both
/// the backward and forward views; one generic ranking helper serves both.
/// What: counts occurrences of `key(item)` across `items`, sorts descending by
/// count (ties broken by key for determinism), and returns at most 5
/// `(key, count)` pairs.
/// Test: covered by `super::tests::gap_summary_lists_top_offenders`.
fn top_offenders<'a, T>(items: &'a [T], key: impl Fn(&'a T) -> &'a str) -> Vec<(&'a str, usize)> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for item in items {
        let k = key(item);
        match counts.iter_mut().find(|(existing, _)| *existing == k) {
            Some((_, n)) => *n += 1,
            None => counts.push((k, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    counts.truncate(5);
    counts
}

/// The compiled Rust public-item declaration matcher.
///
/// Why: a `OnceLock` compiles the pattern once per process (mirrors
/// `trusty_common::sld::grammar`'s convention). Excluding `pub(crate)` /
/// `pub(super)` is deliberate — those are not part of the crate's public API;
/// requiring whitespace directly after `pub` naturally excludes them (`pub(`
/// has no space). `const` is treated only as a modifier (never a standalone
/// item keyword) so `pub const fn f()` cannot be misparsed as a `const` item
/// named `fn` — the cost is that a plain `pub const NAME: T = …;` constant is
/// not detected as a unit at all (documented limitation, module doc comment).
fn rust_unit_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"^pub\s+(?:async\s+|unsafe\s+|const\s+|extern\s+"[^"]*"\s+)*(fn|struct|enum|trait|type|mod|static)\s+([A-Za-z_][A-Za-z0-9_]*)"#,
        )
        .expect("rust unit pattern compiles")
    })
}

/// The compiled Python public-item declaration matcher (module-level only).
fn python_unit_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:async\s+)?(def|class)\s+([A-Za-z_][A-Za-z0-9_]*)")
            .expect("python unit pattern compiles")
    })
}

/// The compiled TypeScript/JavaScript exported-item declaration matcher.
fn ts_js_unit_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^export\s+(?:default\s+)?(?:abstract\s+|async\s+)*(function|class|interface|type|enum|const)\s+([A-Za-z_$][A-Za-z0-9_$]*)",
        )
        .expect("ts/js unit pattern compiles")
    })
}

/// Detect public code-unit declarations in `content` (per-extension regex scan).
///
/// Why: DOC-38's backward-gap question ("which units have no declared spec
/// link?") needs an enumeration of units; a full AST/code index is explicitly
/// out of scope for slice 1 (issue #595) — a deterministic, documented,
/// lightweight regex scan over each line is a pragmatic stand-in. Requiring
/// the match to start at column 0 (after trimming only leading whitespace, not
/// a comment lead-in) is what naturally excludes fenced/quoted example code
/// inside a doc comment or docstring: a real Rust `pub fn` is never itself
/// prefixed by `///`/`//!`, a real Python top-level `def`/`class` is never
/// itself indented inside a docstring, and a real `export` statement is never
/// itself prefixed by a JSDoc `*` continuation — so no separate fence-skipping
/// pass is needed here (unlike [`trusty_common::sld::parse_inline_refs`], which
/// scans comment *content* and does need one).
/// What: for `rs`, matches [`rust_unit_re`]; for `py`, matches
/// [`python_unit_re`] on lines with zero leading whitespace whose captured name
/// does not start with `_` (Python's "private" convention); for
/// `ts`/`tsx`/`js`/`mjs`/`cjs`, matches [`ts_js_unit_re`]. Any other extension
/// (including `sh`/`toml`/`yaml`, which have no code-unit concept) yields an
/// empty vec. Returned units are in file (line) order.
/// Test: `super::tests::gap_detect_units_rust`, `gap_detect_units_python`,
/// `gap_detect_units_ts`, `gap_detect_units_unsupported_ext`.
#[must_use]
pub fn detect_units(path: &str, content: &str, ext: &str) -> Vec<CodeUnit> {
    let mut out = Vec::new();
    for (idx, raw) in content.lines().enumerate() {
        let line = idx + 1;
        let unit = match ext {
            "rs" => {
                let trimmed = raw.trim_start();
                rust_unit_re()
                    .captures(trimmed)
                    .map(|c| (c[1].to_string(), c[2].to_string()))
            }
            "py" => {
                // Module-level only: real code has zero leading whitespace;
                // an indented `def`/`class` inside a docstring example or a
                // nested/method definition is out of scope for this
                // pragmatic scan.
                (raw == raw.trim_start())
                    .then(|| python_unit_re().captures(raw))
                    .flatten()
                    .map(|c| (c[1].to_string(), c[2].to_string()))
                    .filter(|(_, name)| !name.starts_with('_'))
            }
            "ts" | "tsx" | "js" | "mjs" | "cjs" => {
                let trimmed = raw.trim_start();
                ts_js_unit_re()
                    .captures(trimmed)
                    .map(|c| (c[1].to_string(), c[2].to_string()))
            }
            _ => None,
        };
        if let Some((kind, name)) = unit {
            out.push(CodeUnit {
                path: path.to_string(),
                line,
                kind,
                name,
            });
        }
    }
    out
}

/// True when a trimmed line is comment/docstring content under `syntax`.
///
/// Why: [`preceding_doc_block_start`] walks upward from a unit's declaration
/// line and must know, per line, whether it is still inside a comment or
/// docstring region — a coarser question than [`parse_inline_refs`]'s (which
/// also tracks the `# Spec References` marker and fenced code within it). This
/// mirrors the block/docstring open-close tracking `trusty_common::sld::inline`
/// applies internally, at the granularity this module needs: "is this line
/// comment content at all?"
/// What: for a block/docstring language (`syntax.block` is `Some`), tracks
/// `in_doc` across calls (threaded through `in_doc`) — a line inside an open
/// block, or one that opens/closes one, counts as comment content; otherwise
/// falls back to [`CommentSyntax::strip_line_comment`]. A blank line is never
/// comment content (a real doc comment or docstring line always carries its
/// own delimiter/prefix), which is what breaks contiguity at a blank
/// separator line.
/// Test: covered by `super::tests::gap_preceding_doc_block_*`.
pub(crate) fn is_comment_line(trimmed: &str, syntax: &CommentSyntax, in_doc: &mut bool) -> bool {
    if let Some((open, close)) = syntax.block {
        if *in_doc {
            if trimmed.contains(close) {
                *in_doc = false;
            }
            return true;
        }
        if let Some(idx) = trimmed.find(open) {
            let after = &trimmed[idx + open.len()..];
            *in_doc = !after.contains(close);
            return true;
        }
    }
    syntax.strip_line_comment(trimmed).is_some()
}

/// Find the first line of the contiguous comment/docstring run directly above
/// `unit_line` (1-based), or `unit_line` itself when there is none.
///
/// Why: the backward-gap fix (issue #595 PR #3783 review) — a unit is
/// "documented" only by its OWN immediately preceding doc block, never by a
/// reference that happens to sit anywhere between the previous unit and this
/// one (that previously let an unrelated `pub fn` following a large
/// documented `struct`'s body inherit that struct's coverage). Determining
/// comment-ness requires a forward scan from the top of the file (docstring
/// open/close is stateful), even though this function reports upward from
/// `unit_line`.
/// What: scans `content` top-to-bottom tracking [`is_comment_line`] per line;
/// returns the earliest line of the maximal contiguous comment run ending
/// immediately at `unit_line - 1`, or `unit_line` when line `unit_line - 1`
/// is not comment content (no preceding block at all) or `unit_line <= 1`.
/// Test: `super::tests::gap_preceding_doc_block_contiguous_run`,
/// `gap_preceding_doc_block_stops_at_blank_line`,
/// `gap_preceding_doc_block_none_when_no_comment_directly_above`.
#[must_use]
pub(crate) fn preceding_doc_block_start(
    content: &str,
    unit_line: usize,
    syntax: &CommentSyntax,
) -> usize {
    if unit_line <= 1 {
        return unit_line;
    }
    let mut in_doc = false;
    let mut run_start: Option<usize> = None;
    for (idx, raw) in content.lines().enumerate() {
        let line = idx + 1;
        if line >= unit_line {
            break;
        }
        if is_comment_line(raw.trim_start(), syntax, &mut in_doc) {
            run_start.get_or_insert(line);
        } else {
            run_start = None;
        }
    }
    // `run_start` only survives if the run reached all the way to `unit_line - 1`.
    run_start.unwrap_or(unit_line)
}

/// Find units in `units` (file order) with no directly preceding spec reference.
///
/// Why: a unit is "linked" when a reference is declared in the contiguous
/// comment/doc block immediately above its OWN declaration — not merely
/// somewhere after the previous unit (see [`preceding_doc_block_start`]'s doc
/// comment for the bug this fixes).
/// What: for each unit, computes its [`preceding_doc_block_start`] and flags
/// it as a gap when no `refs` entry's line falls in
/// `[block_start, unit.line)`.
/// Test: `super::tests::gap_backward_gaps_flags_undocumented_unit`,
/// `gap_backward_gaps_clears_documented_unit`,
/// `gap_backward_gaps_ref_inside_previous_unit_body_does_not_cover_next_unit`.
#[must_use]
pub fn backward_gaps(
    content: &str,
    syntax: &CommentSyntax,
    units: &[CodeUnit],
    refs: &[Reference],
) -> Vec<CodeUnit> {
    units
        .iter()
        .filter(|unit| {
            let block_start = preceding_doc_block_start(content, unit.line, syntax);
            !refs
                .iter()
                .any(|r| r.line >= block_start && r.line < unit.line)
        })
        .cloned()
        .collect()
}

/// Run the full bidirectional gap report over a repository checkout.
///
/// Why: this is the single entry point the `gap-report` subcommand calls — it
/// wires the same [`discover::discover`] scope the linter itself uses, so the
/// gap report and the linter agree on what is in scope, and folds in the
/// existing reference-resolution checks so the result is one coherent picture
/// (issue #595 slice 1). Unlike [`crate::run`], no spec catalog is required —
/// the gap views need only the spec docs' own anchors and the code files'
/// declared references.
/// What: for each in-scope code file, resolves its inline references
/// ([`checks::check_code_file`]) into `broken_references`, detects public units
/// ([`detect_units`]), and folds any [`backward_gaps`] into the report; also
/// records every referenced base id. For each in-scope spec doc, resolves its
/// frontmatter ([`checks::check_markdown_refs`]) into `broken_references`, then
/// flags any anchored section ([`trusty_common::sld::spec_anchors`]) whose base
/// id was never referenced by a code file as a forward gap. Files that cannot
/// be read as UTF-8 are skipped, matching [`crate::run`]'s behaviour.
/// Test: `super::tests::gap_run_report_backward_and_forward`.
#[must_use]
pub fn run_gap_report(root: &Path) -> GapReport {
    let discovered = discover::discover(root);
    let lookup = |path: &str| crate::safe_read(root, path);

    let mut report = GapReport::default();
    let mut code_referenced_base_ids: HashSet<String> = HashSet::new();

    for rel in &discovered.code_files {
        let Some(content) = crate::read_rel(root, rel) else {
            continue;
        };
        let ext = rel.extension().and_then(|e| e.to_str()).unwrap_or("");
        let path = rel.to_string_lossy().to_string();

        report
            .broken_references
            .extend(checks::check_code_file(&path, &content, ext, &lookup));

        let Some(syntax) = syntax_for_extension(ext) else {
            continue;
        };
        let refs = parse_inline_refs(&content, &syntax);
        code_referenced_base_ids.extend(refs.iter().map(|r| base_id(&r.id).to_string()));

        let units = detect_units(&path, &content, ext);
        report.units_scanned += units.len();
        report
            .backward_gaps
            .extend(backward_gaps(&content, &syntax, &units, &refs));
    }

    for rel in &discovered.spec_docs {
        let Some(content) = crate::read_rel(root, rel) else {
            continue;
        };
        let path = rel.to_string_lossy().to_string();

        report
            .broken_references
            .extend(checks::check_markdown_refs(&path, &content, &lookup));

        let anchors = spec_anchors(&content);
        report.spec_sections_scanned += anchors.len();
        for a in anchors {
            if !code_referenced_base_ids.contains(base_id(&a.id)) {
                report.forward_gaps.push(SpecSection {
                    path: path.clone(),
                    id: a.id,
                    line: a.line,
                });
            }
        }
    }

    report
}
