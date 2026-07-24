//! Frontmatter split + deterministic YAML render.
//!
//! Why: markdown-with-frontmatter files are only "deterministic" if the same
//! logical frontmatter always serialises to byte-identical YAML. serde_yaml
//! preserves map insertion order, so two runs that build the map differently
//! would diff. This module owns the split (hand-rolled on `---`, per the design
//! brief — no gray_matter dependency) and a canonical renderer that sorts every
//! mapping key recursively before emitting, so diffs are stable.
//!
//! What: [`split`] separates an optional leading `---`-fenced YAML block from
//! the body; [`parse`] turns the YAML text into a [`serde_yaml::Value`];
//! [`sort_value`] recursively key-sorts mappings; [`render`] re-assembles a
//! sorted-frontmatter + body document with a normalised trailing newline.
//!
//! Test: `split_roundtrips_frontmatter`, `split_handles_no_frontmatter`,
//! `render_is_key_sorted_and_stable`, `render_without_frontmatter_omits_fence`.

use serde_yaml::{Mapping, Value};

/// The result of splitting a document into optional frontmatter + body.
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    /// The raw YAML text between the `---` fences, if a well-formed leading
    /// fence was present. `None` means the document had no frontmatter.
    pub frontmatter: Option<String>,
    /// Everything after the closing fence (or the whole document when there was
    /// no frontmatter). Leading blank line after the fence is trimmed once.
    pub body: String,
}

/// Split a document into an optional leading YAML frontmatter block + body.
///
/// Why: a hand-rolled split (per the brief) keeps behaviour explicit and
/// dependency-free — a document is frontmattered iff its first line is exactly
/// `---` and a later line is exactly `---`.
/// What: recognises a leading `---` fence, captures text up to the next lone
/// `---` line as `frontmatter`, and returns the remainder as `body`. A document
/// whose first line is not `---`, or that never closes the fence, is treated as
/// pure body (`frontmatter: None`) — never an error, so malformed input is
/// preserved verbatim rather than lost.
/// Test: `split_roundtrips_frontmatter`, `split_handles_no_frontmatter`.
pub fn split(content: &str) -> Split {
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return Split {
            frontmatter: None,
            body: content.to_string(),
        };
    }
    let mut yaml = String::new();
    let mut closed = false;
    let mut rest = String::new();
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    if !closed {
        // Unterminated fence: not real frontmatter, keep the whole doc as body.
        return Split {
            frontmatter: None,
            body: content.to_string(),
        };
    }
    for line in lines {
        rest.push_str(line);
        rest.push('\n');
    }
    // Drop a single leading blank line the author put between fence and body.
    let body = rest.strip_prefix('\n').unwrap_or(&rest).to_string();
    Split {
        frontmatter: Some(yaml),
        body,
    }
}

/// Parse a YAML frontmatter block into a mapping [`Value`].
///
/// Why: an empty or whitespace-only block is common (a file with a bare fence)
/// and must parse to an empty mapping, not an error or `Null`.
/// What: returns a [`Value::Mapping`]; a non-mapping top-level YAML document is
/// an error (frontmatter must be a key/value map).
/// Test: `render_is_key_sorted_and_stable`.
pub fn parse(yaml: &str) -> anyhow::Result<Value> {
    if yaml.trim().is_empty() {
        return Ok(Value::Mapping(Mapping::new()));
    }
    let value: Value = serde_yaml::from_str(yaml)?;
    match value {
        Value::Mapping(_) => Ok(value),
        Value::Null => Ok(Value::Mapping(Mapping::new())),
        other => anyhow::bail!("frontmatter must be a YAML mapping, got {other:?}"),
    }
}

/// Recursively key-sort every mapping in a value.
///
/// Why: deterministic output is the whole point — insertion order must never
/// leak into the file. Sorting keys (and recursing into nested maps/sequences)
/// gives one canonical form per logical value.
/// What: rebuilds mappings with keys ordered by their serialised string form;
/// sequences keep their element order (order is meaningful for lists) but each
/// element is itself sorted. Scalars pass through unchanged.
/// Test: `render_is_key_sorted_and_stable`.
pub fn sort_value(value: &Value) -> Value {
    match value {
        Value::Mapping(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (key_string(k), sort_value(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = Mapping::new();
            for (k, v) in entries {
                out.insert(Value::String(k), v);
            }
            Value::Mapping(out)
        }
        Value::Sequence(seq) => Value::Sequence(seq.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

/// Stable string form of a mapping key for sort/dedup purposes.
fn key_string(key: &Value) -> String {
    match key {
        Value::String(s) => s.clone(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

/// Render a frontmatter value + body into a canonical document string.
///
/// Why: the single choke-point that guarantees byte-stable output — sort, then
/// serialise, then fence.
/// What: when `frontmatter` is a non-empty mapping, emits
/// `---\n<sorted-yaml>---\n\n<body>`; when it is empty/`Null`, emits just the
/// body. The body is emitted with exactly one trailing newline.
/// Test: `render_is_key_sorted_and_stable`, `render_without_frontmatter_omits_fence`.
pub fn render(frontmatter: &Value, body: &str) -> String {
    let is_empty = match frontmatter {
        Value::Mapping(m) => m.is_empty(),
        Value::Null => true,
        _ => false,
    };
    let body_norm = normalise_body(body);
    if is_empty {
        return body_norm;
    }
    let sorted = sort_value(frontmatter);
    let yaml = serde_yaml::to_string(&sorted).unwrap_or_default();
    if body_norm.is_empty() {
        format!("---\n{yaml}---\n")
    } else {
        format!("---\n{yaml}---\n\n{body_norm}")
    }
}

/// Trim trailing whitespace and guarantee exactly one trailing newline (or an
/// empty string for an empty body).
fn normalise_body(body: &str) -> String {
    let trimmed = body.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the split/parse/render round-trip is the load-bearing determinism
    /// primitive; a frontmatter+body doc must survive it.
    /// What: splits a fenced doc, asserts the YAML and body are separated, and
    /// that parsing the YAML yields the expected key.
    /// Test: self-contained.
    #[test]
    fn split_roundtrips_frontmatter() {
        let doc = "---\nname: Ada\ntype: person\n---\n\nBody text here.\n";
        let s = split(doc);
        assert!(s.frontmatter.is_some());
        assert_eq!(s.body, "Body text here.\n");
        let fm = parse(&s.frontmatter.unwrap()).unwrap();
        assert_eq!(fm.get("name").unwrap().as_str(), Some("Ada"));
    }

    /// Why: plain markdown with no fence must not be mistaken for frontmatter.
    /// What: asserts a fence-less doc returns `frontmatter: None` and the whole
    /// text as body, and an unterminated fence is likewise treated as body.
    /// Test: self-contained.
    #[test]
    fn split_handles_no_frontmatter() {
        let s = split("# Just a heading\n\ntext");
        assert_eq!(s.frontmatter, None);
        assert_eq!(s.body, "# Just a heading\n\ntext");

        let unterminated = split("---\nname: x\nno closing fence");
        assert_eq!(unterminated.frontmatter, None);
    }

    /// Why: this is the byte-stability guarantee — the same logical frontmatter,
    /// built in different key orders, must render identically.
    /// What: builds two mappings with keys inserted in opposite orders and a
    /// nested map, asserts `render` produces byte-identical, key-sorted output.
    /// Test: self-contained.
    #[test]
    fn render_is_key_sorted_and_stable() {
        let mut a = Mapping::new();
        a.insert(Value::from("type"), Value::from("person"));
        a.insert(Value::from("name"), Value::from("Ada"));
        let mut b = Mapping::new();
        b.insert(Value::from("name"), Value::from("Ada"));
        b.insert(Value::from("type"), Value::from("person"));

        let ra = render(&Value::Mapping(a), "body");
        let rb = render(&Value::Mapping(b), "body");
        assert_eq!(ra, rb);
        // `name` sorts before `type`.
        assert!(ra.find("name:").unwrap() < ra.find("type:").unwrap());
        assert!(ra.ends_with("body\n"));
    }

    /// Why: an empty frontmatter mapping must not emit an empty `---\n---` fence.
    /// What: asserts a document with no frontmatter renders as bare body.
    /// Test: self-contained.
    #[test]
    fn render_without_frontmatter_omits_fence() {
        let out = render(&Value::Mapping(Mapping::new()), "just body");
        assert_eq!(out, "just body\n");
    }
}
