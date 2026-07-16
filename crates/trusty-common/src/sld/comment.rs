//! Per-language comment/docstring idioms for inline SLD blocks (DOC-38 §3).
//!
//! # Spec References
//!
//! - [`SPEC-SLD-03~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft)
//!
//! Why: DOC-38 meets each language in its native idiom — the ONLY per-language
//! surface a resolver needs (§3, Annex A.2). Everything downstream (marker,
//! reference grammar, anchor resolution) is shared. Isolating the table here
//! keeps that "one small lookup" claim literally true.
//! What: [`CommentSyntax`] (the line-comment prefixes + optional block/docstring
//! delimiters for a file type) and [`syntax_for_extension`] — the extension →
//! syntax map for Rust, Python, TS/JS, shell, and TOML/YAML. Markdown is
//! intentionally absent: it declares references via frontmatter (§2.5), not
//! inline blocks.
//! Test: `super::tests::comment_*`.

/// The comment/docstring idiom for one file type (DOC-38 §3, Annex A.2).
///
/// Why: inline `# Spec References` blocks live inside a language's comments or
/// docstrings; parsing them needs the line-comment prefixes to strip and, for
/// docstring languages, the block delimiters that bound a multi-line region in
/// which non-prefixed reference lines are still in scope.
/// What: `line_prefixes` are the line-comment lead-ins (longest-first is applied
/// by the caller); `block` is the optional `(open, close)` delimiter pair for a
/// docstring / block-comment region (`None` for line-comment-only languages).
/// Test: `super::tests::comment_syntax_table`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentSyntax {
    /// Line-comment lead-ins (e.g. `//!`, `///`, `//` for Rust; `#` for shell).
    pub line_prefixes: &'static [&'static str],
    /// Optional `(open, close)` docstring / block-comment delimiters.
    pub block: Option<(&'static str, &'static str)>,
}

impl CommentSyntax {
    /// Strip the longest matching line-comment prefix from a trimmed line.
    ///
    /// Why: the marker and reference grammar apply to the comment *content*, so
    /// the lead-in must be removed first; matching the longest prefix ensures
    /// `//!`/`///` win over `//` (Rust) rather than leaving a stray `/` or `!`.
    /// What: given an already-`trim_start`ed line, returns the remainder after
    /// the longest matching prefix (and one optional following space), or `None`
    /// when no prefix matches.
    /// Test: `super::tests::comment_strip_line`.
    #[must_use]
    pub fn strip_line_comment<'a>(&self, trimmed: &'a str) -> Option<&'a str> {
        let best = self
            .line_prefixes
            .iter()
            .filter(|p| trimmed.starts_with(**p))
            .max_by_key(|p| p.len())?;
        let rest = &trimmed[best.len()..];
        Some(rest.strip_prefix(' ').unwrap_or(rest))
    }
}

/// Rust line-comment idiom: `//!`, `///`, `//` (no docstring block).
const RUST: CommentSyntax = CommentSyntax {
    line_prefixes: &["//!", "///", "//"],
    block: None,
};

/// Python idiom: `#` line comments plus `"""` / `'''` docstring regions.
const PYTHON: CommentSyntax = CommentSyntax {
    line_prefixes: &["#"],
    block: Some(("\"\"\"", "\"\"\"")),
};

/// TypeScript / JavaScript idiom: `//` line comments plus `/** … */` JSDoc.
const TS_JS: CommentSyntax = CommentSyntax {
    line_prefixes: &["//"],
    block: Some(("/*", "*/")),
};

/// Shell / TOML / YAML idiom: `#` line comments only.
const HASH: CommentSyntax = CommentSyntax {
    line_prefixes: &["#"],
    block: None,
};

/// Look up the [`CommentSyntax`] for a file extension (lowercase, no dot).
///
/// Why: the per-extension table is the single language-specific surface of the
/// SLD grammar (DOC-38 Annex A.2). An unknown extension yields `None` — the
/// resolver never guesses an idiom (declared-links-only, §1.2 G4).
/// What: maps `rs` → Rust, `py` → Python, `ts`/`tsx`/`js`/`mjs`/`cjs` → TS/JS,
/// `sh`/`bash` → shell, `toml`/`yaml`/`yml` → hash-comment. Markdown (`md`) is
/// deliberately excluded — it declares references via frontmatter (§2.5).
/// Test: `super::tests::comment_syntax_table`.
#[must_use]
pub fn syntax_for_extension(ext: &str) -> Option<CommentSyntax> {
    match ext {
        "rs" => Some(RUST),
        "py" => Some(PYTHON),
        "ts" | "tsx" | "js" | "mjs" | "cjs" => Some(TS_JS),
        "sh" | "bash" => Some(HASH),
        "toml" | "yaml" | "yml" => Some(HASH),
        _ => None,
    }
}
