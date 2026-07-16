//! `spec_refs:` YAML-frontmatter parsing for Markdown (DOC-38 §2.5).
//!
//! # Spec References
//!
//! - [`SPEC-SLD-02~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft)
//!
//! Why: Markdown declares its spec references canonically in YAML frontmatter —
//! a structured, directly machine-parseable form (§2.5, goal G3). A document
//! with no frontmatter declares no references (declared-links-only, G4), which
//! is a valid state, not an error.
//! What: [`parse_frontmatter_refs`] extracts the leading `---`-delimited block
//! and reads `spec_refs:` as a list of either structured maps (`id`/`path`/
//! `anchor`/`note`) or bare-string shorthand (parsed with the §2.2 regex).
//! [`has_frontmatter_spec_refs`] answers the linter's "is this file opted in?"
//! question. [`FrontmatterError`] is the structured (thiserror) schema-violation.
//! Test: `super::tests::frontmatter_*`.

use serde::Deserialize;

use super::grammar::reference_regex;

/// One resolved `spec_refs:` entry with its declaration line (1-based).
///
/// Why: the linter resolves each entry (path + anchor) and reports unresolved
/// ones by `file:line`; carrying the line the entry's `id` appears on gives an
/// actionable diagnostic without a second scan.
/// What: the `(id, path, anchor)` triple, an optional free-text `note`, and the
/// best-effort source `line` of the entry within the frontmatter block.
/// Test: `super::tests::frontmatter_maps_and_shorthand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterRef {
    /// The spec id (`SPEC-…~rev`).
    pub id: String,
    /// The repo-root-relative `.md` path (canonical form, §2.1).
    pub path: String,
    /// The in-file anchor (MUST equal `id`, §2.5 self-check — the linter checks).
    pub anchor: String,
    /// Optional free-text annotation (not resolved).
    pub note: Option<String>,
    /// Best-effort 1-based source line of the entry.
    pub line: usize,
}

/// A `spec_refs:` schema violation (DOC-38 §2.5).
///
/// Why: trusty-common is a library, so frontmatter failures are a structured
/// `thiserror` enum rather than `anyhow`; the linter maps each variant to a
/// `file:line` diagnostic.
/// What: malformed YAML, a `spec_refs` value that is neither a list nor absent,
/// an entry that is neither a map nor a string, a map missing a required key, or
/// a bare-string shorthand that does not match the §2.2 reference grammar.
/// Test: `super::tests::frontmatter_rejects_*`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrontmatterError {
    /// The frontmatter block is not valid YAML.
    #[error("invalid YAML frontmatter: {0}")]
    Yaml(String),
    /// `spec_refs` is present but is not a list.
    #[error("`spec_refs` must be a list")]
    NotAList,
    /// An entry is neither a structured map nor a bare string.
    #[error("spec_refs[{0}] must be a map or a string")]
    BadEntryKind(usize),
    /// A map entry is missing a required key (`id`/`path`/`anchor`).
    #[error("spec_refs[{index}] is missing required key `{key}`")]
    MissingKey {
        /// Zero-based entry index.
        index: usize,
        /// The absent required key.
        key: &'static str,
    },
    /// A bare-string entry does not match the §2.2 reference grammar.
    #[error("spec_refs[{index}] shorthand does not match the reference grammar: {raw:?}")]
    BadShorthand {
        /// Zero-based entry index.
        index: usize,
        /// The offending raw string.
        raw: String,
    },
}

/// The typed shell of a Markdown document's frontmatter (only `spec_refs`).
///
/// Why: modelling just `spec_refs` (with `#[serde(default)]`) lets serde ignore
/// every other frontmatter key, so a spec's other metadata never trips parsing.
/// What: a struct carrying the raw `spec_refs` list as untyped YAML values, so
/// each entry can be validated individually with precise errors.
/// Test: covered by `super::tests::frontmatter_*`.
#[derive(Deserialize, Default)]
struct RawFrontmatter {
    #[serde(default)]
    spec_refs: Option<serde_yaml::Value>,
}

/// True when a Markdown document declares any `spec_refs:` frontmatter.
///
/// Why: the linter's default (grandfathering) mode applies full spec-doc checks
/// only to files that have opted in — those carrying `spec_refs:` frontmatter
/// (or, in `--strict`, all specs). This is that opt-in predicate.
/// What: returns `true` when the leading frontmatter parses and carries a
/// non-null `spec_refs` key (even an empty list counts as opted-in).
/// Test: `super::tests::frontmatter_opt_in`.
#[must_use]
pub fn has_frontmatter_spec_refs(markdown: &str) -> bool {
    let Some((yaml, _)) = extract_frontmatter(markdown) else {
        return false;
    };
    serde_yaml::from_str::<RawFrontmatter>(yaml)
        .ok()
        .and_then(|f| f.spec_refs)
        .is_some_and(|v| !v.is_null())
}

/// Parse a Markdown document's `spec_refs:` frontmatter into resolved entries.
///
/// Why: the canonical Markdown declaration form is frontmatter (§2.5); the
/// linter resolves each entry and validates the schema. A document with no
/// frontmatter — or frontmatter without `spec_refs` — declares no references,
/// which is valid (returns an empty vec, not an error).
/// What: extracts the leading `---`-delimited block, deserializes `spec_refs`,
/// and validates each entry as either a structured map (`id`/`path`/`anchor`
/// required, `note` optional) or bare-string shorthand (parsed with the §2.2
/// regex). Returns the resolved [`FrontmatterRef`]s or the first
/// [`FrontmatterError`].
/// Test: `super::tests::frontmatter_maps_and_shorthand`,
/// `frontmatter_rejects_*`, `frontmatter_no_frontmatter_ok`.
pub fn parse_frontmatter_refs(markdown: &str) -> Result<Vec<FrontmatterRef>, FrontmatterError> {
    let Some((yaml, body_start)) = extract_frontmatter(markdown) else {
        return Ok(Vec::new());
    };
    let raw: RawFrontmatter =
        serde_yaml::from_str(yaml).map_err(|e| FrontmatterError::Yaml(e.to_string()))?;
    let Some(value) = raw.spec_refs else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let serde_yaml::Value::Sequence(entries) = value else {
        return Err(FrontmatterError::NotAList);
    };

    let fm_lines: Vec<&str> = yaml.lines().collect();
    let mut out = Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let parsed = parse_entry(&entry, index)?;
        let line = locate_line(&fm_lines, body_start, &parsed.id);
        out.push(FrontmatterRef { line, ..parsed });
    }
    Ok(out)
}

/// Validate one `spec_refs` entry (map or bare string) into a [`FrontmatterRef`].
///
/// Why: per-entry validation with a precise index yields actionable schema
/// diagnostics (`spec_refs[2] missing key path`) instead of an opaque
/// "did not match any variant".
/// What: a `Value::Mapping` requires `id`/`path`/`anchor` (string) and accepts
/// an optional `note`; a `Value::String` is parsed with the §2.2 regex; any
/// other value kind is a `BadEntryKind`. The returned `line` is a placeholder
/// (0) filled in by the caller.
/// Test: `super::tests::frontmatter_rejects_*`.
fn parse_entry(
    entry: &serde_yaml::Value,
    index: usize,
) -> Result<FrontmatterRef, FrontmatterError> {
    match entry {
        serde_yaml::Value::Mapping(map) => {
            let get = |k: &'static str| {
                map.get(serde_yaml::Value::String(k.to_string()))
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::to_string)
            };
            let id = get("id").ok_or(FrontmatterError::MissingKey { index, key: "id" })?;
            let path = get("path").ok_or(FrontmatterError::MissingKey { index, key: "path" })?;
            let anchor = get("anchor").ok_or(FrontmatterError::MissingKey {
                index,
                key: "anchor",
            })?;
            Ok(FrontmatterRef {
                id,
                path,
                anchor,
                note: get("note"),
                line: 0,
            })
        }
        serde_yaml::Value::String(raw) => {
            let caps =
                reference_regex()
                    .captures(raw)
                    .ok_or_else(|| FrontmatterError::BadShorthand {
                        index,
                        raw: raw.clone(),
                    })?;
            Ok(FrontmatterRef {
                id: caps[1].to_string(),
                path: caps[2].to_string(),
                anchor: caps[3].to_string(),
                note: None,
                line: 0,
            })
        }
        _ => Err(FrontmatterError::BadEntryKind(index)),
    }
}

/// Best-effort 1-based source line of an entry, by locating its `id` text.
///
/// Why: serde_yaml discards positions, but a `file:line` diagnostic is far more
/// actionable than "somewhere in the frontmatter"; locating the id's line is a
/// good-enough anchor.
/// What: scans the frontmatter lines for the first containing `id`, returning
/// its absolute 1-based line (`body_start` + offset); falls back to `body_start`
/// when not found.
/// Test: covered by `super::tests::frontmatter_maps_and_shorthand`.
fn locate_line(fm_lines: &[&str], body_start: usize, id: &str) -> usize {
    fm_lines
        .iter()
        .position(|l| l.contains(id))
        .map_or(body_start, |off| body_start + off)
}

/// Extract the leading `---`-delimited frontmatter block.
///
/// Why: frontmatter must be the very first content (§2.5); only a leading block
/// is canonical. Returning the body's start line lets the caller compute
/// absolute line numbers.
/// What: when `markdown` begins with a `---` line, returns the text between it
/// and the next `---` (or `...`) line, plus that body's 1-based start line.
/// Returns `None` when there is no leading, closed frontmatter block.
/// Test: `super::tests::frontmatter_no_frontmatter_ok`.
fn extract_frontmatter(markdown: &str) -> Option<(&str, usize)> {
    let mut lines = markdown.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    // Byte offset of the body (char after the first newline).
    let body_off = markdown.find('\n')? + 1;
    let rest = &markdown[body_off..];
    for (i, line) in rest.lines().enumerate() {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            let end = rest.lines().take(i).map(|l| l.len() + 1).sum::<usize>();
            // body_start line: line 1 is the opening `---`, so body begins at 2.
            return Some((&rest[..end], 2));
        }
    }
    None
}
