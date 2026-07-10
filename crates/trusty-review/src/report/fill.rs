//! Deterministic template fill engine (M1, #2313).
//!
//! Why: report templates use two placeholder forms — `{{field}}` scalars and
//! `<!-- BEGIN name --> … <!-- END name -->` repeatable blocks (one instance per
//! repository or finding).  The fill engine turns a filled data model into
//! markdown deterministically, with a strict honesty rule: any placeholder with
//! no value renders as `not stated in source data` — never left as `{{…}}` and
//! never invented.  No LLM is involved (M1 is deterministic only).
//! What: [`Scope`] is a tree of scalar values and named block-repetition lists;
//! [`render`] walks the template, expanding blocks per scope and substituting
//! scalars, and leaves `<!-- dataset: … -->` markers untouched.
//! Test: `fill_tests.rs` covers scalar substitution, the honesty marker, single
//! and repeated blocks, nested blocks, and dataset-comment preservation.

use std::collections::BTreeMap;

use regex::{Captures, Regex};

/// The string rendered for any placeholder that has no value.
///
/// Why: the report's honesty rule forbids leaving raw `{{…}}` in output and
/// forbids inventing values; a single, greppable sentinel makes gaps explicit.
/// What: the exact marker text mandated by the report spec.
/// Test: `fill_tests.rs::missing_scalar_renders_honesty_marker`.
pub const HONESTY_MARKER: &str = "not stated in source data";

/// A fill scope: scalar values plus named lists of child block scopes.
///
/// Why: templates nest repeatable blocks (a findings list inside a per-app
/// block); a recursive scope models that exactly — each block repetition gets
/// its own scope with its own scalars and nested blocks.
/// What: `scalars` maps a placeholder name to its value; `blocks` maps a block
/// name to the ordered scopes it should be repeated with.
/// Test: `fill_tests.rs` builds scopes directly and asserts rendered output.
#[derive(Debug, Default, Clone)]
pub struct Scope {
    scalars: BTreeMap<String, String>,
    blocks: BTreeMap<String, Vec<Scope>>,
}

impl Scope {
    /// Create an empty scope (no scalars, no blocks).
    ///
    /// Why: callers build scopes incrementally with `set`/`push_block`.
    /// What: returns the default (empty) scope.
    /// Test: exercised by every `fill_tests.rs` case.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a scalar placeholder value.
    ///
    /// Why: report/per-app fields are filled one at a time as data is resolved.
    /// What: inserts `key → value`, overwriting any prior value.
    /// Test: `fill_tests.rs::scalar_substitution`.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.scalars.insert(key.into(), value.into());
    }

    /// Set a scalar from an `Option`, skipping `None` so it falls to the marker.
    ///
    /// Why: much report data is optional; a `None` field must render as the
    /// honesty marker, which happens automatically when the key is absent.
    /// What: inserts only when `value` is `Some`.
    /// Test: `fill_tests.rs::missing_scalar_renders_honesty_marker`.
    pub fn set_opt(&mut self, key: impl Into<String>, value: Option<impl Into<String>>) {
        if let Some(v) = value {
            self.scalars.insert(key.into(), v.into());
        }
    }

    /// Append a child scope for a named repeatable block.
    ///
    /// Why: each repository (or finding) contributes one repetition of a block;
    /// callers push one child scope per repetition in order.
    /// What: appends `scope` to the list for `name`, preserving insertion order.
    /// Test: `fill_tests.rs::block_repeats_per_scope`.
    pub fn push_block(&mut self, name: impl Into<String>, scope: Scope) {
        self.blocks.entry(name.into()).or_default().push(scope);
    }
}

/// Render a template against a root scope, honouring the honesty rule.
///
/// Why: the single public entry point the reporter calls to turn a template +
/// data model into finished markdown.
/// What: compiles the placeholder regex once, then recursively expands blocks
/// and substitutes scalars.  Unknown scalars → [`HONESTY_MARKER`]; blocks with
/// provided scopes repeat once per scope; blocks with no provided scope render
/// NOTHING (#2342 omit-empty — the polish pass collapses the empty section and
/// records it under Gaps & Caveats, never a wall of markers).  `<!-- dataset: …
/// -->` comments are preserved verbatim.
/// Test: `fill_tests.rs` — all cases.
pub fn render(template: &str, scope: &Scope) -> String {
    // A compile-time-constant pattern; a failure here is a programmer error.
    let re = Regex::new(r"\{\{\s*([A-Za-z0-9_]+)\s*\}\}").expect("valid placeholder regex");
    render_scope(template, scope, &re)
}

/// Recursively expand blocks and substitute scalars for one scope level.
fn render_scope(text: &str, scope: &Scope, re: &Regex) -> String {
    match find_first_block(text) {
        None => substitute_scalars(text, scope, re),
        Some(block) => {
            let mut out = String::new();
            out.push_str(&substitute_scalars(&text[..block.pre_end], scope, re));

            let inner = &text[block.inner_start..block.inner_end];
            match scope.blocks.get(&block.name) {
                Some(children) if !children.is_empty() => {
                    for child in children {
                        out.push_str(&render_scope(inner, child, re));
                    }
                }
                _ => {
                    // Absent/empty block → render NOTHING (#2342 omit-empty): an
                    // unfilled repeatable section (risk register, findings band,
                    // benchmark row) is empty scaffolding, not data.  The post-
                    // render polish pass collapses the now-empty section to a
                    // single line and records it under Gaps & Caveats — no wall of
                    // honesty markers.  (Pre-wave-2 this rendered once with
                    // markers; the change is deliberate — see the omit-empty spec.)
                }
            }

            out.push_str(&render_scope(&text[block.post_start..], scope, re));
            out
        }
    }
}

/// Substitute `{{field}}` placeholders using a scope's scalars.
fn substitute_scalars(text: &str, scope: &Scope, re: &Regex) -> String {
    re.replace_all(text, |caps: &Captures| {
        let key = &caps[1];
        scope
            .scalars
            .get(key)
            .cloned()
            .unwrap_or_else(|| HONESTY_MARKER.to_string())
    })
    .into_owned()
}

/// Byte offsets describing the first top-level block found in a template.
struct BlockSpan {
    name: String,
    /// End of the text preceding the BEGIN marker (exclusive).
    pre_end: usize,
    /// Start of the block body (after the BEGIN marker).
    inner_start: usize,
    /// End of the block body (before the matching END marker).
    inner_end: usize,
    /// Start of the text following the END marker.
    post_start: usize,
}

/// Locate the first top-level `<!-- BEGIN name --> … <!-- END name -->` block.
///
/// Why: block expansion must process one block at a time, matching the correct
/// END even when the same block name is nested inside itself.
/// What: finds the first BEGIN marker, then scans for its matching END while
/// balancing nested same-name BEGIN/END pairs.  Returns `None` when no complete
/// block exists.  Differently-named nested blocks are handled by recursion into
/// the body (their markers don't affect this name's depth count).
/// Test: `fill_tests.rs::{block_repeats_per_scope, nested_blocks_expand}`.
fn find_first_block(text: &str) -> Option<BlockSpan> {
    let begin_re = Regex::new(r"<!--\s*BEGIN\s+([A-Za-z0-9_]+)\s*-->").expect("valid begin regex");
    let first = begin_re.captures(text)?;
    let whole = first.get(0)?;
    let name = first.get(1)?.as_str().to_string();

    let pre_end = whole.start();
    let inner_start = whole.end();

    let begin_marker = format!("<!-- BEGIN {name} -->");
    let end_marker = format!("<!-- END {name} -->");

    // Balance nested same-name blocks to find the matching END.
    let mut depth = 1usize;
    let mut cursor = inner_start;
    loop {
        let next_begin = find_from(text, &begin_marker, cursor);
        let next_end = find_from(text, &end_marker, cursor)?;

        match next_begin {
            Some(b) if b < next_end => {
                depth += 1;
                cursor = b + begin_marker.len();
            }
            _ => {
                depth -= 1;
                if depth == 0 {
                    return Some(BlockSpan {
                        name,
                        pre_end,
                        inner_start,
                        inner_end: next_end,
                        post_start: next_end + end_marker.len(),
                    });
                }
                cursor = next_end + end_marker.len();
            }
        }
    }
}

/// Find `needle` in `haystack` at or after byte offset `from`.
fn find_from(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    haystack[from..].find(needle).map(|i| i + from)
}

/// Strip a leading `<!-- … -->` instructional comment block from template
/// source before filling (live-QA defect, epic #2312 / #2314).
///
/// Why: the bundled templates open with a human-authored instructional comment
/// (placeholder syntax, the no-green-analysis rule, etc.) documenting HOW to
/// author a template instance. A generated report must never carry that
/// instructional text — and worse, the comment's own literal `{{field_name}}`
/// and `<!-- BEGIN … --> … <!-- END … -->` documentation examples would
/// otherwise be mistaken for real placeholders/blocks by [`render`] and mangled
/// into the output (a literal `per_application`-named example inside the header
/// would even get expanded with live per-application data). Stripping the
/// header before rendering removes both problems at the root, without touching
/// the on-disk template files (which still carry the instructions for a human
/// author).
/// What: if `template`, after trimming leading whitespace, does not start with
/// `<!--`, returns it unchanged (no leading comment to strip — covers a custom
/// template with no header, and guards against ever touching non-leading
/// comments). Otherwise scans forward balancing every `<!--`/`-->` token pair —
/// exactly like [`find_first_block`]'s nested-block balancing — so a literal
/// `<!--`/`-->` example embedded as documentation text inside the header does
/// not end the strip early; the strip point is where the outer comment's depth
/// returns to zero. Any blank line(s) immediately following the closing marker
/// are also trimmed. An unterminated leading comment (malformed template) is
/// left untouched rather than guessing a cut point.
/// Test: `fill_tests.rs::{strip_leading_comment_removes_header,
/// strip_leading_comment_noop_without_header,
/// strip_leading_comment_only_removes_first_block,
/// strip_leading_comment_unterminated_is_untouched}`.
pub fn strip_leading_comment(template: &str) -> &str {
    let trimmed_start = template.trim_start();
    if !trimmed_start.starts_with("<!--") {
        return template;
    }
    let leading_ws = template.len() - trimmed_start.len();
    let body = &template[leading_ws..];

    let mut depth = 1i32; // the leading "<!--" itself opens the outer comment
    let mut cursor = 4usize; // past that leading "<!--"
    loop {
        let next_open = find_from(body, "<!--", cursor);
        let next_close = find_from(body, "-->", cursor);
        match (next_open, next_close) {
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                cursor = o + 4;
            }
            (_, Some(c)) => {
                depth -= 1;
                cursor = c + 3;
                if depth == 0 {
                    let rest = &body[cursor..];
                    return rest.trim_start_matches(['\n', '\r']);
                }
            }
            _ => return template, // unterminated — leave the template untouched
        }
    }
}

#[cfg(test)]
#[path = "fill_tests.rs"]
mod tests;
