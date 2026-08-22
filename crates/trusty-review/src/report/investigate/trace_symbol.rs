//! Turning a finding's `file:line` citation into the symbol name the symbol
//! graph answers to (#6166).
//!
//! Why: a verified finding cites the line its evidence quote matched, and on the
//! engagement that drove this both RED findings cite a DOC-COMMENT line —
//! `usearch_store.rs:169` and `usearch_store.rs:163` both land inside the
//! 31-line block above `SHRINK_GUARD_RATIO_DIVISOR`, and `hnsw_store.rs:302`
//! lands inside the block above `HnswStore`. Asking the index for "the chunk
//! spanning line 169" answers nothing: no indexed chunk spans a doc comment.
//! The declaration is a few lines away in the file itself, so the file is what
//! gets read.
//!
//! What: [`resolve_symbol`] reads the citation's line, walks to the item
//! declaration that owns it, and returns the name — as `Type::method` when the
//! item is a method, because that is the only anchoring form
//! `GET /indexes/{id}/call_chain` disambiguates correctly (a bare `save`
//! resolves to whichever crate's `save` the graph saw first, see #6167).
//!
//! Test: `trace_symbol_tests.rs`.

/// A symbol the citation resolved to.
///
/// Why: the caller needs the anchoring form AND the line it came from — the
/// line is what the entry-file check reports when the two disagree.
/// What: `name` is the anchoring form (`Type::method` inside an `impl`, the
/// bare item name otherwise); `line` is the 1-based line of the declaration.
/// Test: `trace_symbol_tests::a_doc_comment_citation_scans_down_to_the_item`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSymbol {
    /// The anchoring form the call-chain endpoint is queried with.
    pub name: String,
    /// 1-based line of the declaration the citation resolved to.
    pub line: u64,
}

/// How far the scan may travel from the cited line before giving up.
///
/// The block above `SHRINK_GUARD_RATIO_DIVISOR` is 31 lines of doc comment and
/// the one above `HnswStore` is 15; 200 covers those with room to spare while
/// keeping a citation that landed in a page of comments from anchoring onto an
/// unrelated item far below.
const MAX_SCAN_LINES: usize = 200;

/// Whether the citation sits ABOVE its declaration and must scan DOWN.
///
/// Why an ordinary `//` comment is NOT in this set: a `//` comment inside a
/// function body belongs to the code above it, so scanning down walks past the
/// end of the body. That cost a real trace —
/// `payload_store/store.rs:413` is a `// Collect into an owned Vec…` line
/// inside `load_all`, and treating it as a header refused a finding that
/// anchors correctly by scanning up. A `///` or `//!` comment cannot appear
/// inside a body, so it is unambiguous.
/// Test: `trace_symbol_tests::a_plain_comment_inside_a_body_scans_up`.
fn scans_down(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty()
        || t.starts_with("///")
        || t.starts_with("//!")
        || t.starts_with("#[")
        || t.starts_with("#!")
}

/// Resolve the item declaration that owns a cited line.
///
/// Why: see the module docs — the citation is as likely to name a doc-comment
/// line as the declaration itself, and the two need opposite scan directions.
/// What: when the cited line is a doc comment, attribute, or blank (see
/// [`scans_down`]), scans DOWN to the first line that parses as a declaration;
/// otherwise scans UP. Returns `None`
/// when no declaration is found within [`MAX_SCAN_LINES`], which is the correct
/// answer for a citation into a non-Rust file, a manifest, or the middle of a
/// function body with no owning item in range — the caller fails that finding
/// closed rather than guessing a name.
/// Test: `trace_symbol_tests::{a_doc_comment_citation_scans_down_to_the_item,
/// a_body_citation_scans_up_to_its_function, a_citation_with_no_declaration_is_unresolved}`.
pub fn resolve_symbol(source: &str, line: u64) -> Option<ResolvedSymbol> {
    let lines: Vec<&str> = source.lines().collect();
    let idx = usize::try_from(line.saturating_sub(1)).ok()?;
    let cited = *lines.get(idx)?;

    let decl_idx = if scans_down(cited) {
        scan_down(&lines, idx)?
    } else {
        scan_up(&lines, idx)?
    };
    let (kind, name) = declaration(lines[decl_idx])?;
    let name = match enclosing_impl_type(&lines, decl_idx) {
        Some(ty) if kind == "fn" => format!("{ty}::{name}"),
        _ => name,
    };
    Some(ResolvedSymbol {
        name,
        line: decl_idx as u64 + 1,
    })
}

/// Scan forward for the first declaration at or after `idx`.
fn scan_down(lines: &[&str], idx: usize) -> Option<usize> {
    (idx..lines.len().min(idx + MAX_SCAN_LINES))
        .find(|&i| declaration(lines[i]).is_some() && !is_impl_header(lines[i]))
}

/// Scan backward for the first declaration at or before `idx`.
///
/// An `impl` header is skipped rather than accepted: a citation inside a method
/// body must anchor on the method, and the `impl` line is what
/// [`enclosing_impl_type`] reads separately to build `Type::method`.
fn scan_up(lines: &[&str], idx: usize) -> Option<usize> {
    (idx.saturating_sub(MAX_SCAN_LINES)..=idx)
        .rev()
        .find(|&i| declaration(lines[i]).is_some() && !is_impl_header(lines[i]))
}

/// Item keywords whose following token is the item's name.
const ITEM_KEYWORDS: &[&str] = &[
    "fn", "struct", "enum", "trait", "type", "const", "static", "union", "mod",
];

/// Tokens that may precede an item keyword without being one.
const MODIFIERS: &[&str] = &["pub", "async", "unsafe", "extern", "default", "\"C\""];

/// Whether a token is a modifier — including the `pub(crate)` / `pub(super)`
/// / `pub(in path)` spellings, which are one token after whitespace splitting
/// only when they carry no space (`pub(in ::foo)` splits, and its tail is
/// tolerated by the keyword scan simply not matching it).
fn is_modifier(tok: &str) -> bool {
    MODIFIERS.contains(&tok) || tok.starts_with("pub(")
}

/// The declared item's kind and name, or `None` when the line declares nothing.
///
/// Why: this is the whole Rust "parser" the trace pass needs. It is a token
/// scan, not a grammar: anything it cannot read returns `None` and the finding
/// fails closed, which is the same outcome as a citation into a `.ts` file.
/// The KIND is returned because only a function anchors as `Type::method` — a
/// nested `const` inside an `impl` keeps its bare name in the graph.
/// What: skips visibility/`async`/`unsafe`/`extern` modifiers, finds the first
/// item keyword, and returns it with the following token trimmed of generics,
/// parameter list, type ascription, and block/statement punctuation. `const fn`
/// reads as a function, not a constant.
/// Test: `trace_symbol_tests::{declaration_covers_the_item_forms,
/// a_non_rust_line_declares_nothing}`.
fn declaration(line: &str) -> Option<(&'static str, String)> {
    let mut toks = line.split_whitespace().skip_while(|t| is_modifier(t));
    let mut kw = toks.next()?;
    // `const fn foo` / `static` never precedes `fn`, but `const` does: the bare
    // `const` is a constant, `const fn` is a function.
    if kw == "const" && toks.clone().next() == Some("fn") {
        toks.next();
        kw = "fn";
    }
    let kind = ITEM_KEYWORDS.iter().find(|k| **k == kw)?;
    let raw = toks.next()?;
    let name: String = raw
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some((*kind, name))
}

/// Whether the line opens an `impl` block.
fn is_impl_header(line: &str) -> bool {
    line.trim_start().starts_with("impl")
}

/// The self type of the `impl` block enclosing `decl_idx`, if any.
///
/// Why: `Type::method` is the only form the call-chain endpoint disambiguates;
/// a bare method name resolves to whichever crate's same-named function the
/// graph indexed first (#6167).
/// What: scans up for an `impl` header indented strictly LESS than the
/// declaration — the containment test a brace parser would make, done by
/// indentation because this file is not a Rust parser. `impl Trait for Type`
/// yields `Type`; `impl Type` yields `Type`; generics and paths are stripped to
/// the final segment.
/// Test: `trace_symbol_tests::{a_method_anchors_as_type_colon_colon_method,
/// an_impl_for_a_trait_anchors_on_the_self_type,
/// a_free_function_anchors_on_its_bare_name}`.
fn enclosing_impl_type(lines: &[&str], decl_idx: usize) -> Option<String> {
    let decl_indent = indent_of(lines[decl_idx]);
    if decl_indent == 0 {
        return None; // a top-level item is inside no impl block
    }
    lines[decl_idx.saturating_sub(MAX_SCAN_LINES)..decl_idx]
        .iter()
        .rev()
        .find(|l| is_impl_header(l) && indent_of(l) < decl_indent)
        .and_then(|l| impl_self_type(l))
}

/// Leading-whitespace width of a line.
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The self type named by an `impl` header line.
fn impl_self_type(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("impl")?;
    // Drop the impl-level generic parameter list, e.g. `impl<'a, T: Copy>`.
    let rest = strip_leading_generics(rest.trim_start());
    // `impl Trait for Type` names the type after the LAST ` for `.
    let target = rest.rsplit_once(" for ").map_or(rest, |(_, t)| t);
    let target = target
        .split(['{', '<'])
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .next()?;
    let name = target.rsplit("::").next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Drop a leading `<…>` generic parameter list, honouring nesting.
fn strip_leading_generics(s: &str) -> &str {
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return s[i + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    s
}

#[cfg(test)]
#[path = "trace_symbol_tests.rs"]
mod tests;
