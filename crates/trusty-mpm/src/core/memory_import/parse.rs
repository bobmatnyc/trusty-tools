//! Pure frontmatter → drawer-field derivation for `tm memory import`.
//!
//! Why: the mapping from a memory `.md` file onto a palace drawer is the whole
//! substance of the import (issue #4837) and the only part worth unit-testing
//! in isolation — it must reproduce, byte-for-byte, the mapping an agent
//! applied by hand during the halted 2026-08-04 migration, or a re-run would
//! write divergent duplicates of the 120 drawers already stored.
//! What: [`parse_memory_file`] splits the YAML frontmatter from the body,
//! reads `name` / `description` / `metadata.type`, and derives the drawer
//! `text` and `tags`. The frontmatter scanner is deliberately purpose-built
//! rather than `serde_yaml`-backed: the established mapping stores the
//! description's *raw* scalar text, quotes included (many descriptions are
//! quoted headlines whose quotes are prose, not YAML syntax), which a real
//! YAML load would strip. Block scalars (`>`, `>-`, `|`, `|-`) are folded
//! here so the awkward multi-line form still parses.
//! Test: `parse::tests` in `../tests.rs`.

use std::collections::BTreeSet;

/// A memory file parsed into the fields a palace drawer needs.
///
/// Why: keeps the pure derivation (this module) separable from the HTTP write
/// path, so the mapping is testable without a running trusty-memory daemon.
/// What: the fact's own slug plus the exact `text` and `tags` to store.
/// Test: `derives_text_and_tags`, `appends_period_only_when_needed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMemory {
    /// Frontmatter `name` — the kebab slug, identical to the filename stem.
    /// Doubles as the idempotency key: it is always present in `tags`.
    pub name: String,
    /// Frontmatter `metadata.type` (`user` | `feedback` | `project` | `reference`).
    pub kind: String,
    /// Drawer body: the description, then a blank line, then the file body.
    pub text: String,
    /// Sorted, deduplicated tag set.
    pub tags: Vec<String>,
}

/// Split frontmatter from body and derive the drawer fields.
///
/// Why: this is the single entry point the importer calls per file; returning
/// `Ok(None)` for a file with no frontmatter lets the caller report an index
/// file such as `MEMORY.md` as a clean skip rather than an error.
/// What: requires the source to open with a `---` line; everything up to the
/// next `---` line is scanned as frontmatter, the remainder (trimmed) is the
/// body. `text` = description + `"\n\n"` + body, where a non-empty description
/// gains a trailing `.` unless it already ends in `.`, `:` or `"`. `tags` =
/// the sorted union of `name`, `metadata.type`, and every `[[wikilink]]`
/// target in the body. Returns `Err` when frontmatter exists but carries no
/// `name`, since the name is the idempotency key.
/// Test: `derives_text_and_tags`, `missing_frontmatter_is_none`,
/// `frontmatter_without_name_is_error`.
pub fn parse_memory_file(source: &str) -> anyhow::Result<Option<ParsedMemory>> {
    let Some((frontmatter, body)) = split_frontmatter(source) else {
        return Ok(None);
    };
    let fields = scan_frontmatter(frontmatter)?;
    if fields.name.is_empty() {
        anyhow::bail!("frontmatter has no `name` field");
    }

    let mut text = describe(&fields.description);
    text.push_str("\n\n");
    text.push_str(body.trim());

    let mut tags: BTreeSet<String> = BTreeSet::new();
    tags.insert(fields.name.clone());
    if !fields.kind.is_empty() {
        tags.insert(fields.kind.clone());
    }
    tags.extend(wikilink_targets(body));

    Ok(Some(ParsedMemory {
        name: fields.name,
        kind: fields.kind,
        text,
        tags: tags.into_iter().collect(),
    }))
}

/// Apply the description's trailing-punctuation rule.
///
/// Why: the stored description is a sentence lead-in to the body; the halted
/// migration appended a period only when the raw scalar did not already close
/// itself, and re-running must reproduce that exactly.
/// What: returns the trimmed description with a `.` appended unless it is
/// empty or already ends in `.`, `:` or `"`.
/// Test: `appends_period_only_when_needed`.
pub(super) fn describe(description: &str) -> String {
    let trimmed = description.trim();
    match trimmed.chars().last() {
        None => String::new(),
        Some('.') | Some(':') | Some('"') => trimmed.to_string(),
        Some(_) => format!("{trimmed}."),
    }
}

/// Split a leading `---`-fenced YAML block from the rest of the document.
///
/// Why: the body must be kept verbatim (markdown, code fences, wikilinks), so
/// the split is textual rather than a YAML round-trip.
/// What: returns `Some((frontmatter, body))` when the source's first
/// non-empty line is exactly `---` and a later line is exactly `---`.
/// Test: `missing_frontmatter_is_none`, `unterminated_frontmatter_is_none`.
pub(super) fn split_frontmatter(source: &str) -> Option<(&str, &str)> {
    let stripped = source.strip_prefix('\u{feff}').unwrap_or(source);
    let rest = stripped
        .strip_prefix("---\n")
        .or_else(|| stripped.strip_prefix("---\r\n"))?;
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

/// The three frontmatter fields the import consumes.
#[derive(Default)]
struct Fields {
    name: String,
    description: String,
    kind: String,
}

/// Scan the frontmatter block for `name`, `description`, and `metadata.type`.
///
/// Why: see the module doc — a real YAML load would strip quotes that the
/// established mapping keeps, and the importer needs only three fields, so a
/// focused line scanner is both more faithful and smaller than a full parse.
/// Being purpose-built, though, it must **fail closed** on any shape it does
/// not model: the alternative is importing a silently truncated description,
/// which the palace then holds as if it were the whole fact.
/// What: walks the block tracking the ancestor key chain by indentation, so
/// `type:` is read only as the direct child of a top-level `metadata:` — a
/// `type:` nested deeper is not the field we want. Values introduced by a `>`
/// or `|` block-scalar indicator are gathered from the following more-indented
/// lines and folded accordingly. Two shapes are hard errors rather than
/// silently-skipped lines: a line carrying no `key:` separator, and a line
/// indented under a key whose value was a plain (unindicated) scalar — that is
/// YAML's plain multi-line form, whose continuation this scanner cannot fold.
/// Test: `reads_nested_metadata_type`, `folds_block_scalar_description`,
/// `reads_literal_block_scalar_description`,
/// `plain_multiline_description_is_error`, `colonless_frontmatter_line_is_error`,
/// `type_below_metadata_child_depth_is_ignored`.
fn scan_frontmatter(frontmatter: &str) -> anyhow::Result<Fields> {
    let lines: Vec<&str> = frontmatter.lines().collect();
    let mut out = Fields::default();
    // Ancestor chain of the line being scanned, outermost first: (indent, key).
    let mut path: Vec<(usize, String)> = Vec::new();
    // The most recent key whose value was a non-empty plain scalar, if any —
    // anything indented under it is an unsupported multi-line continuation.
    let mut plain: Option<(usize, String)> = None;
    let mut i = 0usize;
    while i < lines.len() {
        let raw = lines[i];
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if let Some((plain_indent, plain_key)) = &plain
            && indent > *plain_indent
        {
            anyhow::bail!(
                "`{plain_key}` is a plain multi-line scalar (continuation {trimmed:?}); \
                 rewrite it as a single line or a `>`/`|` block scalar"
            );
        }
        let Some((key, head)) = trimmed.split_once(':') else {
            anyhow::bail!("frontmatter line has no `key:` separator: {trimmed:?}");
        };
        let key = key.trim().to_string();
        let head = head.trim();
        let multiline_risk = !head.is_empty() && block_style(head).is_none();
        let (value, next) = match block_style(head) {
            Some(style) => read_block_scalar(&lines, i + 1, indent, style),
            None => (head.to_string(), i + 1),
        };
        while path.last().is_some_and(|(depth, _)| *depth >= indent) {
            path.pop();
        }
        match key.as_str() {
            "name" if indent == 0 => out.name = value,
            "description" if indent == 0 => out.description = value,
            "type" if path.len() == 1 && path[0].1 == "metadata" => out.kind = value,
            _ => {}
        }
        plain = multiline_risk.then(|| (indent, key.clone()));
        path.push((indent, key));
        i = next;
    }
    Ok(out)
}

/// Whether a scalar value is a block-scalar indicator, and of which kind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockStyle {
    /// `>` — newlines inside a paragraph fold to spaces.
    Folded,
    /// `|` — newlines are preserved verbatim.
    Literal,
}

/// Classify a `key:` line's inline value as a block-scalar indicator.
///
/// Why: `description: >` (and `|`) put the real value on the following
/// indented lines; treating the indicator itself as the value would store a
/// literal `>`.
/// What: returns the style when the value is `>`/`|` optionally followed by a
/// chomping (`-`/`+`) or explicit-indent digit. Chomping only affects trailing
/// newlines, which the caller trims, so the indicators are accepted and
/// ignored.
/// Test: `folds_block_scalar_description`, `reads_literal_block_scalar_description`.
fn block_style(value: &str) -> Option<BlockStyle> {
    let mut chars = value.chars();
    let style = match chars.next()? {
        '>' => BlockStyle::Folded,
        '|' => BlockStyle::Literal,
        _ => return None,
    };
    if chars.all(|c| matches!(c, '-' | '+') || c.is_ascii_digit()) {
        Some(style)
    } else {
        None
    }
}

/// Gather and fold the indented lines that make up a block scalar.
///
/// Why: keeps [`scan_frontmatter`] linear — it hands off at the indicator and
/// resumes at the returned index.
/// What: consumes blank lines and lines indented deeper than `parent_indent`,
/// dedents them by the first content line's indentation, then joins: `Folded`
/// joins consecutive non-blank lines with a space and turns a blank line into
/// a newline; `Literal` joins every line with a newline. The result is
/// trimmed. Returns the folded value and the index of the first unconsumed
/// line.
/// Test: `folds_block_scalar_description`, `reads_literal_block_scalar_description`.
fn read_block_scalar(
    lines: &[&str],
    start: usize,
    parent_indent: usize,
    style: BlockStyle,
) -> (String, usize) {
    let mut collected: Vec<&str> = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            collected.push("");
            i += 1;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent <= parent_indent {
            break;
        }
        collected.push(line);
        i += 1;
    }
    let block_indent = collected
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let dedented: Vec<&str> = collected.iter().map(|l| dedent(l, block_indent)).collect();

    let value = match style {
        BlockStyle::Literal => dedented.join("\n"),
        BlockStyle::Folded => {
            let mut out = String::new();
            for line in &dedented {
                if line.trim().is_empty() {
                    out.push('\n');
                } else {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push(' ');
                    }
                    out.push_str(line.trim_end());
                }
            }
            out
        }
    };
    (value.trim().to_string(), i)
}

/// Drop `n` leading bytes of indentation from `line`.
///
/// Why: `n` is the block's minimum indent measured in *bytes*, and
/// `str::trim_start` also strips non-ASCII whitespace (U+00A0 and friends), so
/// a block indented with a mix of the two can put `n` inside a multi-byte
/// character — a raw slice there panics.
/// What: slices when `n` is a char boundary within the line, and otherwise
/// falls back to the line's content with all leading whitespace removed.
/// Test: `dedent_survives_multibyte_indentation`.
fn dedent(line: &str, n: usize) -> &str {
    line.get(n..).unwrap_or_else(|| line.trim_start())
}

/// Extract every `[[wikilink]]` target in the body as a tag-shaped slug.
///
/// Why: the memory corpus cross-references itself with wikilinks; projecting
/// those targets into tags is what makes the palace navigable by the same
/// graph the files encoded.
/// What: scans for `[[ … ]]` pairs and, for each, keeps the portion before a
/// `|` alias or `#` anchor, trims it, and strips a trailing `.md`. Targets
/// spanning a newline, or empty after normalisation, are dropped.
///
/// Deviation from the 2026-08-04 hand-derivation (recorded deliberately): that
/// pass dropped `.md`-suffixed targets outright rather than normalising them,
/// losing three genuine cross-references. Stripping the suffix reproduces
/// 145 of its 148 tag sets exactly and repairs the other three.
/// Test: `extracts_wikilinks`, `normalises_wikilink_targets`.
pub(super) fn wikilink_targets(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] != b'[' || bytes[i + 1] != b'[' {
            i += 1;
            continue;
        }
        let inner_start = i + 2;
        let Some(rel_end) = body[inner_start..].find("]]") else {
            break;
        };
        let inner = &body[inner_start..inner_start + rel_end];
        if let Some(target) = normalise_link_target(inner) {
            out.push(target);
        }
        i = inner_start + rel_end + 2;
    }
    out
}

/// Reduce a raw wikilink body to its target slug.
///
/// What: drops an `|alias` and a `#anchor`, trims, strips a trailing `.md`,
/// and rejects multi-line or empty results.
/// Test: `normalises_wikilink_targets`.
fn normalise_link_target(inner: &str) -> Option<String> {
    if inner.contains('\n') {
        return None;
    }
    let target = inner
        .split('|')
        .next()
        .unwrap_or(inner)
        .split('#')
        .next()
        .unwrap_or(inner)
        .trim();
    let target = target.strip_suffix(".md").unwrap_or(target).trim();
    (!target.is_empty()).then(|| target.to_string())
}
