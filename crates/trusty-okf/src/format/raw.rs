//! Raw-preserving frontmatter: split, top-level entries and patch render
//! (SPEC-OKF-01 §2.2.1).
//!
//! Why: a read followed by a write must keep every frontmatter entry the write
//! does not own byte for byte, including comments, flow style and values the
//! typed view cannot hold. Re-serialising the parsed mapping loses all of
//! that, so the raw text is the source of truth and a write patches it.
//! What: [`split_raw`] finds the `---` fenced block without normalising any
//! byte; [`read_raw`] splits the block into top-level entries and parses each
//! one alone; [`patch`] rewrites only the entries whose value changed, drops
//! removed ones and appends new ones after the last entry.
//! Test: `raw_split_keeps_every_byte`, `patch_rewrites_only_changed_entries`,
//! `unsplittable_frontmatter_is_not_patchable`.

use std::ops::Range;

use serde_yaml::{Mapping, Value};

use crate::format::frontmatter::sort_value;

/// The stored frontmatter of a concept file, byte for byte.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawFrontmatter {
    /// The whole frontmatter block, both `---` fence lines included. Empty
    /// when the file has no frontmatter.
    pub text: String,
    /// The top-level entries in file order. Each span indexes into `text`.
    pub entries: Vec<RawEntry>,
    /// False when the block could not be split into entries that each parse
    /// alone (an anchor shared across keys, a complex key, a non-string key).
    /// Such a concept reads normally, and every write to it is refused with
    /// `Parse`.
    pub patchable: bool,
}

/// One top-level frontmatter entry: the byte span from its key line to the
/// line before the next top-level key, comments and blank lines included.
#[derive(Debug, Clone, PartialEq)]
pub struct RawEntry {
    /// The key.
    pub key: String,
    /// The byte range in [`RawFrontmatter::text`].
    pub span: Range<usize>,
    /// The value parsed from this span alone.
    pub value: Value,
}

/// Locate the frontmatter block of a document without changing a byte.
///
/// What: a document has frontmatter when its first line is exactly `---`
/// (LF or CRLF) and a later line is exactly `---`. Returns
/// `(block_end, inner)`: the block is `text[..block_end]`, both fences
/// included, and `inner` is the YAML between the fences. `None` means no
/// frontmatter; an unclosed fence is no frontmatter either, as in
/// [`super::frontmatter::split`].
/// Test: `raw_split_keeps_every_byte`.
pub fn split_raw(text: &str) -> Option<(usize, Range<usize>)> {
    let open = if text.starts_with("---\n") {
        4
    } else if text.starts_with("---\r\n") {
        5
    } else {
        return None;
    };
    let mut pos = open;
    while pos < text.len() {
        let line_end = text[pos..].find('\n').map_or(text.len(), |i| pos + i + 1);
        let line = text[pos..line_end]
            .trim_end_matches('\n')
            .trim_end_matches('\r');
        if line == "---" {
            return Some((line_end, open..pos));
        }
        pos = line_end;
    }
    None
}

/// Whether a line of the YAML block starts a top-level key.
fn is_key_line(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    if first.is_whitespace() || "#-[]{},*!|>%@`?".contains(first) {
        return false;
    }
    let trimmed = line.trim_end();
    trimmed.contains(": ") || trimmed.contains(":\t") || trimmed.ends_with(':')
}

/// Split the block `text` (fences included) into top-level entries.
///
/// What: returns the entries and whether they are patchable. Entries are
/// patchable when every span parses alone to a one-key mapping with a string
/// key, and the keys and values equal `whole`, the mapping parsed from the
/// full block. Otherwise the entry list is empty and `patchable` is false.
/// Test: `patch_rewrites_only_changed_entries`,
/// `unsplittable_frontmatter_is_not_patchable`.
pub fn read_raw(text: &str, inner: Range<usize>, whole: &Mapping) -> RawFrontmatter {
    let mut starts = Vec::new();
    let mut pos = inner.start;
    while pos < inner.end {
        let line_end = text[pos..inner.end]
            .find('\n')
            .map_or(inner.end, |i| pos + i + 1);
        if is_key_line(&text[pos..line_end]) {
            starts.push(pos);
        }
        pos = line_end;
    }
    let mut entries = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(inner.end);
        match parse_one_entry(&text[start..end]) {
            Some((key, value)) => entries.push(RawEntry {
                key,
                span: start..end,
                value,
            }),
            None => return unpatchable(text),
        }
    }
    let same = entries.len() == whole.len()
        && entries
            .iter()
            .all(|e| whole.get(e.key.as_str()) == Some(&e.value));
    if !same {
        return unpatchable(text);
    }
    RawFrontmatter {
        text: text.to_string(),
        entries,
        patchable: true,
    }
}

fn unpatchable(text: &str) -> RawFrontmatter {
    RawFrontmatter {
        text: text.to_string(),
        entries: Vec::new(),
        patchable: false,
    }
}

/// Parse one span to its single string key and value.
fn parse_one_entry(span: &str) -> Option<(String, Value)> {
    let Ok(Value::Mapping(map)) = serde_yaml::from_str::<Value>(span) else {
        return None;
    };
    if map.len() != 1 {
        return None;
    }
    let (key, value) = map.into_iter().next()?;
    match key {
        Value::String(k) => Some((k, value)),
        _ => None,
    }
}

/// Render one entry as `key: value` YAML, nested mappings key-sorted.
pub(crate) fn render_entry(key: &str, value: &Value) -> String {
    let mut one = Mapping::new();
    one.insert(Value::String(key.to_string()), sort_value(value));
    serde_yaml::to_string(&Value::Mapping(one)).unwrap_or_default()
}

/// Patch a stored block to hold `values`.
///
/// Why: §2.2.1 "writes change only owned entries".
/// What: walks the stored entries in file order. An entry whose value equals
/// `values[key]` keeps its bytes; a changed one is re-rendered in place; one
/// absent from `values` is dropped. Keys new to `values` are appended,
/// key-sorted, after the last stored entry. The fences and any text before
/// the first entry keep their bytes. Returns `None` when `raw` has no block
/// or is not patchable.
/// Test: `patch_rewrites_only_changed_entries`.
pub fn patch(raw: &RawFrontmatter, values: &Mapping) -> Option<String> {
    if !raw.patchable {
        return None;
    }
    let text = raw.text.as_str();
    let (_, inner) = split_raw(text)?;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..inner.start]);
    let mut pos = inner.start;
    for entry in &raw.entries {
        out.push_str(&text[pos..entry.span.start]);
        match values.get(entry.key.as_str()) {
            Some(v) if *v == entry.value => out.push_str(&text[entry.span.clone()]),
            Some(v) => out.push_str(&render_entry(&entry.key, v)),
            None => {}
        }
        pos = entry.span.end;
    }
    out.push_str(&text[pos..inner.end]);
    let mut added: Vec<(&str, &Value)> = values
        .iter()
        .filter_map(|(k, v)| k.as_str().map(|k| (k, v)))
        .filter(|(k, _)| !raw.entries.iter().any(|e| e.key == *k))
        .collect();
    added.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in added {
        out.push_str(&render_entry(k, v));
    }
    out.push_str(&text[inner.end..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(doc: &str) -> RawFrontmatter {
        let (end, inner) = split_raw(doc).expect("fenced");
        let whole = match serde_yaml::from_str::<Value>(&doc[inner.clone()]).unwrap() {
            Value::Mapping(m) => m,
            other => panic!("not a mapping: {other:?}"),
        };
        read_raw(&doc[..end], inner, &whole)
    }

    /// Why: the split must not normalise a single byte, CRLF included.
    /// What: splits LF and CRLF documents and an unclosed fence.
    /// Test: self-contained.
    #[test]
    fn raw_split_keeps_every_byte() {
        let doc = "---\ntype: A\n---\n\nBody\n";
        let (end, inner) = split_raw(doc).unwrap();
        assert_eq!(&doc[..end], "---\ntype: A\n---\n");
        assert_eq!(&doc[inner], "type: A\n");
        let crlf = "---\r\ntype: A\r\n---\r\nBody";
        let (end, _) = split_raw(crlf).unwrap();
        assert_eq!(&crlf[end..], "Body");
        assert!(split_raw("---\ntype: A\n").is_none());
        assert!(split_raw("# no fence\n").is_none());
    }

    /// Why: §2.2.1 — only owned entries change; comments, flow style and
    /// unknown keys elsewhere keep their bytes.
    /// What: changes one key, removes one, adds one; checks every other span.
    /// Test: self-contained.
    #[test]
    fn patch_rewrites_only_changed_entries() {
        let doc = "---\n# lead comment\ntype: A\nodd: {b: 2, a: 1}  # keep me\ntags:\n- x\nstatus: draft\n---\n";
        let raw = read(doc);
        assert!(raw.patchable);
        assert_eq!(raw.entries.len(), 4);
        let mut values = Mapping::new();
        for e in &raw.entries {
            values.insert(Value::String(e.key.clone()), e.value.clone());
        }
        assert_eq!(
            patch(&raw, &values).unwrap(),
            doc,
            "no change writes the same bytes"
        );
        values.insert("status".into(), "deprecated".into());
        values.shift_remove("tags");
        values.insert("new_key".into(), Value::Bool(true));
        let out = patch(&raw, &values).unwrap();
        assert_eq!(
            out,
            "---\n# lead comment\ntype: A\nodd: {b: 2, a: 1}  # keep me\nstatus: deprecated\nnew_key: true\n---\n"
        );
    }

    /// Why: an alias shared across keys cannot be split; a write must refuse
    /// rather than guess.
    /// What: asserts `patchable` is false and `patch` returns `None`.
    /// Test: self-contained.
    #[test]
    fn unsplittable_frontmatter_is_not_patchable() {
        let raw = read("---\ntype: &t A\nkind: *t\n---\n");
        assert!(!raw.patchable);
        assert!(patch(&raw, &Mapping::new()).is_none());
    }
}
